use pumpkin_data::tag::{self, Taggable};

use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::Sensor;
use crate::entity::EntityBase;

const REQUIRES: [MemoryModuleId; 2] = [
    types::NEAREST_VISIBLE_ADULT.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
];

// sensor: AdultSensorAnyType
pub struct AdultSensorAnyType;

fn is_adult(entity: &dyn EntityBase) -> bool {
    entity
        .get_entity()
        .entity_type
        .has_tag(&tag::EntityType::MINECRAFT_FOLLOWABLE_FRIENDLY_MOBS)
        && !entity
            .get_living_entity()
            .is_some_and(crate::entity::living::LivingEntity::is_baby)
}

impl Sensor for AdultSensorAnyType {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let adult = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .and_then(|visible| {
                visible.find_closest(&tick.visibility(), |entity| is_adult(entity.as_ref()))
            });
        tick.state.set_optional(types::NEAREST_VISIBLE_ADULT, adult);
    }
}
