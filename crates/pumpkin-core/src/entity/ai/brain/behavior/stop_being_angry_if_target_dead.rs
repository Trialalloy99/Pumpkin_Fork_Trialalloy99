use pumpkin_data::entity::EntityType;

use super::super::memory::{MemoryStatus, types};
use super::OneShot;

// brain: StopBeingAngryIfTargetDead.create
#[must_use]
pub fn create() -> OneShot {
    OneShot::new(
        "StopBeingAngryIfTargetDead",
        vec![(types::ANGRY_AT.id(), MemoryStatus::ValuePresent)],
        move |tick| {
            let Some(uuid) = tick.state.get(types::ANGRY_AT).copied() else {
                return true;
            };
            let dead_target = tick
                .world
                .get_entity_by_uuid(uuid)
                .filter(|entity| {
                    entity
                        .get_living_entity()
                        .is_some_and(crate::entity::living::LivingEntity::is_dead_or_dying)
                })
                .filter(|entity| {
                    entity.get_entity().entity_type != &EntityType::PLAYER
                        || tick.world.level_info.load().game_rules.forgive_dead_players
                });
            if dead_target.is_some() {
                tick.state.erase(types::ANGRY_AT.id());
            }
            true
        },
    )
}
