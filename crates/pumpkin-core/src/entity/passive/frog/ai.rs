use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::Block;
use pumpkin_data::entity::{EntityPose, EntityType};
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use super::FrogEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        AnimalPanic, Behavior, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink,
        MoveToTargetSink, Timed,
        animal_make_love::AnimalMakeLove,
        gate::{GateBehavior, OrderPolicy, RunningPolicy},
        long_jump::{LongJumpMidJump, LongJumpToRandomPos},
        one_shot, random_stroll, run_one_with_conditions, set_entity_look_target_sometimes,
        set_walk_target_from_look_target, start_attacking, stop_attacking_if_target_invalid,
        try_find_land, try_lay_spawn_on_fluid_near_land, utils,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types, walk_target::WalkTarget},
    sensing::SensorType,
};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::ai::pathfinder::pathfinding_context::PathfindingContext;
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_ON_LAND: f32 = 1.0;
const SPEED_MULTIPLIER_IN_WATER: f32 = 0.75;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const TIME_BETWEEN_LONG_JUMPS: (i32, i32) = (100, 140);
const MAX_LONG_JUMP_HEIGHT: i32 = 2;
const MAX_LONG_JUMP_WIDTH: i32 = 4;
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 3.571_428_8;
const PREFERRED_BLOCKS_CHANCE: f32 = 0.5;
const LOOK_AT_PLAYER_RANGE: f32 = 6.0;
const LOOK_AT_PLAYER_INTERVAL: (i32, i32) = (30, 60);
const ADJACENT_LAND_SEARCH_RANGE: i32 = 6;
const SWIM_LAND_SEARCH_RANGE: i32 = 8;

const CROAK_TICKS: i32 = 60;
const CROAK_TIME_OUT_DURATION: i32 = 100;

const SHOOT_TONGUE_TIME_OUT_DURATION: i32 = 100;
const CATCH_ANIMATION_DURATION: i32 = 6;
const TONGUE_ANIMATION_DURATION: i32 = 10;
const EATING_DISTANCE: f64 = 1.75;
const EATING_MOVEMENT_FACTOR: f64 = 0.75;
const UNREACHABLE_TONGUE_TARGETS_COOLDOWN_DURATION: i64 = 100;
const MAX_UNREACHABLE_TONGUE_TARGETS_IN_MEMORY: usize = 5;
const TONGUE_WALK_SPEED: f32 = 2.0;
const TONGUE_PATH_RECALCULATION_TICKS: i32 = 10;

const MEMORIES: [MemoryModuleId; 20] = [
    types::LONG_JUMP_COOLDOWN_TICKS.id(),
    types::LONG_JUMP_MID_JUMP.id(),
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::TEMPTING_PLAYER.id(),
    types::BREED_TARGET.id(),
    types::IS_PANICKING.id(),
    types::IS_IN_WATER.id(),
    types::IS_PREGNANT.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::ATTACK_TARGET.id(),
    types::ATTACK_COOLING_DOWN.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
    types::NEAREST_ATTACKABLE.id(),
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::UNREACHABLE_TONGUE_TARGETS.id(),
    types::HURT_BY.id(),
    types::HURT_BY_ENTITY.id(),
    types::NEAREST_VISIBLE_ADULT.id(),
];

// frog: Brain.provider(NEAREST_LIVING_ENTITIES, HURT_BY, FROG_ATTACKABLES, FROG_TEMPTATIONS, IS_IN_WATER)
pub(super) fn make_brain(frog: &FrogEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::HurtBy,
            SensorType::FrogAttackables,
            SensorType::FoodTemptations,
            SensorType::IsInWater,
        ],
        activities,
    }
    .make_brain(frog, packed)
}

// frog: FrogAi.initMemories
pub(super) fn init_memories(frog: &FrogEntity) {
    let cooldown = frog
        .get_random()
        .random_range(TIME_BETWEEN_LONG_JUMPS.0..=TIME_BETWEEN_LONG_JUMPS.1);
    frog.mob_entity.with_brain(frog, |brain| {
        brain.set(types::LONG_JUMP_COOLDOWN_TICKS, cooldown);
    });
}

// frog: FrogAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[
        Activity::Tongue,
        Activity::LaySpawn,
        Activity::LongJump,
        Activity::Swim,
        Activity::Idle,
    ]);
}

// frog: FrogAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        core_activity(),
        idle_activity(),
        swim_activity(),
        lay_spawn_activity(),
        tongue_activity(),
        jump_activity(),
    ]
}

fn in_water(tick: &BrainTick<'_>) -> bool {
    tick.mob.get_entity().is_in_water()
}

fn on_ground(tick: &BrainTick<'_>) -> bool {
    tick.mob.get_entity().on_ground.load(Ordering::Relaxed)
}

// frog: FrogAi.canAttack
fn can_attack(tick: &BrainTick<'_>) -> bool {
    !utils::is_breeding(tick.state)
}

fn find_attackable(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
    tick.state.get(types::NEAREST_ATTACKABLE).cloned()
}

fn look_at_players() -> one_shot::OneShot {
    set_entity_look_target_sometimes::create_for_type(
        &EntityType::PLAYER,
        LOOK_AT_PLAYER_RANGE,
        LOOK_AT_PLAYER_INTERVAL.0,
        LOOK_AT_PLAYER_INTERVAL.1,
    )
}

fn tempted() -> Timed<FollowTemptation> {
    Timed::new(FollowTemptation::new(
        SPEED_MULTIPLIER_WHEN_TEMPTED,
        2.5,
        false,
    ))
}

// frog: FrogAi.initCoreActivity
fn core_activity() -> ActivityData {
    ActivityData::with_priority_start(
        Activity::Core,
        0,
        vec![
            Box::new(Timed::new(AnimalPanic::new(
                SPEED_MULTIPLIER_WHEN_PANICKING,
            ))),
            Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
            Box::new(Timed::new(MoveToTargetSink::default())),
            Box::new(CountDownCooldownTicks::new(
                types::TEMPTATION_COOLDOWN_TICKS,
            )),
            Box::new(CountDownCooldownTicks::new(types::LONG_JUMP_COOLDOWN_TICKS)),
        ],
    )
}

// frog: FrogAi.initIdleActivity
fn idle_activity() -> ActivityData {
    ActivityData::with_pairs_and_conditions(
        Activity::Idle,
        vec![
            (0, Box::new(look_at_players())),
            (
                0,
                Box::new(Timed::new(AnimalMakeLove::new(&EntityType::FROG, |_| {}))),
            ),
            (1, Box::new(tempted())),
            (
                2,
                Box::new(start_attacking::create(can_attack, find_attackable)),
            ),
            (
                3,
                Box::new(try_find_land::create(
                    ADJACENT_LAND_SEARCH_RANGE,
                    SPEED_MULTIPLIER_ON_LAND,
                )),
            ),
            (
                4,
                Box::new(run_one_with_conditions(
                    vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
                    vec![
                        (
                            Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                            1,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create(
                                SPEED_MULTIPLIER_WHEN_IDLING,
                                3,
                            )),
                            1,
                        ),
                        (Box::new(Timed::new(Croak::default())), 3),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), on_ground)),
                            2,
                        ),
                    ],
                )),
            ),
        ],
        vec![
            (types::LONG_JUMP_MID_JUMP.id(), MemoryStatus::ValueAbsent),
            (types::IS_IN_WATER.id(), MemoryStatus::ValueAbsent),
        ],
    )
}

// frog: FrogAi.initSwimActivity
fn swim_activity() -> ActivityData {
    ActivityData::with_pairs_and_conditions(
        Activity::Swim,
        vec![
            (0, Box::new(look_at_players())),
            (1, Box::new(tempted())),
            (
                2,
                Box::new(start_attacking::create(can_attack, find_attackable)),
            ),
            (
                3,
                Box::new(try_find_land::create(SWIM_LAND_SEARCH_RANGE, 1.5)),
            ),
            (
                5,
                Box::new(GateBehavior::new(
                    "Gate",
                    vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
                    Vec::new(),
                    OrderPolicy::Ordered,
                    RunningPolicy::TryAll,
                    vec![
                        (Box::new(random_stroll::swim(SPEED_MULTIPLIER_IN_WATER)), 1),
                        (
                            Box::new(random_stroll::stroll_from_water(
                                SPEED_MULTIPLIER_ON_LAND,
                                true,
                            )),
                            1,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create(
                                SPEED_MULTIPLIER_WHEN_IDLING,
                                3,
                            )),
                            1,
                        ),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), in_water)),
                            5,
                        ),
                    ],
                )),
            ),
        ],
        vec![
            (types::LONG_JUMP_MID_JUMP.id(), MemoryStatus::ValueAbsent),
            (types::IS_IN_WATER.id(), MemoryStatus::ValuePresent),
        ],
    )
}

// frog: FrogAi.initLaySpawnActivity
fn lay_spawn_activity() -> ActivityData {
    ActivityData::with_pairs_and_conditions(
        Activity::LaySpawn,
        vec![
            (0, Box::new(look_at_players())),
            (
                1,
                Box::new(start_attacking::create(can_attack, find_attackable)),
            ),
            (
                2,
                Box::new(try_find_land::create_near_liquid(
                    SWIM_LAND_SEARCH_RANGE,
                    SPEED_MULTIPLIER_ON_LAND,
                    &tag::Fluid::MINECRAFT_FROG_TRIES_TO_FIND_LAND_NEAR,
                )),
            ),
            (
                3,
                Box::new(try_lay_spawn_on_fluid_near_land::create(&Block::FROGSPAWN)),
            ),
            (
                4,
                Box::new(run_one_with_conditions(
                    Vec::new(),
                    vec![
                        (
                            Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                            2,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create(
                                SPEED_MULTIPLIER_WHEN_IDLING,
                                3,
                            )),
                            1,
                        ),
                        (Box::new(Timed::new(Croak::default())), 2),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), on_ground)),
                            1,
                        ),
                    ],
                )),
            ),
        ],
        vec![
            (types::LONG_JUMP_MID_JUMP.id(), MemoryStatus::ValueAbsent),
            (types::IS_PREGNANT.id(), MemoryStatus::ValuePresent),
        ],
    )
}

// frog: FrogAi.initJumpActivity
fn jump_activity() -> ActivityData {
    ActivityData::with_pairs_and_conditions(
        Activity::LongJump,
        vec![
            (
                0,
                Box::new(Timed::new(LongJumpMidJump::new(
                    TIME_BETWEEN_LONG_JUMPS,
                    Sound::EntityFrogStep,
                ))),
            ),
            (
                1,
                Box::new(Timed::new(
                    LongJumpToRandomPos::new(
                        TIME_BETWEEN_LONG_JUMPS,
                        MAX_LONG_JUMP_HEIGHT,
                        MAX_LONG_JUMP_WIDTH,
                        MAX_JUMP_VELOCITY_MULTIPLIER,
                        |_| Sound::EntityFrogLongJump,
                    )
                    .with_preferred_block(
                        &tag::Block::MINECRAFT_FROG_PREFER_JUMP_TO,
                        PREFERRED_BLOCKS_CHANCE,
                    )
                    .with_landing_spot(is_acceptable_landing_spot),
                )),
            ),
        ],
        vec![
            (types::TEMPTING_PLAYER.id(), MemoryStatus::ValueAbsent),
            (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::LONG_JUMP_COOLDOWN_TICKS.id(),
                MemoryStatus::ValueAbsent,
            ),
            (types::IS_IN_WATER.id(), MemoryStatus::ValueAbsent),
        ],
    )
}

// frog: FrogAi.initTongueActivity
fn tongue_activity() -> ActivityData {
    ActivityData::with_gate_memory(
        Activity::Tongue,
        0,
        vec![
            Box::new(stop_attacking_if_target_invalid::create(|_, _| false)),
            Box::new(Timed::new(ShootTongue::default())),
        ],
        types::ATTACK_TARGET.id(),
    )
}

// frog: FrogAi.isAcceptableLandingSpot
fn is_acceptable_landing_spot(tick: &BrainTick<'_>, target: &BlockPos) -> bool {
    let world = tick.world;
    let below = target.down();
    let fluid_free = |pos: &BlockPos| world.get_fluid_and_fluid_state(pos).1.is_empty;
    if !(fluid_free(target) && fluid_free(&below) && fluid_free(&target.up())) {
        return false;
    }
    let prefer = &tag::Block::MINECRAFT_FROG_PREFER_JUMP_TO;
    if world.get_block(target).has_tag(prefer) || world.get_block(&below).has_tag(prefer) {
        return true;
    }
    let context = PathfindingContext::new(target.0, world.clone());
    let path_type = context.compute_path_type_from_state(target.0);
    let path_type_below = context.compute_path_type_from_state(below.0);
    if path_type != PathType::Trapdoor
        && (!world.get_block_state(target).is_air() || path_type_below != PathType::Trapdoor)
    {
        LongJumpToRandomPos::default_acceptable_landing_spot(tick, target)
    } else {
        true
    }
}

// frog: Croak
#[derive(Default)]
struct Croak {
    croak_counter: i32,
}

const CROAK_CONDITIONS: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)];

impl Behavior for Croak {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &CROAK_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        CROAK_TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        CROAK_TIME_OUT_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.mob.get_entity().pose.load() == EntityPose::Standing
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        self.croak_counter < CROAK_TICKS
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if !(entity.is_in_water() || entity.touching_lava.load(Ordering::Relaxed)) {
            entity.set_pose(EntityPose::Croaking);
            self.croak_counter = 0;
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob.get_entity().set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, _tick: &mut BrainTick<'_>) {
        self.croak_counter += 1;
    }

    fn debug_name(&self) -> &'static str {
        "Croak"
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TongueState {
    MoveToTarget,
    CatchAnimation,
    EatAnimation,
    #[default]
    Done,
}

// frog: ShootTongue
#[derive(Default)]
struct ShootTongue {
    eat_animation_timer: i32,
    calculate_path_counter: i32,
    state: TongueState,
}

const SHOOT_TONGUE_CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
];

impl ShootTongue {
    // frog: ShootTongue.canPathfindToTarget
    fn can_pathfind_to_target(tick: &BrainTick<'_>, target: &dyn EntityBase) -> bool {
        let mob_entity = tick.mob.get_mob_entity();
        let mut navigator = mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator
            .create_path(&mob_entity.living_entity, target.get_entity().pos.load(), 0)
            .is_some_and(|path| f64::from(path.get_dist_to_target()) < EATING_DISTANCE)
    }

    // frog: ShootTongue.addUnreachableTargetToMemory
    fn add_unreachable_target_to_memory(tick: &mut BrainTick<'_>, uuid: Uuid) {
        let mut targets = tick
            .state
            .get(types::UNREACHABLE_TONGUE_TARGETS)
            .cloned()
            .unwrap_or_default();
        let should_add = !targets.contains(&uuid);
        if targets.len() == MAX_UNREACHABLE_TONGUE_TARGETS_IN_MEMORY && should_add {
            targets.remove(0);
        }
        if should_add {
            targets.push(uuid);
        }
        tick.state.set_with_expiry(
            types::UNREACHABLE_TONGUE_TARGETS,
            targets,
            UNREACHABLE_TONGUE_TARGETS_COOLDOWN_DURATION,
        );
    }

    // frog: ShootTongue.eatEntity
    fn eat_entity(tick: &BrainTick<'_>, frog: &FrogEntity) {
        let entity = frog.get_entity();
        tick.world.play_sound_fine(
            Sound::EntityFrogEat,
            SoundCategory::Neutral,
            &entity.pos.load(),
            2.0,
            1.0,
        );
        if let Some(target) = frog.tongue_target()
            && target.get_entity().is_alive()
        {
            frog.get_mob_entity().try_attack(frog, target.as_ref());
            if !target.get_entity().is_alive() {
                target.get_entity().remove();
            }
        }
    }

    fn walk_to(tick: &mut BrainTick<'_>, target: &dyn EntityBase) {
        tick.state.set(
            types::WALK_TARGET,
            WalkTarget::from_vec(target.get_entity().pos.load(), TONGUE_WALK_SPEED, 0),
        );
    }
}

impl Behavior for ShootTongue {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SHOOT_TONGUE_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        SHOOT_TONGUE_TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        SHOOT_TONGUE_TIME_OUT_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return false;
        };
        let can_pathfind = Self::can_pathfind_to_target(tick, target.as_ref());
        if !can_pathfind {
            tick.state.erase(types::ATTACK_TARGET.id());
            Self::add_unreachable_target_to_memory(tick, target.get_entity().entity_uuid);
        }
        can_pathfind
            && tick.mob.get_entity().pose.load() != EntityPose::Croaking
            && FrogEntity::can_eat(target.as_ref())
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::ATTACK_TARGET.id())
            && self.state != TongueState::Done
            && !tick.state.has_memory_value(types::IS_PANICKING.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return;
        };
        utils::look_at_entity(tick.state, target.clone());
        if let Some(frog) = tick.mob.cast_any().downcast_ref::<FrogEntity>() {
            frog.set_tongue_target(Some(target.as_ref()));
        }
        Self::walk_to(tick, target.as_ref());
        self.calculate_path_counter = TONGUE_PATH_RECALCULATION_TICKS;
        self.state = TongueState::MoveToTarget;
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::ATTACK_TARGET.id());
        if let Some(frog) = tick.mob.cast_any().downcast_ref::<FrogEntity>() {
            frog.set_tongue_target(None);
        }
        tick.mob.get_entity().set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return;
        };
        let Some(frog) = tick.mob.cast_any().downcast_ref::<FrogEntity>() else {
            return;
        };
        frog.set_tongue_target(Some(target.as_ref()));
        match self.state {
            TongueState::MoveToTarget => {
                let body_pos = frog.get_entity().pos.load();
                let target_pos = target.get_entity().pos.load();
                if target_pos.squared_distance_to_vec(&body_pos) < EATING_DISTANCE * EATING_DISTANCE
                {
                    tick.world.play_sound_fine(
                        Sound::EntityFrogTongue,
                        SoundCategory::Neutral,
                        &body_pos,
                        2.0,
                        1.0,
                    );
                    frog.get_entity().set_pose(EntityPose::UsingTongue);
                    let pull = (body_pos - target_pos).normalize() * EATING_MOVEMENT_FACTOR;
                    target.get_entity().velocity.store(pull);
                    target
                        .get_entity()
                        .velocity_dirty
                        .store(true, Ordering::Relaxed);
                    self.eat_animation_timer = 0;
                    self.state = TongueState::CatchAnimation;
                } else if self.calculate_path_counter <= 0 {
                    Self::walk_to(tick, target.as_ref());
                    self.calculate_path_counter = TONGUE_PATH_RECALCULATION_TICKS;
                } else {
                    self.calculate_path_counter -= 1;
                }
            }
            TongueState::CatchAnimation => {
                let timer = self.eat_animation_timer;
                self.eat_animation_timer += 1;
                if timer >= CATCH_ANIMATION_DURATION {
                    self.state = TongueState::EatAnimation;
                    Self::eat_entity(tick, frog);
                }
            }
            TongueState::EatAnimation => {
                if self.eat_animation_timer >= TONGUE_ANIMATION_DURATION {
                    self.state = TongueState::Done;
                } else {
                    self.eat_animation_timer += 1;
                }
            }
            TongueState::Done => {}
        }
    }

    fn debug_name(&self) -> &'static str {
        "ShootTongue"
    }
}
