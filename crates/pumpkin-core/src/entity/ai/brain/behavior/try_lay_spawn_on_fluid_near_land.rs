use std::sync::atomic::Ordering;

use pumpkin_data::Block;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::world::World;
use pumpkin_data::game_event::GameEvent;

const TOP_FACE_EPSILON: f64 = 1.0E-7;

// brain: state.getCollisionShape(level, pos).getFaceShape(Direction.UP).isEmpty()
fn has_no_top_face(world: &World, pos: &BlockPos) -> bool {
    let top = f64::from(pos.0.y) + 1.0;
    !world
        .get_block_state(pos)
        .get_block_collision_shapes_at(pos)
        .any(|shape| {
            shape.max.y >= top - TOP_FACE_EPSILON
                && shape.max.x > shape.min.x
                && shape.max.z > shape.min.z
        })
}

// brain: TryLaySpawnOnFluidNearLand.create(spawnBlock)
#[must_use]
pub fn create(spawn_block: &'static Block) -> OneShot {
    OneShot::new(
        "TryLaySpawnOnFluidNearLand",
        vec![
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValuePresent),
            (types::IS_PREGNANT.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let entity = tick.mob.get_entity();
            if entity.is_in_water() || !entity.on_ground.load(Ordering::Relaxed) {
                return false;
            }
            let below_pos = entity.block_pos.load().down();
            for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                let relative = BlockPos::new(below_pos.0.x + dx, below_pos.0.y, below_pos.0.z + dz);
                if !has_no_top_face(tick.world, &relative) {
                    continue;
                }
                if !(tick
                    .world
                    .get_fluid(&relative)
                    .has_tag(&tag::Fluid::MINECRAFT_SUPPORTS_FROGSPAWN)
                    || tick
                        .world
                        .get_block(&relative)
                        .has_tag(&tag::Block::MINECRAFT_SUPPORTS_FROGSPAWN))
                {
                    continue;
                }
                let spawn_pos = relative.up();
                if tick.world.get_block_state(&spawn_pos).is_air() {
                    tick.world.set_block_state(
                        &spawn_pos,
                        spawn_block.default_state.id,
                        BlockFlags::NOTIFY_ALL,
                    );
                    tick.world
                        .emit_game_event(GameEvent::BlockPlace.name(), spawn_pos.to_centered_f64());
                    tick.world.play_sound_fine(
                        Sound::EntityFrogLaySpawn,
                        SoundCategory::Blocks,
                        &entity.pos.load(),
                        1.0,
                        1.0,
                    );
                    tick.state.erase(types::IS_PREGNANT.id());
                    return true;
                }
            }
            true
        },
    )
}
