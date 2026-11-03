use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use rand::RngExt;

use super::{ArmadilloEntity, ArmadilloState};
use crate::entity::EntityBase;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        AnimalPanic, Behavior, CountDownCooldownTicks, DoNothing, FollowTemptation,
        LookAtTargetSink, MoveToTargetSink, OneShot, RandomLookAround, Swim, Timed,
        animal_make_love::AnimalMakeLove, baby_follow_adult, random_stroll, run_one,
        run_one_with_conditions, set_entity_look_target_sometimes,
        set_walk_target_from_look_target,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types},
    sensing::SensorType,
};
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 1.25;
const DEFAULT_CLOSE_ENOUGH_DIST: f64 = 2.0;
const BABY_CLOSE_ENOUGH_DIST: f64 = 1.0;
const ADULT_FOLLOW_RANGE: (i32, i32) = (5, 16);
// ArmadilloBallUp
const BALL_UP_STAY_IN_STATE: i32 = 5 * 60 * 20;
const DANGER_DETECTED_RECENTLY_DANGER_THRESHOLD: i64 = 75;

const MEMORIES: [MemoryModuleId; 9] = [
    types::GAZE_COOLDOWN_TICKS.id(),
    types::IS_PANICKING.id(),
    types::DANGER_DETECTED_RECENTLY.id(),
    types::TEMPTING_PLAYER.id(),
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::BREED_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::WALK_TARGET.id(),
    types::NEAREST_VISIBLE_ADULT.id(),
];

// armadillo: Brain.provider(NEAREST_LIVING_ENTITIES, HURT_BY, FOOD_TEMPTATIONS, NEAREST_ADULT, ARMADILLO_SCARE_DETECTED)
pub(super) fn make_brain(armadillo: &ArmadilloEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::HurtBy,
            SensorType::FoodTemptations,
            SensorType::NearestAdult,
            SensorType::ArmadilloScareDetected,
        ],
        activities,
    }
    .make_brain(armadillo, packed)
}

// armadillo: ArmadilloAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Panic, Activity::Idle]);
}

fn armadillo_of(mob: &dyn Mob) -> Option<&ArmadilloEntity> {
    mob.cast_any().downcast_ref::<ArmadilloEntity>()
}

// armadillo: ARMADILLO_ROLLING_OUT
fn rolling_out() -> OneShot {
    OneShot::new(
        "ArmadilloRollingOut",
        vec![(
            types::DANGER_DETECTED_RECENTLY.id(),
            MemoryStatus::ValueAbsent,
        )],
        |tick| {
            let Some(armadillo) = armadillo_of(tick.mob) else {
                return false;
            };
            if armadillo.is_scared() {
                armadillo.roll_out();
                true
            } else {
                false
            }
        },
    )
}

// armadillo: ArmadilloAi.ArmadilloPanic, roll out before panicking
struct ArmadilloPanic(AnimalPanic);

impl Behavior for ArmadilloPanic {
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

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(armadillo) = armadillo_of(tick.mob) {
            armadillo.roll_out();
        }
        self.0.start(tick);
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        self.0.can_still_use(tick)
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        self.0.stop(tick);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        self.0.tick(tick);
    }

    fn debug_name(&self) -> &'static str {
        "AnimalPanic"
    }
}

// armadillo: MoveToTargetSink.checkExtraStartConditions, not while rolled up
struct ArmadilloMoveToTargetSink(MoveToTargetSink);

impl Behavior for ArmadilloMoveToTargetSink {
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
        !armadillo_of(tick.mob).is_some_and(ArmadilloEntity::is_scared)
            && self.0.check_extra_start_conditions(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.0.start(tick);
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        self.0.can_still_use(tick)
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        self.0.stop(tick);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        self.0.tick(tick);
    }

    fn debug_name(&self) -> &'static str {
        "MoveToTargetSink"
    }
}

// armadillo: ArmadilloAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(Timed::new(ArmadilloPanic(
                    AnimalPanic::new(SPEED_MULTIPLIER_WHEN_PANICKING)
                        .with_causes(&tag::DamageType::MINECRAFT_PANIC_ENVIRONMENTAL_CAUSES),
                ))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(ArmadilloMoveToTargetSink(
                    MoveToTargetSink::default(),
                ))),
                Box::new(CountDownCooldownTicks::new(
                    types::TEMPTATION_COOLDOWN_TICKS,
                )),
                Box::new(CountDownCooldownTicks::new(types::GAZE_COOLDOWN_TICKS)),
                Box::new(rolling_out()),
            ],
        ),
        ActivityData::with_priority_start(
            Activity::Idle,
            0,
            vec![
                Box::new(set_entity_look_target_sometimes::create_for_type(
                    &EntityType::PLAYER,
                    6.0,
                    30,
                    60,
                )),
                Box::new(Timed::new(
                    AnimalMakeLove::new(&EntityType::ARMADILLO, |_| {})
                        .with_close_enough_distance(1),
                )),
                Box::new(run_one(vec![
                    (
                        Box::new(Timed::new(
                            FollowTemptation::new(
                                SPEED_MULTIPLIER_WHEN_TEMPTED,
                                DEFAULT_CLOSE_ENOUGH_DIST,
                                false,
                            )
                            .with_baby_distance(BABY_CLOSE_ENOUGH_DIST),
                        )),
                        1,
                    ),
                    (
                        Box::new(baby_follow_adult::create(
                            ADULT_FOLLOW_RANGE.0,
                            ADULT_FOLLOW_RANGE.1,
                            SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                            types::NEAREST_VISIBLE_ADULT,
                            false,
                            baby_follow_adult::same_entity,
                        )),
                        1,
                    ),
                ])),
                Box::new(Timed::new(RandomLookAround::new(150, 250, 30.0, 0.0, 0.0))),
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
                        (Box::new(DoNothing::new(30, 60)), 1),
                    ],
                )),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Panic,
            0,
            vec![Box::new(Timed::new(ArmadilloBallUp::default()))],
            vec![
                (
                    types::DANGER_DETECTED_RECENTLY.id(),
                    MemoryStatus::ValuePresent,
                ),
                (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
            ],
        ),
    ]
}

// armadillo: ArmadilloAi.ArmadilloBallUp
#[derive(Default)]
struct ArmadilloBallUp {
    next_peek_timer: i32,
    danger_was_around: bool,
}

impl ArmadilloBallUp {
    fn pick_next_peek_timer(armadillo: &ArmadilloEntity) -> i32 {
        ArmadilloState::Scared.animation_duration() as i32
            + armadillo.get_random().random_range(100..=400)
    }
}

impl Behavior for ArmadilloBallUp {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &[]
    }

    fn min_duration(&self) -> i32 {
        BALL_UP_STAY_IN_STATE
    }

    fn max_duration(&self) -> i32 {
        BALL_UP_STAY_IN_STATE
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        armadillo_of(tick.mob).is_some_and(|armadillo| {
            let entity = armadillo.get_entity();
            entity.on_ground.load(std::sync::atomic::Ordering::Relaxed) && !armadillo.is_in_liquid()
        })
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        armadillo_of(tick.mob).is_some_and(|armadillo| armadillo.get_state().is_threatened())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(armadillo) = armadillo_of(tick.mob) {
            armadillo.roll_up();
        }
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(armadillo) = armadillo_of(tick.mob) else {
            return;
        };
        if self.next_peek_timer > 0 {
            self.next_peek_timer -= 1;
        }
        let entity = armadillo.get_entity();
        let on_ground = entity.on_ground.load(std::sync::atomic::Ordering::Relaxed);
        if armadillo.should_switch_to_scared_state() {
            armadillo.switch_to_state(ArmadilloState::Scared);
            if on_ground {
                armadillo.play_entity_sound(Sound::EntityArmadilloLand, 1.0, 1.0);
            }
            return;
        }
        let state = armadillo.get_state();
        let danger_ticks = tick
            .state
            .time_until_expiry(types::DANGER_DETECTED_RECENTLY.id())
            .unwrap_or(0);
        let danger_is_around = danger_ticks > DANGER_DETECTED_RECENTLY_DANGER_THRESHOLD;
        if danger_is_around != self.danger_was_around {
            self.next_peek_timer = Self::pick_next_peek_timer(armadillo);
        }
        self.danger_was_around = danger_is_around;
        if state == ArmadilloState::Scared {
            if self.next_peek_timer == 0 && on_ground && danger_is_around {
                tick.world.send_entity_status(
                    entity,
                    EntityStatus::ArmadilloPeek,
                    ActorEventID::None,
                );
                self.next_peek_timer = Self::pick_next_peek_timer(armadillo);
            }
            if danger_ticks < ArmadilloState::Unrolling.animation_duration() as i64 {
                armadillo.play_entity_sound(Sound::EntityArmadilloUnrollStart, 1.0, 1.0);
                armadillo.switch_to_state(ArmadilloState::Unrolling);
            }
        } else if state == ArmadilloState::Unrolling
            && danger_ticks > ArmadilloState::Unrolling.animation_duration() as i64
        {
            armadillo.switch_to_state(ArmadilloState::Scared);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(armadillo) = armadillo_of(tick.mob)
            && !armadillo.can_stay_rolled_up_in(tick.state)
        {
            armadillo.roll_out();
        }
    }

    fn debug_name(&self) -> &'static str {
        "ArmadilloBallUp"
    }
}
