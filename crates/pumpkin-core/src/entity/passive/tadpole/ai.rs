use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;

use super::TadpoleEntity;
use crate::entity::ai::brain::{
    Brain, BrainProvider,
    activity::ActivityData,
    behavior::{
        AnimalPanic, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink, MoveToTargetSink,
        Timed,
        gate::{GateBehavior, OrderPolicy, RunningPolicy},
        one_shot, random_stroll, set_entity_look_target_sometimes,
        set_walk_target_from_look_target,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types},
    sensing::SensorType,
};
use crate::entity::mob::Mob;

// tadpole: TadpoleAi speed multipliers
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER: f32 = 0.5;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const LOOK_AT_PLAYER_RANGE: f32 = 6.0;
const LOOK_AT_PLAYER_INTERVAL: (i32, i32) = (30, 60);

const MEMORIES: [MemoryModuleId; 14] = [
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::TEMPTING_PLAYER.id(),
    types::IS_PANICKING.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::NEAREST_PLAYERS.id(),
    types::NEAREST_VISIBLE_PLAYER.id(),
    types::NEAREST_VISIBLE_ATTACKABLE_PLAYER.id(),
    types::NEAREST_VISIBLE_ATTACKABLE_PLAYERS.id(),
    types::HURT_BY.id(),
    types::HURT_BY_ENTITY.id(),
];

// tadpole: Brain.provider(NEAREST_LIVING_ENTITIES, NEAREST_PLAYERS, HURT_BY, FROG_TEMPTATIONS)
pub(super) fn make_brain(tadpole: &TadpoleEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::NearestPlayers,
            SensorType::HurtBy,
            SensorType::FoodTemptations,
        ],
        activities,
    }
    .make_brain(tadpole, packed)
}

// tadpole: TadpoleAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Idle]);
}

// tadpole: TadpoleAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![core_activity(), idle_activity()]
}

// tadpole: TadpoleAi.initCoreActivity
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
        ],
    )
}

// tadpole: TadpoleAi.initIdleActivity
fn idle_activity() -> ActivityData {
    ActivityData::with_pairs(
        Activity::Idle,
        vec![
            (
                0,
                Box::new(set_entity_look_target_sometimes::create_for_type(
                    &EntityType::PLAYER,
                    LOOK_AT_PLAYER_RANGE,
                    LOOK_AT_PLAYER_INTERVAL.0,
                    LOOK_AT_PLAYER_INTERVAL.1,
                )),
            ),
            (
                1,
                Box::new(Timed::new(FollowTemptation::new(
                    SPEED_MULTIPLIER_WHEN_TEMPTED,
                    2.5,
                    false,
                ))),
            ),
            (
                2,
                Box::new(GateBehavior::new(
                    "Gate",
                    vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
                    Vec::new(),
                    OrderPolicy::Ordered,
                    RunningPolicy::TryAll,
                    vec![
                        (
                            Box::new(random_stroll::swim(SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER)),
                            2,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create(
                                SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER,
                                3,
                            )),
                            3,
                        ),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), |tick| {
                                tick.mob.get_entity().is_in_water()
                            })),
                            5,
                        ),
                    ],
                )),
            ),
        ],
    )
}
