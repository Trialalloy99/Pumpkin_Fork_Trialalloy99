use super::{Controls, Goal, GoalReach};
use crate::entity::ai::goal::revenge::MobFilter;
use crate::entity::mob::Mob;

/// Runs `inner` only while `gate` holds, e.g. the brain activity or the swim/land speed split.
pub struct GatedGoal<G: Goal> {
    inner: G,
    gate: MobFilter,
}

impl<G: Goal> GatedGoal<G> {
    #[must_use]
    pub const fn new(inner: G, gate: MobFilter) -> Self {
        Self { inner, gate }
    }
}

impl<G: Goal> Goal for GatedGoal<G> {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        (self.gate)(mob) && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        (self.gate)(mob) && self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn can_stop(&self) -> bool {
        self.inner.can_stop()
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }

    fn tick_reach(&self, _mob: &dyn Mob, _running: bool) -> Option<GoalReach> {
        None
    }
}
