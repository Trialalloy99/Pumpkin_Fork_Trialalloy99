use pumpkin_data::tag::{self, Taggable};

use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::{Sensor, is_entity_attackable};
use crate::entity::EntityBase;

const REQUIRES: [MemoryModuleId; 3] = [
    types::NEAREST_ATTACKABLE.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::HAS_HUNTING_COOLDOWN.id(),
];
const TARGET_DETECTION_DISTANCE_SQ: f64 = 64.0;

// sensor: AxolotlAttackablesSensor (NearestVisibleLivingEntitySensor)
pub struct AxolotlAttackablesSensor;

impl Sensor for AxolotlAttackablesSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let ctx = tick.visibility();
        let body_pos = tick.mob.get_entity().pos.load();
        let hunting = !tick
            .state
            .has_memory_value(types::HAS_HUNTING_COOLDOWN.id());
        let matching = |mob: &dyn EntityBase| {
            let entity = mob.get_entity();
            body_pos.squared_distance_to_vec(&entity.pos.load()) <= TARGET_DETECTION_DISTANCE_SQ
                && entity.is_in_water()
                && (entity
                    .entity_type
                    .has_tag(&tag::EntityType::MINECRAFT_AXOLOTL_ALWAYS_HOSTILES)
                    || (hunting
                        && entity
                            .entity_type
                            .has_tag(&tag::EntityType::MINECRAFT_AXOLOTL_HUNT_TARGETS)))
                && is_entity_attackable(&ctx, mob)
        };
        let found = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .and_then(|visible| visible.find_closest(&ctx, |entity| matching(entity.as_ref())));
        tick.state.set_optional(types::NEAREST_ATTACKABLE, found);
    }
}
