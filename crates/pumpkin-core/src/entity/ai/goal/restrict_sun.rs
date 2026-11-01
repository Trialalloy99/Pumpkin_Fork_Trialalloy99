use super::{Controls, Goal};
use crate::entity::mob::Mob;
use pumpkin_data::data_component_impl::EquipmentSlot;

// restrict sun: avoid sky-lit path nodes by day, bare head (RestrictSunGoal)
#[derive(Default)]
pub struct RestrictSunGoal;

impl RestrictSunGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn applies(mob: &dyn Mob) -> bool {
        mob.get_entity().world.load().is_bright_outside()
            && mob
                .get_mob_entity()
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&EquipmentSlot::HEAD)
                .is_empty()
    }

    fn set_avoid_sun(mob: &dyn Mob, avoid_sun: bool) {
        mob.get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_avoid_sun(avoid_sun);
    }
}

impl Goal for RestrictSunGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        Self::applies(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        Self::applies(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        Self::set_avoid_sun(mob, true);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        Self::set_avoid_sun(mob, false);
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}
