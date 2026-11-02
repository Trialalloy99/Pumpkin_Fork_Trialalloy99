use std::sync::Arc;

use pumpkin_data::entity::EntityType;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;

// brain: SetLookAndInteract.create
#[must_use]
pub fn create(entity_type: &'static EntityType, interaction_range: i32) -> OneShot {
    let range_sq = f64::from(interaction_range * interaction_range);
    OneShot::new(
        "SetLookAndInteract",
        vec![
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::INTERACTION_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        move |tick| {
            let position = tick.mob.get_entity().pos.load();
            let closest = tick
                .state
                .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                .and_then(|visible| {
                    visible.find_closest(&tick.visibility(), |entity| {
                        entity.get_entity().entity_type == entity_type
                            && entity
                                .get_entity()
                                .pos
                                .load()
                                .squared_distance_to_vec(&position)
                                <= range_sq
                    })
                });
            let Some(closest) = closest else {
                return false;
            };
            tick.state.set(types::INTERACTION_TARGET, closest.clone());
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(EntityTracker::new(closest, true)),
            );
            true
        },
    )
}
