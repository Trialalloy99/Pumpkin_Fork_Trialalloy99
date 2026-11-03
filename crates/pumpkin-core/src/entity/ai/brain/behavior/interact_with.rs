use std::sync::Arc;

use pumpkin_data::entity::EntityType;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryModuleType, MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;

fn any_entity(_entity: &dyn EntityBase) -> bool {
    true
}

// brain: InteractWith.of
#[must_use]
pub fn of(
    entity_type: &'static EntityType,
    interaction_range: i32,
    interaction_target: MemoryModuleType<Arc<dyn EntityBase>>,
    speed_modifier: f32,
    stop_distance: i32,
) -> OneShot {
    of_with_predicates(
        entity_type,
        interaction_range,
        any_entity,
        any_entity,
        interaction_target,
        speed_modifier,
        stop_distance,
    )
}

// brain: InteractWith.of with self and target filters
#[must_use]
pub fn of_with_predicates(
    entity_type: &'static EntityType,
    interaction_range: i32,
    self_filter: fn(&dyn EntityBase) -> bool,
    target_filter: fn(&dyn EntityBase) -> bool,
    interaction_target: MemoryModuleType<Arc<dyn EntityBase>>,
    speed_modifier: f32,
    stop_distance: i32,
) -> OneShot {
    let range_sq = f64::from(interaction_range * interaction_range);
    OneShot::new(
        "InteractWith",
        vec![
            (interaction_target.id(), MemoryStatus::Registered),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        move |tick| {
            if !self_filter(tick.mob) {
                return false;
            }
            let Some(visible) = tick.state.get(types::NEAREST_VISIBLE_LIVING_ENTITIES) else {
                return false;
            };
            let ctx = tick.visibility();
            let is_valid = |entity: &Arc<dyn EntityBase>| {
                entity.get_entity().entity_type == entity_type && target_filter(entity.as_ref())
            };
            if !visible.contains_any(&ctx, is_valid) {
                return false;
            }
            let position = tick.mob.get_entity().pos.load();
            let closest = visible.find_closest(&ctx, |entity| {
                is_valid(entity)
                    && entity
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&position)
                        <= range_sq
            });
            if let Some(closest) = closest {
                tick.state.set(interaction_target, closest.clone());
                tick.state.set(
                    types::LOOK_TARGET,
                    Arc::new(EntityTracker::new(closest.clone(), true)),
                );
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(
                        Arc::new(EntityTracker::new(closest, false)),
                        speed_modifier,
                        stop_distance,
                    ),
                );
            }
            true
        },
    )
}
