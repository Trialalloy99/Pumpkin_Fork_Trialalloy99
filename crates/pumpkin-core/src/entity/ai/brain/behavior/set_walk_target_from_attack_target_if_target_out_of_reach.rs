use std::sync::Arc;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;

// brain: SetWalkTargetFromAttackTargetIfTargetOutOfReach.create
#[must_use]
pub fn create(speed_modifier: f32) -> OneShot {
    OneShot::new(
        "SetWalkTargetFromAttackTargetIfTargetOutOfReach",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::Registered,
            ),
        ],
        move |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            let target_id = target.get_entity().entity_id;
            let visible = tick
                .state
                .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                .is_some_and(|entities| {
                    entities
                        .nearby()
                        .iter()
                        .any(|entity| entity.get_entity().entity_id == target_id)
                });
            // BehaviorUtils.isWithinAttackRange, melee only
            if visible
                && tick
                    .mob
                    .get_mob_entity()
                    .is_in_attack_range(target.as_ref())
            {
                tick.state.erase(types::WALK_TARGET.id());
            } else {
                tick.state.set(
                    types::LOOK_TARGET,
                    Arc::new(EntityTracker::new(target.clone(), true)),
                );
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(
                        Arc::new(EntityTracker::new(target, false)),
                        speed_modifier,
                        0,
                    ),
                );
            }
            true
        },
    )
}
