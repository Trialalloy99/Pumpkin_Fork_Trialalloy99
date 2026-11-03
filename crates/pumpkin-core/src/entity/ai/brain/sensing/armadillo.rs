use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::Sensor;
use crate::entity::passive::armadillo::ArmadilloEntity;

const REQUIRES: [MemoryModuleId; 1] = [types::NEAREST_LIVING_ENTITIES.id()];
const MEMORY_TIME_TO_LIVE: i64 = 80;

// sensor: MobSensor(5, Armadillo::isScaredBy, Armadillo::canStayRolledUp, DANGER_DETECTED_RECENTLY, 80)
pub struct ArmadilloScareSensor;

impl Sensor for ArmadilloScareSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(armadillo) = tick.mob.cast_any().downcast_ref::<ArmadilloEntity>() else {
            return;
        };
        if !armadillo.can_stay_rolled_up_in(tick.state) {
            tick.state.erase(types::DANGER_DETECTED_RECENTLY.id());
            return;
        }
        let scared = tick
            .state
            .get(types::NEAREST_LIVING_ENTITIES)
            .map(|nearby| {
                nearby
                    .iter()
                    .any(|entity| armadillo.is_scared_by(entity.as_ref()))
            });
        if scared == Some(true) {
            tick.state
                .set_with_expiry(types::DANGER_DETECTED_RECENTLY, true, MEMORY_TIME_TO_LIVE);
        }
    }
}
