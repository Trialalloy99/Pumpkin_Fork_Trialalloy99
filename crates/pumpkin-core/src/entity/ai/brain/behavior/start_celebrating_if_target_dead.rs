use pumpkin_data::entity::EntityType;

use super::super::BrainTick;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;

// brain: StartCelebratingIfTargetDead.create
#[must_use]
pub fn create(
    celebrate_duration: i64,
    dance_predicate: fn(&BrainTick<'_>, &dyn EntityBase) -> bool,
) -> OneShot {
    OneShot::new(
        "StartCelebratingIfTargetDead",
        vec![
            (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
            (types::ANGRY_AT.id(), MemoryStatus::Registered),
            (types::CELEBRATE_LOCATION.id(), MemoryStatus::ValueAbsent),
            (types::DANCING.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            if !target
                .get_living_entity()
                .is_some_and(crate::entity::living::LivingEntity::is_dead_or_dying)
            {
                return false;
            }
            if dance_predicate(tick, target.as_ref()) {
                tick.state
                    .set_with_expiry(types::DANCING, true, celebrate_duration);
            }
            tick.state.set_with_expiry(
                types::CELEBRATE_LOCATION,
                target.get_entity().block_pos.load(),
                celebrate_duration,
            );
            if target.get_entity().entity_type != &EntityType::PLAYER
                || tick.world.level_info.load().game_rules.forgive_dead_players
            {
                tick.state.erase(types::ATTACK_TARGET.id());
                tick.state.erase(types::ANGRY_AT.id());
            }
            true
        },
    )
}
