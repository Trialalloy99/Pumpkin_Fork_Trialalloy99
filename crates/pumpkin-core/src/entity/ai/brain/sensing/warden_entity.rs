use std::sync::Arc;

use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::{NearestLivingEntitySensor, Sensor};
use crate::entity::EntityBase;
use crate::entity::mob::warden::WardenEntity;

const REQUIRES: [MemoryModuleId; 3] = [
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::NEAREST_ATTACKABLE.id(),
];

// sensor: WardenEntitySensor
pub struct WardenEntitySensor;

impl Sensor for WardenEntitySensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        NearestLivingEntitySensor.do_tick(tick);
        let Some(warden) = tick.mob.cast_any().downcast_ref::<WardenEntity>() else {
            return;
        };
        let nearby: Vec<Arc<dyn EntityBase>> = tick
            .state
            .get(types::NEAREST_LIVING_ENTITIES)
            .cloned()
            .unwrap_or_default();
        let closest = |is_player: bool| {
            nearby
                .iter()
                .filter(|entity| warden.can_target_entity(entity.as_ref()))
                .find(|entity| entity.get_player().is_some() == is_player)
                .cloned()
        };
        match closest(true).or_else(|| closest(false)) {
            Some(entity) => tick.state.set(types::NEAREST_ATTACKABLE, entity),
            None => tick.state.erase(types::NEAREST_ATTACKABLE.id()),
        }
    }
}
