use pumpkin_data::entity::EntityType;

use super::super::memory::{MemoryStatus, types};
use super::{OneShot, utils};

// brain: SetEntityLookTarget.create(EntityType, maxDist)
#[must_use]
pub fn create_for_type(entity_type: &'static EntityType, max_dist: f32) -> OneShot {
    let max_dist_sqr = f64::from(max_dist * max_dist);
    OneShot::new(
        "SetEntityLookTarget",
        vec![
            (types::LOOK_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        move |tick| {
            let position = tick.mob.get_entity().pos.load();
            let target = tick
                .state
                .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                .and_then(|visible| {
                    visible.find_closest(&tick.visibility(), |target| {
                        target.get_entity().entity_type == entity_type
                            && target
                                .get_entity()
                                .pos
                                .load()
                                .squared_distance_to_vec(&position)
                                <= max_dist_sqr
                    })
                });
            let Some(target) = target else {
                return false;
            };
            utils::look_at_entity(tick.state, target);
            true
        },
    )
}
