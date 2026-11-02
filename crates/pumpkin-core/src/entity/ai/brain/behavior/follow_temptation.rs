use std::sync::Arc;

use super::super::BrainTick;
use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::timed::Behavior;
use crate::entity::EntityBase;

const TEMPTATION_COOLDOWN: i32 = 100;
const NEVER_TIMES_OUT: i32 = i32::MAX - 1;

const ENTRY_CONDITIONS: [(MemoryModuleId, MemoryStatus); 6] = [
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::WALK_TARGET.id(), MemoryStatus::Registered),
    (
        types::TEMPTATION_COOLDOWN_TICKS.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::TEMPTING_PLAYER.id(), MemoryStatus::ValuePresent),
    (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
];

// brain: FollowTemptation
pub struct FollowTemptation {
    speed_modifier: f32,
    close_enough_distance: f64,
    look_in_the_eyes: bool,
    baby_close_enough_distance: Option<f64>,
    on_start: Option<fn(&mut BrainTick<'_>)>,
}

impl FollowTemptation {
    #[must_use]
    pub const fn new(
        speed_modifier: f32,
        close_enough_distance: f64,
        look_in_the_eyes: bool,
    ) -> Self {
        Self {
            speed_modifier,
            close_enough_distance,
            look_in_the_eyes,
            baby_close_enough_distance: None,
            on_start: None,
        }
    }

    // brain: FollowTemptation closeEnoughDistance by age
    #[must_use]
    pub const fn with_baby_distance(mut self, distance: f64) -> Self {
        self.baby_close_enough_distance = Some(distance);
        self
    }

    // brain: FollowTemptation.start override
    #[must_use]
    pub const fn with_on_start(mut self, on_start: fn(&mut BrainTick<'_>)) -> Self {
        self.on_start = Some(on_start);
        self
    }
}

impl Behavior for FollowTemptation {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ENTRY_CONDITIONS
    }

    // brain: FollowTemptation.timedOut is always false
    fn min_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn max_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(on_start) = self.on_start {
            on_start(tick);
        }
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::TEMPTING_PLAYER.id())
            && !tick.state.has_memory_value(types::BREED_TARGET.id())
            && !tick.state.has_memory_value(types::IS_PANICKING.id())
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state
            .set(types::TEMPTATION_COOLDOWN_TICKS, TEMPTATION_COOLDOWN);
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::LOOK_TARGET.id());
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(player) = tick.state.get(types::TEMPTING_PLAYER).cloned() else {
            return;
        };
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(player.clone(), true)),
        );
        let own = tick.mob.get_entity().pos.load();
        let distance_sq = own.squared_distance_to_vec(&player.get_entity().pos.load());
        let close = match self.baby_close_enough_distance {
            Some(baby) if tick.mob.get_mob_entity().living_entity.is_baby() => baby,
            _ => self.close_enough_distance,
        };
        if distance_sq < close * close {
            tick.state.erase(types::WALK_TARGET.id());
        } else {
            tick.state.set(
                types::WALK_TARGET,
                WalkTarget::new(
                    Arc::new(EntityTracker::with_target_eye_height(
                        player,
                        self.look_in_the_eyes,
                        self.look_in_the_eyes,
                    )),
                    self.speed_modifier,
                    2,
                ),
            );
        }
    }

    fn debug_name(&self) -> &'static str {
        "FollowTemptation"
    }
}
