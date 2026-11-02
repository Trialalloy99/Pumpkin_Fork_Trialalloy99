use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;

use super::{SnifferEntity, SnifferState};
use crate::entity::ageable::AgeableMob;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        Behavior, CountDownCooldownTicks, DoNothing, FollowTemptation, LookAtTargetSink,
        MoveToTargetSink, Swim, Timed, animal_make_love::AnimalMakeLove, random_stroll, run_one,
        set_entity_look_target, set_walk_target_from_look_target,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types, walk_target::WalkTarget},
    sensing::SensorType,
};
use crate::entity::mob::Mob;

const SNIFFING_COOLDOWN_TICKS: i64 = 9600;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_SNIFFING: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const MAX_LOOK_DISTANCE: f32 = 6.0;
const SEARCHING_DURATION: i32 = 600;

const MEMORIES: [MemoryModuleId; 9] = [
    types::SNIFFER_EXPLORED_POSITIONS.id(),
    types::SNIFFER_SNIFFING_TARGET.id(),
    types::SNIFFER_DIGGING.id(),
    types::SNIFFER_HAPPY.id(),
    types::SNIFF_COOLDOWN.id(),
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::BREED_TARGET.id(),
    types::IS_PANICKING.id(),
    types::TEMPTING_PLAYER.id(),
];

// sniffer: Brain.provider(SNIFFER_EXPLORED_POSITIONS; NEAREST_LIVING_ENTITIES, HURT_BY, NEAREST_PLAYERS, FOOD_TEMPTATIONS)
pub(super) fn make_brain(sniffer: &SnifferEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::HurtBy,
            SensorType::NearestPlayers,
            SensorType::FoodTemptations,
        ],
        activities,
    }
    .make_brain(sniffer, packed)
}

// sniffer: SnifferAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Dig, Activity::Sniff, Activity::Idle]);
}

fn sniffer_of(mob: &dyn Mob) -> Option<&SnifferEntity> {
    mob.cast_any().downcast_ref::<SnifferEntity>()
}

// sniffer: SnifferAi.resetSniffing from a start hook
fn reset_sniffing(tick: &mut BrainTick<'_>) {
    if let Some(sniffer) = sniffer_of(tick.mob) {
        sniffer.reset_sniffing(tick.state);
    }
}

// sniffer: AnimalPanic start override
struct SnifferPanic(crate::entity::ai::brain::behavior::AnimalPanic);

impl Behavior for SnifferPanic {
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
        reset_sniffing(tick);
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

// sniffer: SnifferAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(Timed::new(SnifferPanic(
                    crate::entity::ai::brain::behavior::AnimalPanic::new(
                        SPEED_MULTIPLIER_WHEN_PANICKING,
                    ),
                ))),
                Box::new(Timed::new(MoveToTargetSink::new(500, 700))),
                Box::new(CountDownCooldownTicks::new(
                    types::TEMPTATION_COOLDOWN_TICKS,
                )),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Sniff,
            0,
            vec![Box::new(Timed::new(Searching))],
            vec![
                (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
                (
                    types::SNIFFER_SNIFFING_TARGET.id(),
                    MemoryStatus::ValuePresent,
                ),
                (types::WALK_TARGET.id(), MemoryStatus::ValuePresent),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Dig,
            0,
            vec![
                Box::new(Timed::new(Digging)),
                Box::new(Timed::new(FinishedDigging)),
            ],
            vec![
                (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
                (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
                (types::SNIFFER_DIGGING.id(), MemoryStatus::ValuePresent),
            ],
        ),
        ActivityData::with_priority_start_and_conditions(
            Activity::Idle,
            0,
            vec![
                Box::new(Timed::new(AnimalMakeLove::new(
                    &EntityType::SNIFFER,
                    reset_sniffing,
                ))),
                Box::new(Timed::new(
                    FollowTemptation::new(SPEED_MULTIPLIER_WHEN_TEMPTED, 3.5, false)
                        .with_baby_distance(2.5)
                        .with_on_start(reset_sniffing),
                )),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(FeelingHappy)),
                Box::new(run_one(vec![
                    (
                        Box::new(set_walk_target_from_look_target::create(
                            SPEED_MULTIPLIER_WHEN_IDLING,
                            3,
                        )),
                        2,
                    ),
                    (Box::new(Timed::new(Scenting)), 1),
                    (Box::new(Timed::new(Sniffing)), 1),
                    (
                        Box::new(set_entity_look_target::create_for_type(
                            &EntityType::PLAYER,
                            MAX_LOOK_DISTANCE,
                        )),
                        1,
                    ),
                    (
                        Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                        1,
                    ),
                    (Box::new(DoNothing::new(5, 20)), 2),
                ])),
            ],
            vec![(types::SNIFFER_DIGGING.id(), MemoryStatus::ValueAbsent)],
        ),
    ]
}

// sniffer: SnifferAi.Digging
struct Digging;

const DIGGING_ENTRY: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::SNIFFER_DIGGING.id(), MemoryStatus::ValuePresent),
    (types::SNIFF_COOLDOWN.id(), MemoryStatus::ValueAbsent),
];

impl Behavior for Digging {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &DIGGING_ENTRY
    }

    fn min_duration(&self) -> i32 {
        160
    }

    fn max_duration(&self) -> i32 {
        180
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        sniffer_of(tick.mob).is_some_and(|sniffer| sniffer.can_sniff(tick.state))
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::SNIFFER_DIGGING.id())
            && sniffer_of(tick.mob).is_some_and(|sniffer| {
                sniffer.can_dig(tick.state) && !sniffer.mob_entity.is_in_love()
            })
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Digging);
        }
    }

    fn stop_timed(&mut self, tick: &mut BrainTick<'_>, timed_out: bool) {
        if timed_out {
            tick.state
                .set_with_expiry(types::SNIFF_COOLDOWN, (), SNIFFING_COOLDOWN_TICKS);
        } else {
            reset_sniffing(tick);
        }
    }

    fn debug_name(&self) -> &'static str {
        "SnifferDigging"
    }
}

// sniffer: SnifferAi.FeelingHappy
struct FeelingHappy;

const FEELING_HAPPY_ENTRY: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::SNIFFER_HAPPY.id(), MemoryStatus::ValuePresent)];

impl Behavior for FeelingHappy {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &FEELING_HAPPY_ENTRY
    }

    fn min_duration(&self) -> i32 {
        40
    }

    fn max_duration(&self) -> i32 {
        100
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::FeelingHappy);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Idling);
        }
        tick.state.erase(types::SNIFFER_HAPPY.id());
    }

    fn debug_name(&self) -> &'static str {
        "SnifferFeelingHappy"
    }
}

// sniffer: SnifferAi.FinishedDigging
struct FinishedDigging;

const FINISHED_DIGGING_ENTRY: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::SNIFFER_DIGGING.id(), MemoryStatus::ValuePresent),
    (types::SNIFF_COOLDOWN.id(), MemoryStatus::ValuePresent),
];

impl Behavior for FinishedDigging {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &FINISHED_DIGGING_ENTRY
    }

    fn min_duration(&self) -> i32 {
        40
    }

    fn max_duration(&self) -> i32 {
        40
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::SNIFFER_DIGGING.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Rising);
        }
    }

    fn stop_timed(&mut self, tick: &mut BrainTick<'_>, timed_out: bool) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Idling);
            sniffer.on_digging_complete(tick.state, timed_out);
        }
        tick.state.erase(types::SNIFFER_DIGGING.id());
        tick.state.set(types::SNIFFER_HAPPY, true);
    }

    fn debug_name(&self) -> &'static str {
        "SnifferFinishedDigging"
    }
}

// sniffer: SnifferAi.Scenting
struct Scenting;

const SCENTING_ENTRY: [(MemoryModuleId, MemoryStatus); 5] = [
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
    (types::SNIFFER_DIGGING.id(), MemoryStatus::ValueAbsent),
    (
        types::SNIFFER_SNIFFING_TARGET.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::SNIFFER_HAPPY.id(), MemoryStatus::ValueAbsent),
    (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
];

impl Behavior for Scenting {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SCENTING_ENTRY
    }

    fn min_duration(&self) -> i32 {
        40
    }

    fn max_duration(&self) -> i32 {
        80
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        !SnifferEntity::is_tempted(tick.state)
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Scenting);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Idling);
        }
    }

    fn debug_name(&self) -> &'static str {
        "SnifferScenting"
    }
}

// sniffer: SnifferAi.Searching
struct Searching;

const SEARCHING_ENTRY: [(MemoryModuleId, MemoryStatus); 3] = [
    (types::WALK_TARGET.id(), MemoryStatus::ValuePresent),
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
    (
        types::SNIFFER_SNIFFING_TARGET.id(),
        MemoryStatus::ValuePresent,
    ),
];

impl Behavior for Searching {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SEARCHING_ENTRY
    }

    fn min_duration(&self) -> i32 {
        SEARCHING_DURATION
    }

    fn max_duration(&self) -> i32 {
        SEARCHING_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        sniffer_of(tick.mob).is_some_and(|sniffer| sniffer.can_sniff(tick.state))
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(sniffer) = sniffer_of(tick.mob) else {
            return false;
        };
        if !sniffer.can_sniff(tick.state) {
            sniffer.transition_to(SnifferState::Idling);
            return false;
        }
        let walk_target = tick
            .state
            .get(types::WALK_TARGET)
            .map(|walk| walk.target().current_block_position());
        let sniffing_target = tick.state.get(types::SNIFFER_SNIFFING_TARGET).copied();
        matches!((walk_target, sniffing_target), (Some(walk), Some(target)) if walk == target)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Searching);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob)
            && sniffer.can_dig(tick.state)
            && sniffer.can_sniff(tick.state)
        {
            tick.state.set(types::SNIFFER_DIGGING, true);
        }
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::SNIFFER_SNIFFING_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "SnifferSearching"
    }
}

// sniffer: SnifferAi.Sniffing
struct Sniffing;

const SNIFFING_ENTRY: [(MemoryModuleId, MemoryStatus); 3] = [
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (
        types::SNIFFER_SNIFFING_TARGET.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::SNIFF_COOLDOWN.id(), MemoryStatus::ValueAbsent),
];

impl Behavior for Sniffing {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SNIFFING_ENTRY
    }

    fn min_duration(&self) -> i32 {
        40
    }

    fn max_duration(&self) -> i32 {
        80
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        sniffer_of(tick.mob)
            .is_some_and(|sniffer| !sniffer.is_baby() && sniffer.can_sniff(tick.state))
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        sniffer_of(tick.mob).is_some_and(|sniffer| sniffer.can_sniff(tick.state))
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(sniffer) = sniffer_of(tick.mob) {
            sniffer.transition_to(SnifferState::Sniffing);
        }
    }

    fn stop_timed(&mut self, tick: &mut BrainTick<'_>, timed_out: bool) {
        let Some(sniffer) = sniffer_of(tick.mob) else {
            return;
        };
        sniffer.transition_to(SnifferState::Idling);
        if timed_out && let Some(position) = sniffer.calculate_dig_position(tick.state) {
            tick.state.set(types::SNIFFER_SNIFFING_TARGET, position);
            tick.state.set(
                types::WALK_TARGET,
                WalkTarget::from_block_pos(position, SPEED_MULTIPLIER_WHEN_SNIFFING, 0),
            );
        }
    }

    fn debug_name(&self) -> &'static str {
        "SnifferSniffing"
    }
}
