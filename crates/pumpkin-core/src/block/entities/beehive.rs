use super::BlockEntity;
use crate::block::blocks::beehive::is_smokey_pos;
use crate::entity::{
    EntityBase, passive::bee::BeeEntity, player::Player, r#type::load_entity_from_nbt,
};
use crate::world::World;
use pumpkin_data::{
    Block, BlockDirection, BlockState, HorizontalFacingExt,
    block_properties::BeeNestLikeProperties,
    sound::{Sound, SoundCategory},
    tag::{self, Taggable},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::chunk::io::Dirtiable;
use pumpkin_world::world::BlockFlags;
use rand::RngExt;
use std::sync::{Arc, Mutex, PoisonError};

// beehive: MAX_OCCUPANTS, MIN_OCCUPATION_TICKS_* (BeehiveBlockEntity)
pub const MAX_OCCUPANTS: usize = 3;
const MIN_OCCUPATION_TICKS_NECTAR: i32 = 2400;
const MIN_OCCUPATION_TICKS_NECTARLESS: i32 = 600;
const MIN_TICKS_BEFORE_REENTERING_HIVE: i32 = 400;

// beehive: IGNORED_BEE_TAGS (BeehiveBlockEntity)
const IGNORED_BEE_TAGS: &[&str] = &[
    "Air",
    "drop_chances",
    "equipment",
    "Brain",
    "CanPickUpLoot",
    "DeathTime",
    "fall_distance",
    "FallFlying",
    "Fire",
    "HurtTime",
    "LeftHanded",
    "Motion",
    "NoGravity",
    "OnGround",
    "PortalCooldown",
    "Pos",
    "Rotation",
    "sleeping_pos",
    "CannotEnterHiveTicks",
    "TicksSincePollination",
    "CropsGrownSincePollination",
    "hive_pos",
    "Passengers",
    "leash",
    "UUID",
];

/// Vanilla `BeehiveBlockEntity.BeeReleaseStatus`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BeeReleaseStatus {
    HoneyDelivered,
    BeeReleased,
    Emergency,
}

/// Vanilla `BeehiveBlockEntity.BeeData` (an occupant plus its time in the hive).
#[derive(Clone, Debug)]
pub struct BeeData {
    pub entity_data: NbtCompound,
    pub ticks_in_hive: i32,
    pub min_ticks_in_hive: i32,
}

impl BeeData {
    /// Vanilla `Occupant.of`: the saved bee without the per-world tags.
    #[must_use]
    pub fn of(bee: &BeeEntity) -> Self {
        let mut data = NbtCompound::new();
        bee.write_nbt(&mut data);
        data.put_string("id", "minecraft:bee".to_string());
        for tag in IGNORED_BEE_TAGS {
            data.child_tags.remove(*tag);
        }
        let has_nectar = data.get_bool("HasNectar").unwrap_or(false);
        Self {
            entity_data: data,
            ticks_in_hive: 0,
            min_ticks_in_hive: if has_nectar {
                MIN_OCCUPATION_TICKS_NECTAR
            } else {
                MIN_OCCUPATION_TICKS_NECTARLESS
            },
        }
    }

    #[must_use]
    pub fn from_tag(tag: &NbtTag) -> Option<Self> {
        let compound = tag.extract_compound()?;
        Some(Self {
            entity_data: compound.get_compound("entity_data")?.clone(),
            ticks_in_hive: compound.get_int("ticks_in_hive").unwrap_or(0),
            min_ticks_in_hive: compound.get_int("min_ticks_in_hive").unwrap_or(0),
        })
    }

    #[must_use]
    pub fn to_tag(&self) -> NbtTag {
        let mut compound = NbtCompound::new();
        compound.put_compound("entity_data", self.entity_data.clone());
        compound.put_int("ticks_in_hive", self.ticks_in_hive);
        compound.put_int("min_ticks_in_hive", self.min_ticks_in_hive);
        NbtTag::Compound(compound)
    }

    fn has_nectar(&self) -> bool {
        self.entity_data.get_bool("HasNectar").unwrap_or(false)
    }
}

pub struct BeehiveBlockEntity {
    pub position: BlockPos,
    pub bees: Mutex<Vec<BeeData>>,
    pub flower_pos: Mutex<Option<BlockPos>>,
}

impl BeehiveBlockEntity {
    pub const ID: &'static str = "minecraft:beehive";

    fn mark_chunk_dirty(&self, world: &Arc<World>) {
        let chunk_position = self.position.chunk_position();
        let _ = world.level.read_chunk_sync(&chunk_position, |chunk| {
            chunk.mark_dirty(true);
        });
    }

    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            bees: Mutex::new(Vec::new()),
            flower_pos: Mutex::new(None),
        }
    }

    fn bees(&self) -> std::sync::MutexGuard<'_, Vec<BeeData>> {
        self.bees.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bees().is_empty()
    }

    #[must_use]
    pub fn is_full(&self) -> bool {
        self.bees().len() >= MAX_OCCUPANTS
    }

    #[must_use]
    pub fn occupant_count(&self) -> usize {
        self.bees().len()
    }

    #[must_use]
    pub fn saved_flower_pos(&self) -> Option<BlockPos> {
        *self
            .flower_pos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Vanilla `isFireNearby`: a `FireBlock` within one block.
    #[must_use]
    pub fn is_fire_nearby(&self, world: &World) -> bool {
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = self.position.offset(Vector3::new(x, y, z));
                    if world.get_block(&pos) == &Block::FIRE {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Vanilla `isSedated`.
    #[must_use]
    pub fn is_sedated(&self, world: &World) -> bool {
        is_smokey_pos(world, &self.position)
    }

    /// Vanilla `addOccupant`.
    pub fn add_occupant(&self, world: &Arc<World>, bee: &BeeEntity) {
        let mut bees = self.bees();
        if bees.len() >= MAX_OCCUPANTS {
            return;
        }
        bees.push(BeeData::of(bee));
        drop(bees);
        // beehive: the bee passes its flower on, or keeps the hive's with a coin flip
        if let Some(bee_flower) = bee.saved_flower_pos() {
            let mut flower = self
                .flower_pos
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if flower.is_none() || rand::random::<bool>() {
                *flower = Some(bee_flower);
            }
        }
        let center = self.position.to_centered_f64();
        world.play_sound_fine(
            Sound::BlockBeehiveEnter,
            SoundCategory::Blocks,
            &center,
            1.0,
            1.0,
        );
        bee.get_entity().remove();
        self.mark_chunk_dirty(world);
    }

    /// Vanilla `emptyAllLivingFromHive`.
    pub fn empty_all_living_from_hive(
        &self,
        world: &Arc<World>,
        player: Option<&Arc<Player>>,
        state: &'static BlockState,
        status: BeeReleaseStatus,
    ) {
        let released = self.release_all_occupants(world, state, status);
        let Some(player) = player else { return };
        let player_pos = player.get_entity().pos.load();
        for entity in released {
            let Some(bee) = entity
                .get_mob()
                .and_then(|mob| mob.cast_any().downcast_ref::<BeeEntity>())
            else {
                continue;
            };
            if player_pos.squared_distance_to_vec(&entity.get_entity().pos.load()) <= 16.0 {
                if self.is_sedated(world) {
                    bee.set_stay_out_of_hive_countdown(400);
                } else {
                    bee.mob_entity.set_target(Some(player.clone()));
                }
            }
        }
    }

    fn release_all_occupants(
        &self,
        world: &Arc<World>,
        state: &'static BlockState,
        status: BeeReleaseStatus,
    ) -> Vec<Arc<dyn EntityBase>> {
        let stored: Vec<BeeData> = std::mem::take(&mut *self.bees());
        let flower = self.saved_flower_pos();
        let mut spawned = Vec::new();
        let mut kept = Vec::new();
        for data in stored {
            match release_occupant(world, &self.position, state, &data, status, flower) {
                Some(entity) => spawned.push(entity),
                None => kept.push(data),
            }
        }
        if !kept.is_empty() {
            self.bees().extend(kept);
        }
        if !spawned.is_empty() {
            self.mark_chunk_dirty(world);
        }
        spawned
    }
}

/// Vanilla `BeehiveBlockEntity.releaseOccupant`.
fn release_occupant(
    world: &Arc<World>,
    pos: &BlockPos,
    state: &'static BlockState,
    data: &BeeData,
    status: BeeReleaseStatus,
    saved_flower: Option<BlockPos>,
) -> Option<Arc<dyn EntityBase>> {
    if world.bees_stay_in_hive(pos) && status != BeeReleaseStatus::Emergency {
        return None;
    }
    let props = BeeNestLikeProperties::from_state_id(state.id);
    let facing: BlockDirection = props.facing.to_block_direction();
    let front = pos.offset(facing.to_offset());
    let front_blocked = world
        .get_block_state(&front)
        .get_block_collision_shapes_at(&front)
        .next()
        .is_some();
    if front_blocked && status != BeeReleaseStatus::Emergency {
        return None;
    }

    let mut nbt = data.entity_data.clone();
    for tag in IGNORED_BEE_TAGS {
        nbt.child_tags.remove(*tag);
    }
    let center = pos.to_centered_f64();
    let entity = load_entity_from_nbt(&nbt, center, world)?;
    let bee = entity
        .get_mob()
        .and_then(|mob| mob.cast_any().downcast_ref::<BeeEntity>())?;

    // beehive: BEEHIVE_INHABITORS, createEntity
    bee.get_entity().set_has_no_gravity(true);
    bee.set_hive_pos(*pos);
    bee.apply_release_data(data.ticks_in_hive);

    let mut rng = rand::rng();
    if let Some(flower) = saved_flower
        && !bee.has_saved_flower_pos()
        && rng.random::<f32>() < 0.9
    {
        bee.set_saved_flower_pos(flower);
    }
    if status == BeeReleaseStatus::HoneyDelivered {
        bee.drop_off_nectar();
        if Block::from_state_id(state.id).has_tag(&tag::Block::MINECRAFT_BEEHIVES) {
            let honey = i32::from(props.honey_level);
            if honey < 5 {
                let mut increase = if rng.random_range(0..100) == 0 { 2 } else { 1 };
                if honey + increase > 5 {
                    increase -= 1;
                }
                let mut next = props;
                next.honey_level = (honey + increase) as u8;
                world.set_block_state(
                    pos,
                    next.to_state_id(Block::from_state_id(state.id)),
                    BlockFlags::NOTIFY_ALL,
                );
            }
        }
    }

    let width = f64::from(entity.get_entity().entity_type.dimension[0]);
    let height = f64::from(entity.get_entity().entity_type.dimension[1]);
    let delta = if front_blocked {
        0.0
    } else {
        0.55 + width / 2.0
    };
    let offset = facing.to_offset();
    entity.get_entity().set_pos(Vector3::new(
        f64::from(pos.0.x) + 0.5 + delta * f64::from(offset.x),
        f64::from(pos.0.y) + 0.5 - height / 2.0,
        f64::from(pos.0.z) + 0.5 + delta * f64::from(offset.z),
    ));
    world.play_sound_fine(
        Sound::BlockBeehiveExit,
        SoundCategory::Blocks,
        &center,
        1.0,
        1.0,
    );
    world.spawn_entity(entity.clone()).then_some(entity)
}

impl BlockEntity for BeehiveBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let bees = nbt
            .get_list("bees")
            .or_else(|| nbt.get_list("Bees"))
            .map(|list| list.iter().filter_map(BeeData::from_tag).collect())
            .unwrap_or_default();
        let flower_pos = nbt
            .get_int_array("flower_pos")
            .and_then(|arr| (arr.len() == 3).then(|| BlockPos::new(arr[0], arr[1], arr[2])));
        Self {
            position,
            bees: Mutex::new(bees),
            flower_pos: Mutex::new(flower_pos),
        }
    }

    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_list("bees", self.bees().iter().map(BeeData::to_tag).collect());
        if let Some(fp) = self.saved_flower_pos() {
            nbt.put("flower_pos", NbtTag::IntArray(vec![fp.0.x, fp.0.y, fp.0.z]));
        }
    }

    fn chunk_data_nbt(&self) -> Option<NbtCompound> {
        let mut nbt = NbtCompound::new();
        self.write_nbt(&mut nbt);
        Some(nbt)
    }

    // beehive: serverTick (tickOccupants + BEEHIVE_WORK sound)
    fn tick(&self, world: &Arc<World>) {
        let state_id = world.get_block_state_id(&self.position);
        let state = pumpkin_data::BlockState::from_id(state_id);
        let flower = self.saved_flower_pos();
        let mut due = Vec::new();
        {
            if self.is_empty() {
                return;
            }
            // beehive: fire nearby forces the bees out (BeehiveBlockEntity.setChanged)
            if self.position.0.x.wrapping_add(self.position.0.z) as i64 % 10
                == world.get_world_age() % 10
                && self.is_fire_nearby(world)
            {
                self.release_all_occupants(world, state, BeeReleaseStatus::Emergency);
                return;
            }
            let mut bees = self.bees();
            let mut i = 0;
            while i < bees.len() {
                let bee = &mut bees[i];
                let ready = bee.ticks_in_hive > bee.min_ticks_in_hive;
                bee.ticks_in_hive += 1;
                if ready {
                    due.push(bees.remove(i));
                } else {
                    i += 1;
                }
            }
        }
        let mut changed = false;
        let mut kept = Vec::new();
        for data in due {
            let status = if data.has_nectar() {
                BeeReleaseStatus::HoneyDelivered
            } else {
                BeeReleaseStatus::BeeReleased
            };
            if release_occupant(world, &self.position, state, &data, status, flower).is_some() {
                changed = true;
            } else {
                kept.push(data);
            }
        }
        if !kept.is_empty() {
            self.bees().extend(kept);
        }
        if changed {
            self.mark_chunk_dirty(world);
        }
        if rand::random::<f64>() < 0.005 {
            let pos = Vector3::new(
                f64::from(self.position.0.x) + 0.5,
                f64::from(self.position.0.y),
                f64::from(self.position.0.z) + 0.5,
            );
            world.play_sound_fine(
                Sound::BlockBeehiveWork,
                SoundCategory::Blocks,
                &pos,
                1.0,
                1.0,
            );
        }
    }

    fn collect_item_components(&self, stack: &mut pumpkin_data::item_stack::ItemStack) {
        let bees = self.bees();
        if !bees.is_empty() {
            stack.set_data_component(pumpkin_data::data_component_impl::BeesImpl {
                bees: bees.iter().map(BeeData::to_tag).collect(),
            });
        }
    }

    fn apply_item_components(&self, stack: &pumpkin_data::item_stack::ItemStack) {
        if let Some(bees) =
            stack.get_data_component::<pumpkin_data::data_component_impl::BeesImpl>()
            && !bees.bees.is_empty()
        {
            *self.bees() = bees.bees.iter().filter_map(BeeData::from_tag).collect();
        }
    }

    // beehive: creative break still drops the hive with its bees (BeehiveBlock.playerWillDestroy)
    fn drops_for_creative_player(&self) -> bool {
        !self.is_empty()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Bees that just left the hive wait this long before re-entering (`MIN_TICKS_BEFORE_REENTERING_HIVE`).
#[must_use]
pub const fn reenter_delay() -> i32 {
    MIN_TICKS_BEFORE_REENTERING_HIVE
}
