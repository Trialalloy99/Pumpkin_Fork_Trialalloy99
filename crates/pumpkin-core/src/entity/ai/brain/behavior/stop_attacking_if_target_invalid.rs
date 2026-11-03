use super::super::BrainTick;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;

const TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE: i64 = 200;

// brain: StopAttackingIfTargetInvalid.create
#[must_use]
pub fn create(
    stop_attacking_when: impl FnMut(&BrainTick<'_>, &dyn EntityBase) -> bool + Send + Sync + 'static,
) -> OneShot {
    create_with(stop_attacking_when, |_, _| {}, true)
}

// brain: StopAttackingIfTargetInvalid.create(stopAttackingWhen, onTargetErased, canGrowTired)
#[must_use]
pub fn create_with(
    mut stop_attacking_when: impl FnMut(&BrainTick<'_>, &dyn EntityBase) -> bool + Send + Sync + 'static,
    mut on_target_erased: impl FnMut(&mut BrainTick<'_>, &dyn EntityBase) + Send + Sync + 'static,
    can_grow_tired: bool,
) -> OneShot {
    OneShot::with_required(
        "StopAttackingIfTargetInvalid",
        vec![(types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent)],
        vec![types::CANT_REACH_WALK_TARGET_SINCE.id()],
        move |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            let tired = can_grow_tired
                && tick
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
            on_target_erased(tick, target.as_ref());
            true
        },
    )
}
