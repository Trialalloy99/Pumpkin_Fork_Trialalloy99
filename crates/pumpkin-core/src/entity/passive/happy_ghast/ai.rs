use pumpkin_data::environment_attribute::Activity;

use super::HappyGhastEntity;
use crate::entity::ai::brain::{
    Brain, BrainProvider,
    activity::ActivityData,
    behavior::{
        AnimalPanic, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink, MoveToTargetSink,
        Swim, Timed, baby_follow_adult, random_stroll, run_one, set_walk_target_from_look_target,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types},
    sensing::SensorType,
};
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 1.1;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const BABY_GHAST_CLOSE_ENOUGH_DIST: f64 = 3.0;
const ADULT_FOLLOW_RANGE_MIN: i32 = 3;
const ADULT_FOLLOW_RANGE_MAX: i32 = 16;

const MEMORIES: [MemoryModuleId; 4] = [
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::BREED_TARGET.id(),
    types::IS_PANICKING.id(),
    types::NEAREST_VISIBLE_ADULT.id(),
];

// happy ghast: Brain.provider(NEAREST_LIVING_ENTITIES, HURT_BY, FOOD_TEMPTATIONS, NEAREST_ADULT_ANY_TYPE, NEAREST_PLAYERS)
pub(super) fn make_brain(ghast: &HappyGhastEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::HurtBy,
            SensorType::FoodTemptations,
            SensorType::NearestAdultAnyType,
            SensorType::NearestPlayers,
        ],
        activities,
    }
    .make_brain(ghast, packed)
}

// happy ghast: HappyGhastAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Panic, Activity::Idle]);
}

// happy ghast: HappyGhastAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(Timed::new(AnimalPanic::new(
                    SPEED_MULTIPLIER_WHEN_PANICKING,
                ))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
                Box::new(CountDownCooldownTicks::new(
                    types::TEMPTATION_COOLDOWN_TICKS,
                )),
            ],
        ),
        ActivityData::with_priority_start(
            Activity::Idle,
            1,
            vec![
                Box::new(Timed::new(FollowTemptation::new(
                    SPEED_MULTIPLIER_WHEN_TEMPTED,
                    BABY_GHAST_CLOSE_ENOUGH_DIST,
                    true,
                ))),
                Box::new(baby_follow_adult::create(
                    ADULT_FOLLOW_RANGE_MIN,
                    ADULT_FOLLOW_RANGE_MAX,
                    SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                    types::NEAREST_VISIBLE_PLAYER,
                    true,
                    baby_follow_adult::player_as_entity,
                )),
                Box::new(baby_follow_adult::create(
                    ADULT_FOLLOW_RANGE_MIN,
                    ADULT_FOLLOW_RANGE_MAX,
                    SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                    types::NEAREST_VISIBLE_ADULT,
                    true,
                    baby_follow_adult::same_entity,
                )),
                Box::new(run_one(vec![
                    (
                        Box::new(random_stroll::fly(SPEED_MULTIPLIER_WHEN_IDLING)),
                        1,
                    ),
                    (
                        Box::new(set_walk_target_from_look_target::create(
                            SPEED_MULTIPLIER_WHEN_IDLING,
                            3,
                        )),
                        1,
                    ),
                ])),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Panic,
            0,
            vec![],
            vec![(types::IS_PANICKING.id(), MemoryStatus::ValuePresent)],
        ),
    ]
}
