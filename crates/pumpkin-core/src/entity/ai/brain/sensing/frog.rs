use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::{Sensor, is_entity_attackable};
use crate::entity::EntityBase;
use crate::entity::passive::frog::FrogEntity;

const ATTACKABLES_REQUIRES: [MemoryModuleId; 3] = [
    types::NEAREST_ATTACKABLE.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::UNREACHABLE_TONGUE_TARGETS.id(),
];
const TARGET_DETECTION_DISTANCE: f64 = 10.0;

// sensor: FrogAttackablesSensor (NearestVisibleLivingEntitySensor)
pub struct FrogAttackablesSensor;

impl Sensor for FrogAttackablesSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &ATTACKABLES_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let ctx = tick.visibility();
        let body_pos = tick.mob.get_entity().pos.load();
        let unreachable = tick
            .state
            .get(types::UNREACHABLE_TONGUE_TARGETS)
            .cloned()
            .unwrap_or_default();
        let matching = |mob: &dyn EntityBase| {
            let entity = mob.get_entity();
            is_entity_attackable(&ctx, mob)
                && FrogEntity::can_eat(mob)
                && !unreachable.contains(&entity.entity_uuid)
                && body_pos.squared_distance_to_vec(&entity.pos.load())
                    < TARGET_DETECTION_DISTANCE * TARGET_DETECTION_DISTANCE
        };
        let found = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .and_then(|visible| visible.find_closest(&ctx, |entity| matching(entity.as_ref())));
        tick.state.set_optional(types::NEAREST_ATTACKABLE, found);
    }
}

const IN_WATER_REQUIRES: [MemoryModuleId; 1] = [types::IS_IN_WATER.id()];

// sensor: IsInWaterSensor
pub struct IsInWaterSensor;

impl Sensor for IsInWaterSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &IN_WATER_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        if tick.mob.get_entity().is_in_water() {
            tick.state.set(types::IS_IN_WATER, ());
        } else {
            tick.state.erase(types::IS_IN_WATER.id());
        }
    }
}
