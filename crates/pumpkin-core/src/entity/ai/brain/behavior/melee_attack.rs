use std::sync::Arc;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::mob::Mob;

// brain: MeleeAttack.create
#[must_use]
pub fn create(
    can_attack: impl Fn(&dyn Mob) -> bool + Send + Sync + 'static,
    cooldown_between_attacks: i64,
) -> OneShot {
    OneShot::new(
        "MeleeAttack",
        vec![
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
            (types::ATTACK_COOLING_DOWN.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
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
            let mob_entity = tick.mob.get_mob_entity();
            if !can_attack(tick.mob) || !mob_entity.is_in_attack_range(target.as_ref()) || !visible
            {
                return false;
            }
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(EntityTracker::new(target.clone(), true)),
            );
            mob_entity.living_entity.swing_hand();
            tick.mob.pre_attack(target.as_ref());
            if mob_entity.try_attack(tick.mob, target.as_ref()) {
                tick.mob.on_attack(target.as_ref());
            }
            tick.state
                .set_with_expiry(types::ATTACK_COOLING_DOWN, true, cooldown_between_attacks);
            true
        },
    )
}
