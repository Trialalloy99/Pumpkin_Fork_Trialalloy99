use std::sync::atomic::Ordering;

use super::{Controls, Goal, GoalReach};
use crate::entity::mob::Mob;
use rand::RngExt;

pub struct SwimGoal {
    goal_control: Controls,
    // fox: FoxFloatGoal.canUse, isInFluidDeeperThan(0.25)
    fluid_threshold: Option<f64>,
}

impl Default for SwimGoal {
    fn default() -> Self {
        Self {
            goal_control: Controls::JUMP,
            fluid_threshold: None,
        }
    }
}

impl SwimGoal {
    #[must_use]
    pub const fn with_fluid_threshold(mut self, threshold: f64) -> Self {
        self.fluid_threshold = Some(threshold);
        self
    }

    fn is_in_fluid(&self, mob: &dyn Mob) -> bool {
        let living = &mob.get_mob_entity().living_entity;
        let entity = &living.entity;
        let threshold = self
            .fluid_threshold
            .unwrap_or_else(|| living.get_swim_height());
        let in_water =
            entity.touching_water.load(Ordering::SeqCst) && entity.water_height.load() > threshold;
        in_water || entity.touching_lava.load(Ordering::SeqCst)
    }
}

impl Goal for SwimGoal {
    fn tick_reach(&self, _mob: &dyn Mob, _running: bool) -> Option<GoalReach> {
        Some(GoalReach::default())
    }

    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        mob.get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_float(true);
        self.is_in_fluid(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.is_in_fluid(mob)
    }

    fn tick(&mut self, mob: &dyn Mob) {
        if mob.get_random().random::<f32>() < 0.8 {
            mob.get_mob_entity()
                .jump_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .jump();
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}
