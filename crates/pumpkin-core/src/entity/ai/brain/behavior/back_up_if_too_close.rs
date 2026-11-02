use std::sync::Arc;

use pumpkin_util::math::rotate_if_necessary;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;

// brain: BackUpIfTooClose.create
#[must_use]
pub fn create(too_close_distance: i32, strafe_speed: f32) -> OneShot {
    let too_close_sq = f64::from(too_close_distance * too_close_distance);
    OneShot::new(
        "BackUpIfTooClose",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        move |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            let entity = tick.mob.get_entity();
            let close = entity
                .pos
                .load()
                .squared_distance_to_vec(&target.get_entity().pos.load())
                < too_close_sq;
            let visible = tick
                .state
                .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                .is_some_and(|visible| visible.contains(target.as_ref(), &tick.visibility()));
            if !close || !visible {
                return false;
            }
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(EntityTracker::new(target, true)),
            );
            tick.mob
                .get_mob_entity()
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .strafe(-strafe_speed, 0.0);
            entity.yaw.store(rotate_if_necessary(
                entity.yaw.load(),
                entity.head_yaw.load(),
                0.0,
            ));
            true
        },
    )
}
