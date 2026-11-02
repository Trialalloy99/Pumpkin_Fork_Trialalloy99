use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::random::{RandomGenerator, get_seed, xoroshiro128::Xoroshiro};
use pumpkin_world::generation::feature::features::sculk::{
    SculkLevel,
    spreader::{ChargeCursor, SculkSpreader},
};
use std::sync::{Arc, Mutex};

use crate::world::World;

pub struct SculkCatalystBlockEntity {
    pub position: BlockPos,
    pub decay_delay: Mutex<i32>,
    // catalyst: SculkCatalystBlockEntity.CatalystListener.sculkSpreader
    pub spreader: Mutex<SculkSpreader>,
}

// sculk: spreader view over the loaded level (SculkSpreader.updateCursors on a ServerLevel)
pub struct WorldSculkView<'a>(pub &'a Arc<World>);

impl SculkLevel for WorldSculkView<'_> {
    fn sculk_get(&self, pos: BlockPos) -> Option<pumpkin_data::BlockStateId> {
        self.0
            .level
            .is_chunk_loaded(&pos.chunk_position())
            .then(|| self.0.get_block_state_id(&pos))
    }

    fn sculk_set(&mut self, pos: BlockPos, state: &'static pumpkin_data::BlockState) {
        if self.0.level.is_chunk_loaded(&pos.chunk_position()) {
            self.0
                .set_block_state(&pos, state.id, pumpkin_world::world::BlockFlags::NOTIFY_ALL);
        }
    }

    fn sculk_is_air(&self, pos: BlockPos) -> bool {
        self.0.get_block_state(&pos).is_air()
    }

    fn sculk_is_water_source(&self, pos: BlockPos) -> bool {
        let state = self.0.get_block_state(&pos);
        World::get_fluid_from_state_id(state.id) == &pumpkin_data::fluid::Fluid::WATER
            && state.id == pumpkin_data::Block::WATER.default_state.id
    }

    fn sculk_is_water(&self, pos: BlockPos) -> bool {
        let state = self.0.get_block_state(&pos);
        World::get_fluid_from_state_id(state.id) == &pumpkin_data::fluid::Fluid::WATER
    }

    fn sculk_is_face_sturdy(&self, pos: BlockPos, face: pumpkin_data::BlockDirection) -> bool {
        self.0.get_block_state(&pos).is_side_solid(face)
    }

    fn sculk_is_full_cube(&self, pos: BlockPos) -> bool {
        self.0.get_block_state(&pos).is_full_cube()
    }
}

impl BlockEntity for SculkCatalystBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn from_nbt(nbt: &pumpkin_nbt::compound::NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let entity = Self::new(position);
        *entity
            .decay_delay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            nbt.get_int("decay_delay").unwrap_or(0);
        if let Some(cursors) = nbt.get_list("cursors") {
            let mut spreader = entity
                .spreader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for cursor in cursors {
                if let NbtTag::Compound(cursor) = cursor
                    && let Some(pos) = cursor.get_int_array("pos")
                    && pos.len() == 3
                {
                    let mut loaded = ChargeCursor::new(
                        BlockPos::new(pos[0], pos[1], pos[2]),
                        cursor.get_int("charge").unwrap_or(0).clamp(0, 1000) as u16,
                    );
                    loaded.decay_delay =
                        cursor.get_int("decay_delay").unwrap_or(1).clamp(0, 255) as u8;
                    loaded.update_delay =
                        cursor.get_int("update_delay").unwrap_or(0).clamp(0, 255) as u8;
                    spreader.restore_cursor(loaded);
                }
            }
        }
        entity
    }

    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int(
            "decay_delay",
            *self
                .decay_delay
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let spreader = self
            .spreader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursors: Vec<NbtTag> = spreader
            .cursor_list()
            .iter()
            .map(|cursor| {
                let mut tag = NbtCompound::new();
                tag.put(
                    "pos",
                    NbtTag::IntArray(vec![cursor.pos.0.x, cursor.pos.0.y, cursor.pos.0.z]),
                );
                tag.put_int("charge", i32::from(cursor.charge));
                tag.put_int("decay_delay", i32::from(cursor.decay_delay));
                tag.put_int("update_delay", i32::from(cursor.update_delay));
                NbtTag::Compound(tag)
            })
            .collect();
        nbt.put_list("cursors", cursors);
    }

    fn chunk_data_nbt(&self) -> Option<NbtCompound> {
        Some(NbtCompound::new())
    }

    // cursors move every tick, so a busy catalyst is always saved
    fn is_dirty(&self) -> bool {
        !self
            .spreader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_idle()
    }

    // catalyst: SculkCatalystBlockEntity.serverTick
    fn tick(&self, world: &Arc<World>) {
        let mut spreader = self
            .spreader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if spreader.is_idle() {
            return;
        }
        let mut random = RandomGenerator::Xoroshiro(Xoroshiro::from_seed(get_seed()));
        spreader.update_cursors(&mut WorldSculkView(world), self.position, &mut random, true);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl SculkCatalystBlockEntity {
    pub const ID: &'static str = "minecraft:sculk_catalyst";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            decay_delay: Mutex::new(0),
            spreader: Mutex::new(SculkSpreader::new_level_spreader()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SculkSpreader.save/load keeps the cursors of a busy catalyst across restarts
    #[test]
    fn cursors_survive_a_save_and_load() {
        let catalyst = SculkCatalystBlockEntity::new(BlockPos::new(1, 2, 3));
        catalyst
            .spreader
            .lock()
            .unwrap()
            .add_cursors(BlockPos::new(4, 5, 6), 30);
        let mut nbt = NbtCompound::new();
        catalyst.write_nbt(&mut nbt);
        let loaded = SculkCatalystBlockEntity::from_nbt(&nbt, BlockPos::new(1, 2, 3));
        let spreader = loaded.spreader.lock().unwrap();
        let cursors = spreader.cursor_list();
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].pos, BlockPos::new(4, 5, 6));
        assert_eq!(cursors[0].charge, 30);
    }
}
