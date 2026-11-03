//! Ported from the villager sensors: `VillagerHostilesSensor`, `VillagerBabiesSensor`,
//! `SecondaryPoiSensor`, `GolemSensor` and `NearestBedSensor`.

use std::sync::Arc;

use pumpkin_data::entity::EntityType;
use pumpkin_util::math::position::BlockPos;

use super::super::BrainTick;
use super::super::memory::{GlobalPos, MemoryModuleId, types};
use super::Sensor;
use crate::entity::EntityBase;
use crate::entity::passive::villager::VillagerEntity;
use crate::world::poi_manager::PoiOccupancy;

const HOSTILE_DISTANCES: [(&EntityType, f64); 11] = [
    (&EntityType::DROWNED, 8.0),
    (&EntityType::EVOKER, 12.0),
    (&EntityType::HUSK, 8.0),
    (&EntityType::ILLUSIONER, 12.0),
    (&EntityType::PILLAGER, 15.0),
    (&EntityType::RAVAGER, 12.0),
    (&EntityType::VEX, 8.0),
    (&EntityType::VINDICATOR, 10.0),
    (&EntityType::ZOGLIN, 10.0),
    (&EntityType::ZOMBIE, 8.0),
    (&EntityType::ZOMBIE_VILLAGER, 8.0),
];

const HOSTILES_REQUIRES: [MemoryModuleId; 2] = [
    types::NEAREST_HOSTILE.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
];

// sensor: VillagerHostilesSensor
pub struct VillagerHostilesSensor;

impl Sensor for VillagerHostilesSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &HOSTILES_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let position = tick.mob.get_entity().pos.load();
        let closest = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .and_then(|visible| {
                visible.find_closest(&tick.visibility(), |entity| {
                    let target = entity.get_entity();
                    HOSTILE_DISTANCES
                        .iter()
                        .find(|(entity_type, _)| *entity_type == target.entity_type)
                        .is_some_and(|(_, distance)| {
                            target.pos.load().squared_distance_to_vec(&position)
                                <= distance * distance
                        })
                })
            });
        tick.state.set_optional(types::NEAREST_HOSTILE, closest);
    }
}

const BABIES_REQUIRES: [MemoryModuleId; 1] = [types::VISIBLE_VILLAGER_BABIES.id()];

// sensor: VillagerBabiesSensor
pub struct VillagerBabiesSensor;

impl Sensor for VillagerBabiesSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &BABIES_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let babies: Vec<Arc<dyn EntityBase>> = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .map(|visible| {
                visible
                    .find_all(&tick.visibility(), |entity| {
                        entity.get_entity().entity_type == &EntityType::VILLAGER
                            && entity
                                .get_living_entity()
                                .is_some_and(crate::entity::living::LivingEntity::is_baby)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        tick.state.set(types::VISIBLE_VILLAGER_BABIES, babies);
    }
}

const SECONDARY_REQUIRES: [MemoryModuleId; 1] = [types::SECONDARY_JOB_SITE.id()];

// sensor: SecondaryPoiSensor (scan rate 40)
pub struct SecondaryPoiSensor;

impl Sensor for SecondaryPoiSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &SECONDARY_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(villager) = tick.mob.cast_any().downcast_ref::<VillagerEntity>() else {
            return;
        };
        let secondary =
            crate::entity::passive::villager::secondary_poi_block(villager.profession());
        let Some(secondary) = secondary else {
            tick.state.erase(types::SECONDARY_JOB_SITE.id());
            return;
        };
        let center = tick.mob.get_entity().block_pos.load();
        let mut sites: Vec<GlobalPos> = Vec::new();
        for x in -4..=4 {
            for y in -2..=2 {
                for z in -4..=4 {
                    let test = BlockPos::new(center.0.x + x, center.0.y + y, center.0.z + z);
                    if tick.world.get_block(&test) == secondary {
                        sites.extend(GlobalPos::of(tick.world, test));
                    }
                }
            }
        }
        if sites.is_empty() {
            tick.state.erase(types::SECONDARY_JOB_SITE.id());
        } else {
            tick.state.set(types::SECONDARY_JOB_SITE, sites);
        }
    }
}

const GOLEM_REQUIRES: [MemoryModuleId; 2] = [
    types::NEAREST_LIVING_ENTITIES.id(),
    types::GOLEM_DETECTED_RECENTLY.id(),
];
const GOLEM_MEMORY_TIME_TO_LIVE: i64 = 599;

// sensor: GolemSensor (scan rate 200)
pub struct GolemSensor;

impl GolemSensor {
    // sensor: GolemSensor.golemDetected
    pub fn golem_detected(state: &mut super::super::BrainState) {
        state.set_with_expiry(
            types::GOLEM_DETECTED_RECENTLY,
            true,
            GOLEM_MEMORY_TIME_TO_LIVE,
        );
    }
}

impl Sensor for GolemSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &GOLEM_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let golem_present =
            tick.state
                .get(types::NEAREST_LIVING_ENTITIES)
                .is_some_and(|entities| {
                    entities
                        .iter()
                        .any(|entity| entity.get_entity().entity_type == &EntityType::IRON_GOLEM)
                });
        if golem_present {
            Self::golem_detected(tick.state);
        }
    }
}

const BED_REQUIRES: [MemoryModuleId; 1] = [types::NEAREST_BED.id()];
const BED_SCAN_RANGE: i32 = 48;

// sensor: NearestBedSensor, babies only
pub struct NearestBedSensor;

impl Sensor for NearestBedSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &BED_REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        if !tick.mob.is_baby_mob() {
            return;
        }
        let center = tick.mob.get_entity().block_pos.load();
        let beds: Vec<BlockPos> = tick
            .world
            .poi_find_all_closest_first(
                center,
                BED_SCAN_RANGE,
                |poi_type| poi_type == "minecraft:home",
                PoiOccupancy::Any,
            )
            .into_iter()
            .take(5)
            .map(|(pos, _)| pos)
            .collect();
        if beds.is_empty() {
            return;
        }
        let path = {
            let mob = tick.mob.get_mob_entity();
            mob.navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_path_to_targets(&mob.living_entity, &beds, 1)
        };
        if let Some(path) = path
            && path.can_reach()
        {
            tick.state.set(types::NEAREST_BED, path.get_target());
        }
    }
}
