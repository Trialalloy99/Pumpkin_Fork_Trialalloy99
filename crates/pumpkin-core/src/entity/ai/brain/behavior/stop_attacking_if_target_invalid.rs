use super::super::BrainTick;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;

const TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE: i64 = 200;

// brain: StopAttackingIfTargetInvalid.create
#[must_use]
pub fn create(
    mut stop_attacking_when: impl FnMut(&BrainTick<'_>, &dyn EntityBase) -> bool + Send + Sync + 'static,
) -> OneShot {
    OneShot::with_required(
        "StopAttackingIfTargetInvalid",
        vec![(types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent)],
        vec![types::CANT_REACH_WALK_TARGET_SINCE.id()],
        move |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            let tired = tick
                .state
                .get(types::CANT_REACH_WALK_TARGET_SINCE)
                .is_some_and(|since| tick.time - since > TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE);
            if tick.mob.can_attack(target.as_ref())
                && !tired
                && target.get_entity().is_alive()
                && target.get_entity().world.load().uuid == tick.world.uuid
                && !stop_attacking_when(tick, target.as_ref())
            {
                return true;
            }
            tick.state.erase(types::ATTACK_TARGET.id());
            true
        },
    )
}
