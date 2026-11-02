use std::sync::Arc;

use pumpkin_data::environment_attribute::Activity;

use super::creaking::CreakingEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::activity::ActivityData;
use crate::entity::ai::brain::behavior::{
    Behavior, DoNothing, LookAtTargetSink, MoveToTargetSink, Swim, Timed, melee_attack,
    random_stroll, run_one, set_entity_look_target_sometimes,
    set_walk_target_from_attack_target_if_target_out_of_reach, set_walk_target_from_look_target,
    start_attacking, stop_attacking_if_target_invalid,
};
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, PackedMemories, types};
use crate::entity::ai::brain::sensing::SensorType;
use crate::entity::ai::brain::{Brain, BrainProvider, BrainTick};
use crate::entity::mob::Mob;

const MEMORIES: [MemoryModuleId; 0] = [];
const ATTACK_INTERVAL: i64 = 40;
const MOVEMENT_SPEED_WHEN_FIGHTING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.3;
const LOOK_DISTANCE: f32 = 8.0;
const LOOK_MIN_INTERVAL: i32 = 30;
const LOOK_MAX_INTERVAL: i32 = 60;
const IDLE_PRIORITY_START: i32 = 10;

// creaking: Brain.provider(NEAREST_LIVING_ENTITIES, NEAREST_PLAYERS)
pub(super) fn make_brain(creaking: &CreakingEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::NearestPlayers,
        ],
        activities,
    }
    .make_brain(creaking, packed)
}

// creaking: CreakingAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain, can_move: bool) {
    if can_move {
        brain.set_active_activity_to_first_valid(&[Activity::Fight, Activity::Idle]);
    } else {
        brain.use_default_activity();
    }
}

fn creaking_of(mob: &dyn Mob) -> Option<&CreakingEntity> {
    mob.cast_any().downcast_ref::<CreakingEntity>()
}

// creaking: CreakingAi.initCoreActivity, Swim only while the creaking can move
struct CreakingSwim(Swim);

impl Behavior for CreakingSwim {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        self.0.entry_conditions()
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        creaking_of(tick.mob).is_some_and(CreakingEntity::can_move)
            && self.0.check_extra_start_conditions(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        self.0.can_still_use(tick)
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        self.0.tick(tick);
    }

    fn debug_name(&self) -> &'static str {
        "Swim"
    }
}

// creaking: CreakingAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(CreakingSwim(Swim::new(0.8)))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
            ],
        ),
        ActivityData::with_priority_start(
            Activity::Idle,
            IDLE_PRIORITY_START,
            vec![
                Box::new(start_attacking::create(
                    |tick| creaking_of(tick.mob).is_some_and(CreakingEntity::is_active),
                    |tick| {
                        tick.state
                            .get(types::NEAREST_VISIBLE_ATTACKABLE_PLAYER)
                            .map(|player| Arc::clone(player) as Arc<dyn EntityBase>)
                    },
                )),
                Box::new(set_entity_look_target_sometimes::create(
                    LOOK_DISTANCE,
                    LOOK_MIN_INTERVAL,
                    LOOK_MAX_INTERVAL,
                )),
                Box::new(run_one(vec![
                    (
                        Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                        2,
                    ),
                    (
                        Box::new(set_walk_target_from_look_target::create(
                            SPEED_MULTIPLIER_WHEN_IDLING,
                            3,
                        )),
                        2,
                    ),
                    (Box::new(DoNothing::new(30, 60)), 1),
                ])),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Fight,
            IDLE_PRIORITY_START,
            vec![
                Box::new(
                    set_walk_target_from_attack_target_if_target_out_of_reach::create(
                        MOVEMENT_SPEED_WHEN_FIGHTING,
                    ),
                ),
                Box::new(melee_attack::create(
                    |mob| creaking_of(mob).is_some_and(CreakingEntity::can_move),
                    ATTACK_INTERVAL,
                )),
                Box::new(stop_attacking_if_target_invalid::create(|tick, target| {
                    // creaking: isAttackTargetStillReachable
                    let target_id = target.get_entity().entity_id;
                    !tick
                        .state
                        .get(types::NEAREST_VISIBLE_ATTACKABLE_PLAYERS)
                        .is_some_and(|players| {
                            players
                                .iter()
                                .any(|player| player.get_entity().entity_id == target_id)
                        })
                })),
            ],
            vec![(types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent)],
        ),
    ]
}
