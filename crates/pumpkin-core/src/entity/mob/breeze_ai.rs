use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pumpkin_data::Block;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityPose, EntityType};
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_util::Difficulty;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use rand::seq::SliceRandom;

use super::breeze::{
    BreezeEntity, JUMP_CIRCLE_INNER_RADIUS, JUMP_CIRCLE_MIDDLE_RADIUS, bottom_centered, centered,
    has_line_of_sight, look_at_eyes, random_point_behind_target,
};
use crate::entity::ai::brain::activity::ActivityData;
use crate::entity::ai::brain::behavior::long_jump_util;
use crate::entity::ai::brain::behavior::{
    Behavior, DoNothing, LookAtTargetSink, MoveToTargetSink, Swim, Timed, random_stroll, run_one,
    start_attacking, stop_attacking_if_target_invalid,
};
use crate::entity::ai::brain::memory::walk_target::WalkTarget;
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, PackedMemories, types};
use crate::entity::ai::brain::sensing::{SensorType, is_entity_attackable};
use crate::entity::ai::brain::{Brain, BrainProvider, BrainTick};
use crate::entity::ai::util::default_random_pos;
use crate::entity::mob::Mob;
use crate::entity::projectile::ThrownItemEntity;
use crate::entity::projectile::owner::ProjectileOwner;
use crate::entity::projectile::wind_charge::{WIND_CHARGE_GRAVITY, WindChargeEntity};
use crate::entity::{Entity, EntityBase};
use crate::world::natural_spawner::is_block_dangerous;

const SPEED_MULTIPLIER_WHEN_SLIDING: f32 = 0.6;
const TICKS_TO_REMEMBER_SEEN_TARGET: i32 = 100;
// breeze: LongJump / Shoot constants
const REQUIRED_AIR_BLOCKS_ABOVE: i32 = 4;
const JUMP_COOLDOWN_TICKS: i64 = 10;
const JUMP_COOLDOWN_WHEN_HURT_TICKS: i64 = 2;
const INHALING_DURATION_TICKS: i64 = 10;
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 0.058_333_334;
const ALLOWED_ANGLES: [i32; 5] = [40, 55, 60, 75, 80];
const ATTACK_RANGE_MAX_SQRT: f64 = 256.0;
const PROJECTILE_MOVEMENT_SCALE: f64 = 0.7;
const SHOOT_INITIAL_DELAY_TICKS: i64 = 15;
const SHOOT_RECOVER_DELAY_TICKS: i64 = 4;
const SHOOT_COOLDOWN_TICKS: i64 = 10;
const LONG_JUMP_DURATION: i32 = 200;

const MEMORIES: [MemoryModuleId; 0] = [];

// breeze: Brain.provider(NEAREST_LIVING_ENTITIES, HURT_BY, NEAREST_PLAYERS, BREEZE_ATTACK_ENTITY_SENSOR)
pub(super) fn make_brain(breeze: &BreezeEntity, packed: &PackedMemories) -> Brain {
    let mut brain = BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::HurtBy,
            SensorType::NearestPlayers,
            SensorType::BreezeAttackEntity,
        ],
        activities,
    }
    .make_brain(breeze, packed);
    brain.set_default_activity(Activity::Fight);
    brain.use_default_activity();
    brain
}

// breeze: BreezeAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Fight, Activity::Idle]);
}

fn breeze_of<'a>(tick: &BrainTick<'a>) -> Option<&'a BreezeEntity> {
    tick.mob.cast_any().downcast_ref::<BreezeEntity>()
}

fn set_pose(tick: &BrainTick<'_>, pose: EntityPose) {
    tick.mob.get_entity().set_pose(pose);
}

fn pose(tick: &BrainTick<'_>) -> EntityPose {
    tick.mob.get_entity().pose.load()
}

fn play_sound(tick: &BrainTick<'_>, sound: Sound, volume: f32, pitch: f32) {
    let entity = tick.mob.get_entity();
    entity.world.load().play_sound_fine(
        sound,
        SoundCategory::Hostile,
        &entity.pos.load(),
        volume,
        pitch,
    );
}

fn follow_range(tick: &BrainTick<'_>) -> f64 {
    tick.mob
        .get_mob_entity()
        .living_entity
        .get_attribute_value(&Attributes::FOLLOW_RANGE)
}

// breeze: BreezeAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    // breeze: BreezeAi.initFightActivity, Sensor.wasEntityAttackableLastNTicks
    let mut positives_left = 0;
    let stop_attacking = stop_attacking_if_target_invalid::create(move |tick, target| {
        if is_entity_attackable(&tick.visibility(), target) {
            positives_left = TICKS_TO_REMEMBER_SEEN_TARGET;
            false
        } else {
            positives_left -= 1;
            positives_left < 0
        }
    });
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
            ],
        ),
        ActivityData::with_priority_start(
            Activity::Idle,
            0,
            vec![
                Box::new(start_attacking::create(
                    |_| true,
                    |tick| tick.state.get(types::NEAREST_ATTACKABLE).cloned(),
                )),
                Box::new(start_attacking::create(
                    |_| true,
                    |tick| {
                        tick.state
                            .get(types::HURT_BY)
                            .and_then(|hurt| hurt.attacker.clone())
                            .filter(|entity| entity.get_living_entity().is_some())
                    },
                )),
                Box::new(Timed::new(SlideToTargetSink(MoveToTargetSink::new(20, 40)))),
                Box::new(run_one(vec![
                    (Box::new(DoNothing::new(20, 100)), 1),
                    (Box::new(random_stroll::stroll(0.6)), 2),
                ])),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Fight,
            0,
            vec![
                Box::new(stop_attacking),
                Box::new(Timed::new(Shoot)),
                Box::new(Timed::new(LongJump)),
                Box::new(Timed::new(ShootWhenStuck)),
                Box::new(Timed::new(Slide)),
            ],
            vec![
                (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
                (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            ],
        ),
    ]
}

// breeze: BreezeAi.SlideToTargetSink
struct SlideToTargetSink(MoveToTargetSink);

impl Behavior for SlideToTargetSink {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        self.0.entry_conditions()
    }

    fn min_duration(&self) -> i32 {
        self.0.min_duration()
    }

    fn max_duration(&self) -> i32 {
        self.0.max_duration()
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        self.0.check_extra_start_conditions(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        self.0.can_still_use(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.0.start(tick);
        play_sound(tick, Sound::EntityBreezeSlide, 1.0, 1.0);
        set_pose(tick, EntityPose::Sliding);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        self.0.tick(tick);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        self.0.stop(tick);
        set_pose(tick, EntityPose::Standing);
        if tick.state.has_memory_value(types::ATTACK_TARGET.id()) {
            tick.state.set_with_expiry(types::BREEZE_SHOOT, (), 60);
        }
    }

    fn debug_name(&self) -> &'static str {
        "SlideToTargetSink"
    }
}

// breeze: Shoot
const SHOOT_CONDITIONS: [(MemoryModuleId, MemoryStatus); 7] = [
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::BREEZE_SHOOT_COOLDOWN.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_SHOOT_CHARGING.id(), MemoryStatus::ValueAbsent),
    (
        types::BREEZE_SHOOT_RECOVERING.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::BREEZE_SHOOT.id(), MemoryStatus::ValuePresent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_JUMP_TARGET.id(), MemoryStatus::ValueAbsent),
];

struct Shoot;

impl Shoot {
    // breeze: Shoot.isTargetWithinRange
    fn is_target_within_range(tick: &BrainTick<'_>) -> Option<bool> {
        let target = tick.state.get(types::ATTACK_TARGET)?;
        let distance_sqr = tick
            .mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load());
        Some(distance_sqr < ATTACK_RANGE_MAX_SQRT)
    }

    // breeze: Projectile.spawnProjectileUsingShoot(BreezeWindCharge)
    fn spawn_wind_charge(tick: &BrainTick<'_>, breeze: &BreezeEntity, target: &dyn EntityBase) {
        let entity = breeze.get_entity();
        let world = entity.world.load_full();
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();
        let pos = entity.pos.load();
        let firing_y = breeze.firing_y_position();
        let target_factor = if target_entity.has_vehicle() {
            0.8
        } else {
            0.3
        };
        let xd = target_pos.x - pos.x;
        let yd = target_pos.y
            + f64::from(target_entity.entity_dimension.load().height) * target_factor
            - firing_y;
        let zd = target_pos.z - pos.z;
        let charge_entity = Entity::new(
            world.clone(),
            Vector3::new(pos.x, firing_y, pos.z),
            &EntityType::BREEZE_WIND_CHARGE,
        );
        let thrown = ThrownItemEntity {
            entity: charge_entity,
            owner: ProjectileOwner::new(Some(entity)),
            collides_with_projectiles: false,
            has_hit: AtomicBool::new(false),
            gravity: WIND_CHARGE_GRAVITY,
        };
        let difficulty_id = match world.level_info.load().difficulty {
            Difficulty::Peaceful => 0,
            Difficulty::Easy => 1,
            Difficulty::Normal => 2,
            Difficulty::Hard => 3,
        };
        thrown.set_velocity(
            xd,
            yd,
            zd,
            PROJECTILE_MOVEMENT_SCALE,
            f64::from(5 - difficulty_id * 4),
        );
        world.spawn_entity(Arc::new(WindChargeEntity::new_breeze(thrown)));
        play_sound(tick, Sound::EntityBreezeShoot, 1.5, 1.0);
    }
}

impl Behavior for Shoot {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SHOOT_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        (SHOOT_INITIAL_DELAY_TICKS + 1 + SHOOT_RECOVER_DELAY_TICKS) as i32
    }

    fn max_duration(&self) -> i32 {
        self.min_duration()
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if pose(tick) != EntityPose::Standing {
            return false;
        }
        let Some(within_range) = Self::is_target_within_range(tick) else {
            return false;
        };
        if !within_range {
            tick.state.erase(types::BREEZE_SHOOT.id());
        }
        within_range
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::ATTACK_TARGET.id())
            && tick.state.has_memory_value(types::BREEZE_SHOOT.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if tick.state.has_memory_value(types::ATTACK_TARGET.id()) {
            set_pose(tick, EntityPose::Shooting);
        }
        tick.state
            .set_with_expiry(types::BREEZE_SHOOT_CHARGING, (), SHOOT_INITIAL_DELAY_TICKS);
        play_sound(tick, Sound::EntityBreezeInhale, 1.0, 1.0);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if pose(tick) == EntityPose::Shooting {
            set_pose(tick, EntityPose::Standing);
        }
        tick.state
            .set_with_expiry(types::BREEZE_SHOOT_COOLDOWN, (), SHOOT_COOLDOWN_TICKS);
        tick.state.erase(types::BREEZE_SHOOT.id());
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return;
        };
        look_at_eyes(tick.mob, target.get_entity().pos.load());
        if tick
            .state
            .has_memory_value(types::BREEZE_SHOOT_CHARGING.id())
            || tick
                .state
                .has_memory_value(types::BREEZE_SHOOT_RECOVERING.id())
        {
            return;
        }
        tick.state.set_with_expiry(
            types::BREEZE_SHOOT_RECOVERING,
            (),
            SHOOT_RECOVER_DELAY_TICKS,
        );
        if let Some(breeze) = breeze_of(tick) {
            Self::spawn_wind_charge(tick, breeze, target.as_ref());
        }
    }

    fn debug_name(&self) -> &'static str {
        "Shoot"
    }
}

// breeze: ShootWhenStuck
const SHOOT_WHEN_STUCK_CONDITIONS: [(MemoryModuleId, MemoryStatus); 5] = [
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::BREEZE_JUMP_INHALING.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_JUMP_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_SHOOT.id(), MemoryStatus::ValueAbsent),
];

struct ShootWhenStuck;

impl Behavior for ShootWhenStuck {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SHOOT_WHEN_STUCK_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        entity.has_vehicle()
            || entity.is_in_water()
            || tick
                .mob
                .get_mob_entity()
                .living_entity
                .has_effect(&StatusEffect::LEVITATION)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.set_with_expiry(types::BREEZE_SHOOT, (), 60);
    }

    fn debug_name(&self) -> &'static str {
        "ShootWhenStuck"
    }
}

// breeze: Slide
const SLIDE_CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_JUMP_COOLDOWN.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_SHOOT.id(), MemoryStatus::ValueAbsent),
];

struct Slide;

impl Slide {
    // breeze: Slide.randomPointInMiddleCircle
    fn random_point_in_middle_circle(
        tick: &BrainTick<'_>,
        enemy_pos: Vector3<f64>,
    ) -> Vector3<f64> {
        let our_pos = tick.mob.get_entity().pos.load();
        let direction = enemy_pos.sub(&our_pos);
        let lerp = tick.mob.get_random().random::<f64>();
        let distance = direction.length()
            - (JUMP_CIRCLE_MIDDLE_RADIUS
                + lerp * (JUMP_CIRCLE_INNER_RADIUS - JUMP_CIRCLE_MIDDLE_RADIUS));
        let target = direction.normalize().multiply(distance, distance, distance);
        our_pos.add(&target)
    }
}

impl Behavior for Slide {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SLIDE_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        entity.on_ground.load(Ordering::Relaxed)
            && !entity.is_in_water()
            && pose(tick) == EntityPose::Standing
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let Some(enemy) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return;
        };
        let Some(breeze) = breeze_of(tick) else {
            return;
        };
        let enemy_pos = enemy.get_entity().pos.load();
        let mut position = None;
        if breeze.within_inner_circle_range(enemy_pos) {
            let away = default_random_pos::get_pos_away(tick.mob, 5, 5, enemy_pos);
            if let Some(away) = away
                && has_line_of_sight(tick.mob, away)
                && enemy_pos.squared_distance_to_vec(&away)
                    > enemy_pos.squared_distance_to_vec(&tick.mob.get_entity().pos.load())
            {
                position = Some(away);
            }
        }
        let position = position.unwrap_or_else(|| {
            if tick.mob.get_random().random::<bool>() {
                random_point_behind_target(enemy.as_ref(), &mut tick.mob.get_random())
            } else {
                Self::random_point_in_middle_circle(tick, enemy_pos)
            }
        });
        tick.state.set(
            types::WALK_TARGET,
            WalkTarget::from_block_pos(
                BlockPos::floored_v(position),
                SPEED_MULTIPLIER_WHEN_SLIDING,
                1,
            ),
        );
    }

    fn debug_name(&self) -> &'static str {
        "Slide"
    }
}

// breeze: LongJump
const LONG_JUMP_CONDITIONS: [(MemoryModuleId, MemoryStatus); 7] = [
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::BREEZE_JUMP_COOLDOWN.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_JUMP_INHALING.id(), MemoryStatus::Registered),
    (types::BREEZE_JUMP_TARGET.id(), MemoryStatus::Registered),
    (types::BREEZE_SHOOT.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::BREEZE_LEAVING_WATER.id(), MemoryStatus::Registered),
];

struct LongJump;

impl LongJump {
    // breeze: LongJump.canRun
    fn can_run(tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        let on_ground = entity.on_ground.load(Ordering::Relaxed);
        let in_water = entity.is_in_water();
        if !on_ground && !in_water {
            return false;
        }
        // breeze: isInFluidDeeperThan(getFluidJumpThreshold, ENTITY_FLOATABLE), isInLava
        if f64::from(entity.water_height.load()) > entity.get_eye_height()
            || entity.touching_lava.load(Ordering::Relaxed)
        {
            return false;
        }
        if tick.state.has_memory_value(types::BREEZE_JUMP_TARGET.id()) {
            return true;
        }
        let Some(attack_target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return false;
        };
        let distance = entity
            .pos
            .load()
            .sub(&attack_target.get_entity().pos.load())
            .length();
        // breeze: outOfAggroRange
        if distance >= follow_range(tick) {
            tick.state.erase(types::ATTACK_TARGET.id());
            return false;
        }
        // breeze: tooCloseForJump
        if distance - JUMP_CIRCLE_INNER_RADIUS <= 0.0 {
            return false;
        }
        if !Self::can_jump_from_current_position(tick) {
            return false;
        }
        let point = random_point_behind_target(attack_target.as_ref(), &mut tick.mob.get_random());
        let Some(target_block) = Self::snap_to_surface(tick, point) else {
            return false;
        };
        let below = tick.world.get_block_state(&target_block.down());
        if is_block_dangerous(entity.entity_type, below) {
            return false;
        }
        if !has_line_of_sight(tick.mob, centered(target_block))
            && !has_line_of_sight(tick.mob, centered(target_block.up_height(4)))
        {
            return false;
        }
        tick.state.set(types::BREEZE_JUMP_TARGET, target_block);
        true
    }

    // breeze: LongJump.snapToSurface
    fn snap_to_surface(tick: &BrainTick<'_>, target: Vector3<f64>) -> Option<BlockPos> {
        let clip = |offset_y: f64| {
            tick.world.clip(&pumpkin_world::clip::ClipContext::new(
                target,
                Vector3::new(target.x, target.y + offset_y, target.z),
                pumpkin_world::clip::ClipBlock::Collider,
                pumpkin_world::clip::ClipFluid::None,
            ))
        };
        let below = clip(-10.0);
        if !below.is_miss {
            return Some(BlockPos::floored_v(below.location).up());
        }
        let above = clip(10.0);
        (!above.is_miss).then(|| BlockPos::floored_v(above.location).up())
    }

    // breeze: LongJump.canJumpFromCurrentPosition
    fn can_jump_from_current_position(tick: &BrainTick<'_>) -> bool {
        let current = tick.mob.get_entity().block_pos.load();
        if tick.world.get_block(&current) == &Block::HONEY_BLOCK {
            return false;
        }
        for i in 1..=REQUIRED_AIR_BLOCKS_ABOVE {
            let offset = current.up_height(i);
            if !tick.world.get_block_state(&offset).is_air()
                && !tick
                    .world
                    .get_fluid(&offset)
                    .has_tag(&pumpkin_data::tag::Fluid::MINECRAFT_WATER)
            {
                return false;
            }
        }
        true
    }

    // breeze: LongJump.calculateOptimalJumpVector
    fn calculate_optimal_jump_vector(
        tick: &BrainTick<'_>,
        target_pos: Vector3<f64>,
    ) -> Option<Vector3<f64>> {
        let mut angles = ALLOWED_ANGLES;
        angles.shuffle(&mut tick.mob.get_random());
        let living = &tick.mob.get_mob_entity().living_entity;
        for angle in angles {
            let max_jump_velocity = MAX_JUMP_VELOCITY_MULTIPLIER * follow_range(tick) as f32;
            let Some(vector) = long_jump_util::calculate_jump_vector_for_angle(
                tick.mob,
                target_pos,
                max_jump_velocity,
                angle,
                false,
            ) else {
                continue;
            };
            if let Some(jump_boost) = living.get_effect(&StatusEffect::JUMP_BOOST) {
                // living: getJumpBoostPower
                let power = f64::from(0.1_f32 * (jump_boost.amplifier + 1) as f32);
                return Some(vector.add_raw(0.0, vector.normalize().y * power, 0.0));
            }
            return Some(vector);
        }
        None
    }

    fn is_finished_inhaling(tick: &BrainTick<'_>) -> bool {
        !tick
            .state
            .has_memory_value(types::BREEZE_JUMP_INHALING.id())
            && pose(tick) == EntityPose::Inhaling
    }

    fn is_finished_jumping(tick: &BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        let landed_on_ground = entity.on_ground.load(Ordering::Relaxed);
        let landed_in_water = entity.is_in_water()
            && !tick
                .state
                .has_memory_value(types::BREEZE_LEAVING_WATER.id());
        pose(tick) == EntityPose::LongJumping && (landed_on_ground || landed_in_water)
    }
}

impl Behavior for LongJump {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &LONG_JUMP_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        LONG_JUMP_DURATION
    }

    fn max_duration(&self) -> i32 {
        LONG_JUMP_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::can_run(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        pose(tick) != EntityPose::Standing
            && !tick
                .state
                .has_memory_value(types::BREEZE_JUMP_COOLDOWN.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if !tick
            .state
            .has_memory_value(types::BREEZE_JUMP_INHALING.id())
        {
            tick.state
                .set_with_expiry(types::BREEZE_JUMP_INHALING, (), INHALING_DURATION_TICKS);
        }
        set_pose(tick, EntityPose::Inhaling);
        play_sound(tick, Sound::EntityBreezeCharge, 1.0, 1.0);
        if let Some(target) = tick.state.get(types::BREEZE_JUMP_TARGET).copied() {
            look_at_eyes(tick.mob, centered(target));
        }
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        let living = &tick.mob.get_mob_entity().living_entity;
        let in_water = entity.is_in_water();
        if !in_water
            && tick
                .state
                .has_memory_value(types::BREEZE_LEAVING_WATER.id())
        {
            tick.state.erase(types::BREEZE_LEAVING_WATER.id());
        }
        if Self::is_finished_inhaling(tick) {
            let velocity = tick
                .state
                .get(types::BREEZE_JUMP_TARGET)
                .copied()
                .and_then(|target| {
                    Self::calculate_optimal_jump_vector(tick, bottom_centered(target))
                });
            let Some(velocity) = velocity else {
                set_pose(tick, EntityPose::Standing);
                return;
            };
            if in_water {
                tick.state.set(types::BREEZE_LEAVING_WATER, ());
            }
            play_sound(tick, Sound::EntityBreezeJump, 1.0, 1.0);
            set_pose(tick, EntityPose::LongJumping);
            entity.yaw.store(entity.body_yaw.load());
            living.discard_friction.store(true, Ordering::Relaxed);
            entity.velocity.store(velocity);
            entity.velocity_dirty.store(true, Ordering::Relaxed);
        } else if Self::is_finished_jumping(tick) {
            play_sound(tick, Sound::EntityBreezeLand, 1.0, 1.0);
            set_pose(tick, EntityPose::Standing);
            living.discard_friction.store(false, Ordering::Relaxed);
            let was_hurt = tick.state.has_memory_value(types::HURT_BY.id());
            tick.state.set_with_expiry(
                types::BREEZE_JUMP_COOLDOWN,
                (),
                if was_hurt {
                    JUMP_COOLDOWN_WHEN_HURT_TICKS
                } else {
                    JUMP_COOLDOWN_TICKS
                },
            );
            tick.state.set_with_expiry(types::BREEZE_SHOOT, (), 100);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let current = pose(tick);
        if current == EntityPose::LongJumping || current == EntityPose::Inhaling {
            set_pose(tick, EntityPose::Standing);
        }
        tick.state.erase(types::BREEZE_JUMP_TARGET.id());
        tick.state.erase(types::BREEZE_JUMP_INHALING.id());
        tick.state.erase(types::BREEZE_LEAVING_WATER.id());
    }

    fn debug_name(&self) -> &'static str {
        "LongJump"
    }
}
