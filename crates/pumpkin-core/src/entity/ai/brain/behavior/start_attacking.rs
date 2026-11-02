use std::sync::Arc;

use super::super::BrainTick;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;

// brain: StartAttacking.create
#[must_use]
pub fn create(
    can_attack: impl Fn(&BrainTick<'_>) -> bool + Send + Sync + 'static,
    find_target: impl Fn(&BrainTick<'_>) -> Option<Arc<dyn EntityBase>> + Send + Sync + 'static,
) -> OneShot {
    OneShot::with_required(
        "StartAttacking",
        vec![(types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent)],
        vec![types::CANT_REACH_WALK_TARGET_SINCE.id()],
        move |tick| {
            if !can_attack(tick) {
                return false;
            }
            let Some(target) = find_target(tick) else {
                return false;
            };
            if !tick.mob.can_attack(target.as_ref()) {
                return false;
            }
            tick.state.set(types::ATTACK_TARGET, target);
            tick.state.erase(types::CANT_REACH_WALK_TARGET_SINCE.id());
            true
        },
    )
}
