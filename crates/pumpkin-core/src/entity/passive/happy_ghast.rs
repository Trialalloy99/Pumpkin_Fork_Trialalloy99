use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::CEntityPositionSync;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::math::wrap_degrees;
use pumpkin_world::chunk::ChunkHeightmapType::MotionBlocking;
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::{
        brain::{Brain, memory::PackedMemories},
        control::{
            Control, LookControlTrait, flying_move_control::FlyingMoveControl,
            ghast_move_control::GhastMoveControl, look_control::LookControl,
        },
        goal::{Controls, Goal, swim::SwimGoal, tempt::TemptGoal},
        pathfinder::Navigator,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, ghast::face_movement_direction},
    passive::animal::Animal,
    player::Player,
};

mod ai;

// ghast: Direction.values() offsets
const DIRECTIONS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

const BABY_SCALE: f32 = 0.2375;
const BABY_EYE_HEIGHT: f32 = 0.46875;
const SMALL_RESTRICTION_RADIUS: i32 = 32;
const LARGE_RESTRICTION_RADIUS: i32 = 64;
const RESTRICTION_RADIUS_BUFFER: i32 = 16;
const FAST_HEALING_TICKS: i32 = 20;
const SLOW_HEALING_TICKS: i32 = 600;
const MAX_PASSENGERS: usize = 4;
const STILL_TIMEOUT_ON_LOAD_GRACE_PERIOD: i32 = 60;
const MAX_STILL_TIMEOUT: i32 = 10;
const WANDER_GROUND_DISTANCE: i32 = 16;
const MAX_FLY_TO_ATTEMPTS: i32 = 64;
const FLY_TO_RANGE: f64 = 16.0;
const LEASH_HOLDER_TICKS: i32 = 5;
const RIDDEN_TURN_SPEED: f32 = 0.08;

/// Represents a Happy Ghast, a passive flying mob.
///
/// Wiki: <https://minecraft.wiki/w/Happy_Ghast>
pub struct HappyGhastEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub server_still_timeout: AtomicI32,
    pub leash_holder_time: AtomicI32,
    pub is_leash_holder: AtomicBool,
    pub stays_still: AtomicBool,
    last_passenger_count: AtomicUsize,
}

// happy ghast: Mob.stopInPlace check for the move control
fn still_timeout(mob: &dyn Mob) -> bool {
    mob.cast_any()
        .downcast_ref::<HappyGhastEntity>()
        .is_some_and(HappyGhastEntity::is_on_still_timeout)
}

impl HappyGhastEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let happy_ghast = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            server_still_timeout: AtomicI32::new(0),
            leash_holder_time: AtomicI32::new(0),
            is_leash_holder: AtomicBool::new(false),
            stays_still: AtomicBool::new(false),
            last_passenger_count: AtomicUsize::new(0),
        };
        let mob_arc = Arc::new(happy_ghast);
        mob_arc.adult_ghast_setup();
        mob_arc
    }

    // happy ghast: HappyGhast.adultGhastSetup, move/look control and goals
    fn adult_ghast_setup(&self) {
        let mob = &self.mob_entity;
        *mob.move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(GhastMoveControl::new(true, still_timeout));
        *mob.look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(HappyGhastLookControl::default());
        *mob.navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Navigator::default();
        // happy ghast: registerGoals
        mob.goals_selector.clear(self);
        mob.goals_selector
            .add_goal(3, Box::new(HappyGhastFloatGoal::default()));
        mob.goals_selector.add_goal(
            4,
            Box::new(
                TemptGoal::with_stop_distance(1.0, &[], false, 7.0)
                    .for_non_pathfinders(is_tempt_item),
            ),
        );
        mob.goals_selector.add_goal(
            5,
            Box::new(RandomFloatAroundGoal::new(WANDER_GROUND_DISTANCE)),
        );
        self.mob_entity.clear_brain_memories();
    }

    // happy ghast: HappyGhast.babyGhastSetup
    fn baby_ghast_setup(&self) {
        let mob = &self.mob_entity;
        *mob.move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(FlyingMoveControl::new(180, true));
        *mob.look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Box::<LookControl>::default();
        let mut navigator = mob
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *navigator = Navigator::flying();
        navigator.set_can_open_doors(false);
        navigator.set_can_float(true);
        navigator.set_required_path_length(48.0);
        drop(navigator);
        self.set_server_still_timeout(0);
        mob.goals_selector.clear(self);
    }

    // happy ghast: setServerStillTimeout, position sync when it starts
    pub fn set_server_still_timeout(&self, timeout: i32) {
        let previous = self.server_still_timeout.swap(timeout, Ordering::Relaxed);
        if previous <= 0 && timeout > 0 {
            let entity = self.get_entity();
            let pos = entity.pos.load();
            entity.last_sent_pos.store(pos);
            entity.world.load().broadcast_to_chunk(
                entity.chunk_pos.load(),
                &CEntityPositionSync::new(
                    entity.entity_id.into(),
                    pos,
                    entity.velocity.load(),
                    entity.yaw.load(),
                    entity.pitch.load(),
                    entity.on_ground.load(Ordering::Relaxed),
                ),
            );
        }
        self.sync_stay_still_flag();
    }

    #[must_use]
    pub fn is_on_still_timeout(&self) -> bool {
        self.stays_still.load(Ordering::Relaxed)
            || self.server_still_timeout.load(Ordering::Relaxed) > 0
    }

    // happy ghast: notifyLeashHolder
    pub fn notify_leash_holder(&self) {
        self.leash_holder_time
            .store(LEASH_HOLDER_TICKS, Ordering::Relaxed);
    }

    fn set_leash_holder(&self, is_leash_holder: bool) {
        self.is_leash_holder
            .store(is_leash_holder, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::happy_ghast::IS_LEASH_HOLDER,
            is_leash_holder,
        );
    }

    fn sync_stay_still_flag(&self) {
        let stays_still = self.server_still_timeout.load(Ordering::Relaxed) > 0;
        self.stays_still.store(stays_still, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::happy_ghast::STAYS_STILL,
            stays_still,
        );
    }

    fn body_armor(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&pumpkin_data::data_component_impl::EquipmentSlot::BODY)
    }

    // happy ghast: isWearingBodyArmor
    #[must_use]
    pub fn is_wearing_body_armor(&self) -> bool {
        !self.body_armor().is_empty()
    }

    // happy ghast: scanPlayerAboveGhast
    fn scan_player_above_ghast(&self) -> bool {
        let entity = self.get_entity();
        let bb = entity.bounding_box.load();
        let (min_x, max_x) = (bb.min.x - 1.0, bb.max.x + 1.0);
        let (min_z, max_z) = (bb.min.z - 1.0, bb.max.z + 1.0);
        let min_y = bb.max.y - f64::from(1.0E-5f32);
        let max_y = bb.max.y + (bb.max.y - bb.min.y) / 2.0;
        let world = entity.world.load();
        world.players.load().iter().any(|player| {
            if player.is_spectator() {
                return false;
            }
            let root = player.get_entity().get_root_vehicle();
            let root = root
                .as_deref()
                .unwrap_or(player.as_ref() as &dyn EntityBase);
            if root.get_entity().entity_type == &EntityType::HAPPY_GHAST {
                return false;
            }
            let pos = root.get_entity().pos.load();
            pos.x >= min_x
                && pos.x <= max_x
                && pos.y >= min_y
                && pos.y <= max_y
                && pos.z >= min_z
                && pos.z <= max_z
        })
    }

    // happy ghast: getHappyGhastRestrictionRadius
    fn restriction_radius(&self) -> i32 {
        if !self.is_baby() && !self.is_wearing_body_armor() {
            LARGE_RESTRICTION_RADIUS
        } else {
            SMALL_RESTRICTION_RADIUS
        }
    }

    // happy ghast: checkRestriction
    fn check_restriction(&self) {
        let entity = self.get_entity();
        if entity.is_leashed() || entity.has_passengers() {
            return;
        }
        let mob = &self.mob_entity;
        let radius = self.restriction_radius();
        let pos = entity.block_pos.load();
        let reach = radius + RESTRICTION_RADIUS_BUFFER;
        if !mob.has_position_target()
            || mob.position_target.load().squared_distance(&pos) >= reach * reach
            || radius != mob.position_target_range.load(Ordering::Relaxed)
        {
            mob.position_target.store(pos);
            mob.position_target_range.store(radius, Ordering::Relaxed);
        }
    }

    // happy ghast: continuousHeal
    fn continuous_heal(&self) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        if !entity.is_alive()
            || living.is_dead_or_dying()
            || (living.get_max_health() - living.health.load()).abs() < f32::EPSILON
        {
            return;
        }
        let world = entity.world.load();
        let fast = self.is_in_clouds() || world.is_raining_at(&entity.block_pos.load());
        let interval = if fast {
            FAST_HEALING_TICKS
        } else {
            SLOW_HEALING_TICKS
        };
        if entity.tick_count.load(Ordering::Relaxed) % interval == 0 {
            living.heal(1.0);
        }
    }

    // happy ghast: Entity.isInClouds
    fn is_in_clouds(&self) -> bool {
        let entity = self.get_entity();
        let Some(cloud_bottom) = entity.world.load().dimension.cloud_height else {
            return false;
        };
        let y = entity.pos.load().y;
        let height = f64::from(entity.height());
        y + height >= f64::from(cloud_bottom) && y <= f64::from(cloud_bottom + 4.0)
    }

    // happy ghast: addPassenger/removePassenger side effects, seen as a passenger count change
    fn tick_passengers(&self) {
        let entity = self.get_entity();
        let count = entity
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        let previous = self.last_passenger_count.swap(count, Ordering::Relaxed);
        if count == previous {
            return;
        }
        let world = entity.world.load();
        let pos = entity.pos.load();
        if count > previous {
            if previous == 0 {
                world.play_sound(
                    Sound::EntityHappyGhastHarnessGogglesDown,
                    SoundCategory::Neutral,
                    &pos,
                );
            }
            if !self.scan_player_above_ghast() {
                self.set_server_still_timeout(0);
            } else if self.server_still_timeout.load(Ordering::Relaxed) > MAX_STILL_TIMEOUT {
                self.set_server_still_timeout(MAX_STILL_TIMEOUT);
            }
        } else {
            self.set_server_still_timeout(MAX_STILL_TIMEOUT);
            if count == 0 {
                self.mob_entity
                    .position_target_range
                    .store(-1, Ordering::Relaxed);
                world.play_sound(
                    Sound::EntityHappyGhastHarnessGogglesUp,
                    SoundCategory::Neutral,
                    &pos,
                );
            }
        }
    }

    // happy ghast: Equippable.interactLivingEntity, harness equip and shears
    fn interact_harness(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let entity = self.get_entity();
        if !entity.is_alive() {
            return false;
        }
        let world = entity.world.load();
        let pos = entity.pos.load();
        if item_stack.item.has_tag(&tag::Item::MINECRAFT_HARNESSES) && !self.is_wearing_body_armor()
        {
            let mut worn = item_stack.clone();
            worn.item_count = 1;
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &pumpkin_data::data_component_impl::EquipmentSlot::BODY,
                worn,
            );
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            world.play_sound(Sound::EntityHappyGhastEquip, SoundCategory::Neutral, &pos);
            return true;
        }
        if item_stack.item == &Item::SHEARS && self.is_wearing_body_armor() {
            let worn = self.mob_entity.set_item_slot(
                &pumpkin_data::data_component_impl::EquipmentSlot::BODY,
                ItemStack::EMPTY.clone(),
            );
            entity.spawn_at_location(worn);
            let _ = player.damage_item_stack(item_stack, 1);
            world.play_sound(Sound::EntityHappyGhastUnequip, SoundCategory::Neutral, &pos);
            return true;
        }
        false
    }

    // happy ghast: doPlayerRide
    fn do_player_ride(&self, player: &Arc<Player>) -> bool {
        let entity = self.get_entity();
        let world = entity.world.load();
        if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
            && let Some(passenger) = world.get_player_by_id(player.entity_id())
        {
            entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
            return true;
        }
        false
    }
}

// happy ghast: tempt items by state (HappyGhast.registerGoals)
fn is_tempt_item(mob: &dyn Mob, stack: &ItemStack) -> bool {
    let Some(ghast) = mob.cast_any().downcast_ref::<HappyGhastEntity>() else {
        return false;
    };
    if !ghast.is_wearing_body_armor() && !ghast.is_baby() {
        stack
            .item
            .has_tag(&tag::Item::MINECRAFT_HAPPY_GHAST_TEMPT_ITEMS)
    } else {
        stack.item.has_tag(&tag::Item::MINECRAFT_HAPPY_GHAST_FOOD)
    }
}

// happy ghast: HappyGhastFloatGoal
#[derive(Default)]
struct HappyGhastFloatGoal(SwimGoal);

impl Goal for HappyGhastFloatGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        !still_timeout(mob) && self.0.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.0.should_continue(mob)
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.0.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.0.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        self.0.controls()
    }
}

// happy ghast: HappyGhastLookControl
#[derive(Default)]
struct HappyGhastLookControl {
    look_at_cooldown: i32,
    wanted: Vector3<f64>,
}

impl Control for HappyGhastLookControl {}

impl LookControlTrait for HappyGhastLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        if still_timeout(mob) {
            let close_angle = pumpkin_util::math::wrap_degrees_90(entity.yaw.load());
            entity.yaw.store(entity.yaw.load() - close_angle);
            entity.head_yaw.store(entity.yaw.load());
        } else if self.look_at_cooldown > 0 {
            self.look_at_cooldown -= 1;
            let pos = entity.pos.load();
            let yaw =
                (-f64::atan2(self.wanted.x - pos.x, self.wanted.z - pos.z).to_degrees()) as f32;
            entity.yaw.store(yaw);
            entity.body_yaw.store(yaw);
            entity.head_yaw.store(yaw);
        } else {
            face_movement_direction(mob);
        }
    }

    fn looking_at(&self) -> Option<Vector3<f64>> {
        (self.look_at_cooldown > 0).then_some(self.wanted)
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        _max_yaw_change: f32,
        _max_pitch_change: f32,
    ) {
        self.wanted = Vector3::new(x, y, z);
        self.look_at_cooldown = 2;
    }
}

// ghast: Ghast.RandomFloatAroundGoal
struct RandomFloatAroundGoal {
    distance_to_blocks: i32,
}

impl RandomFloatAroundGoal {
    const fn new(distance_to_blocks: i32) -> Self {
        Self { distance_to_blocks }
    }

    fn choose_random_position(center: Vector3<f64>, mob: &dyn Mob) -> Vector3<f64> {
        let mut rng = mob.get_random();
        Vector3::new(
            center.x + (f64::from(rng.random::<f32>()) * 2.0 - 1.0) * FLY_TO_RANGE,
            center.y + (f64::from(rng.random::<f32>()) * 2.0 - 1.0) * FLY_TO_RANGE,
            center.z + (f64::from(rng.random::<f32>()) * 2.0 - 1.0) * FLY_TO_RANGE,
        )
    }

    // ghast: isGoodTarget
    fn is_good_target(mob: &dyn Mob, target: Vector3<f64>, distance_to_blocks: i32) -> bool {
        if distance_to_blocks <= 0 {
            return true;
        }
        let world = mob.get_entity().world.load();
        let pos = BlockPos::floored_v(target);
        if !world.get_block_state(&pos).is_air() {
            return false;
        }
        DIRECTIONS.iter().any(|(dx, dy, dz)| {
            (1..distance_to_blocks).any(|i| {
                let offset = BlockPos::new(pos.0.x + dx * i, pos.0.y + dy * i, pos.0.z + dz * i);
                !world.get_block_state(&offset).is_air()
            })
        })
    }

    // ghast: getSuitableFlyToPosition
    fn suitable_fly_to_position(mob: &dyn Mob, distance_to_blocks: i32) -> Vector3<f64> {
        let mob_entity = mob.get_mob_entity();
        let center = mob.get_entity().pos.load();
        let mut result = None;
        for _ in 0..MAX_FLY_TO_ATTEMPTS {
            let candidate = Self::choose_random_position(center, mob);
            let candidate = (!mob_entity.has_position_target()
                || mob_entity.is_in_position_target_range_pos(&BlockPos::floored_v(candidate)))
            .then_some(candidate);
            result = candidate;
            if let Some(candidate) = candidate
                && Self::is_good_target(mob, candidate, distance_to_blocks)
            {
                return candidate;
            }
        }
        let mut result = result.unwrap_or_else(|| Self::choose_random_position(center, mob));
        let pos = BlockPos::floored_v(result);
        let world = mob.get_entity().world.load();
        let height_y = world.get_heightmap_height(MotionBlocking, pos.0.x, pos.0.z);
        if height_y < pos.0.y && height_y > world.min_y {
            result.y = center.y - (center.y - result.y).abs();
        }
        result
    }
}

impl Goal for RandomFloatAroundGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let wanted = mob
            .get_mob_entity()
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wanted_position();
        wanted.is_none_or(|wanted| {
            let dd = mob.get_entity().pos.load().squared_distance_to_vec(&wanted);
            !(1.0..=3600.0).contains(&dd)
        })
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        let target = Self::suitable_fly_to_position(mob, self.distance_to_blocks);
        mob.get_mob_entity()
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_wanted_position(target.x, target.y, target.z, 1.0);
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

impl AgeableMob for HappyGhastEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    // happy ghast: ageBoundaryReached
    fn age_boundary_reached(&self) {
        if self.is_baby() {
            self.baby_ghast_setup();
        } else {
            self.adult_ghast_setup();
        }
    }
}

impl Animal for HappyGhastEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack
            .item
            .has_tag(&tag::Item::MINECRAFT_HAPPY_GHAST_FOOD)
    }
}

impl CustomSound for HappyGhastEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityGhastlingDeath
        } else {
            Sound::EntityHappyGhastDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityGhastlingHurt
        } else {
            Sound::EntityHappyGhastHurt
        })
    }

    fn sound_volume(&self) -> f32 {
        if self.is_baby() { 1.0 } else { 4.0 }
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Neutral
    }
}

impl Mob for HappyGhastEntity {
    /// Vanilla `HappyGhast` steps silently.
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {}

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityGhastlingAmbient
        } else {
            Sound::EntityHappyGhastAmbient
        })
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        let interval = MobEntity::DEFAULT_AMBIENT_SOUND_INTERVAL;
        if self.get_entity().has_passengers() {
            interval * 6
        } else {
            interval
        }
    }

    fn baby_dimensions(&self) -> Option<EntityDimensions> {
        let adult = Entity::type_dimensions(self.get_entity().entity_type);
        Some(EntityDimensions {
            width: adult.width * BABY_SCALE,
            height: adult.height * BABY_SCALE,
            eye_height: BABY_EYE_HEIGHT,
        })
    }

    fn can_fall_in_love(&self) -> bool {
        false
    }

    fn can_dispenser_equip_into_slot(
        &self,
        slot: &pumpkin_data::data_component_impl::EquipmentSlot,
    ) -> bool {
        slot == &pumpkin_data::data_component_impl::EquipmentSlot::BODY
    }

    fn should_check_fall_damage(&self) -> bool {
        false
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int(
            "still_timeout",
            self.server_still_timeout.load(Ordering::Relaxed),
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_server_still_timeout(nbt.get_int("still_timeout").unwrap_or(0));
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        ai::make_brain(self, packed)
    }

    // happy ghast: HappyGhast.travel, flying speed * 5 / 3
    fn travel(&self, caller: &dyn EntityBase) -> bool {
        let living = &self.mob_entity.living_entity;
        let speed = living.get_attribute_value(&pumpkin_data::attributes::Attributes::FLYING_SPEED)
            * 5.0
            / 3.0;
        living.travel_flying_with(caller, speed, speed, speed);
        true
    }

    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let world = self.get_entity().world.load();
        if !world.get_block_state(pos).is_air() {
            return 0.0;
        }
        if world.get_block_state(&pos.down()).is_air()
            && !world.get_block_state(&pos.down().down()).is_air()
        {
            10.0
        } else {
            5.0
        }
    }

    fn climbable_override(&self) -> Option<bool> {
        Some(false)
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        if self.mob_entity.is_no_ai() {
            return None;
        }
        let first = self
            .get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .cloned()?;
        if self.is_wearing_body_armor()
            && !self.is_on_still_timeout()
            && first.get_player().is_some()
        {
            return Some(first);
        }
        first
            .get_mob()
            .is_some()
            .then_some(first)
            .filter(|passenger| passenger.can_control_vehicle())
    }

    // happy ghast: tickRidden, turn towards the rider
    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        let rider = controller.get_entity();
        let entity = self.get_entity();
        let target_yaw = rider.yaw.load();
        let mut yaw = entity.yaw.load();
        yaw += wrap_degrees(target_yaw - yaw) * RIDDEN_TURN_SPEED;
        entity.set_rotation(yaw, rider.pitch.load() * 0.5);
        entity.head_yaw.store(yaw);
        entity.body_yaw.store(yaw);
    }

    fn on_elastic_leash_pull(&self) {
        self.mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_wait();
    }

    // happy ghast: canBeCollidedWith
    fn mob_is_collidable(&self, other: Option<&dyn EntityBase>) -> bool {
        if self.is_baby() || !self.get_entity().is_alive() {
            return false;
        }
        if self.get_entity().has_passengers()
            && other.is_some_and(|other| other.get_entity().entity_type == &EntityType::HAPPY_GHAST)
        {
            return true;
        }
        self.is_on_still_timeout()
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let leash_time = self.leash_holder_time.load(Ordering::Relaxed);
        if leash_time > 0 {
            self.leash_holder_time.fetch_sub(1, Ordering::Relaxed);
        }
        self.set_leash_holder(leash_time > 0);

        if self.server_still_timeout.load(Ordering::Relaxed) > 0 {
            if self.get_entity().tick_count.load(Ordering::Relaxed)
                > STILL_TIMEOUT_ON_LOAD_GRACE_PERIOD
            {
                self.server_still_timeout.fetch_sub(1, Ordering::Relaxed);
            }
            self.sync_stay_still_flag();
        }
        self.tick_passengers();
        if self.scan_player_above_ghast() {
            self.set_server_still_timeout(MAX_STILL_TIMEOUT);
        }
    }

    // happy ghast: customServerAiStep, the brain only runs for babies
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        if self.is_baby() {
            self.mob_entity.tick_brain(self);
            self.mob_entity.with_brain(self, ai::update_activity);
        }
        self.check_restriction();
    }

    fn mob_ai_step(&self) {
        self.continuous_heal();
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::happy_ghast::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::happy_ghast::IS_LEASH_HOLDER,
            self.is_leash_holder.load(Ordering::Relaxed),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::happy_ghast::STAYS_STILL,
            self.stays_still.load(Ordering::Relaxed),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let ambient = self
            .get_ambient_sound()
            .unwrap_or(Sound::EntityHappyGhastAmbient);
        if self.is_baby() {
            return self.animal_interact(player, item_stack, ambient);
        }
        if !item_stack.is_empty() && self.interact_harness(player, item_stack) {
            return true;
        }
        let entity = self.get_entity();
        if self.is_wearing_body_armor()
            && !player.get_entity().sneaking.load(Ordering::Relaxed)
            && entity
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
                < MAX_PASSENGERS
        {
            return self.do_player_ride(player);
        }
        self.animal_interact(player, item_stack, ambient)
    }
}
