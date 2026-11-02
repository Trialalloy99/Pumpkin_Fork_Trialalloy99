use std::sync::Arc;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryModuleType, MemoryStatus, types};
use super::OneShot;
use crate::entity::EntityBase;
use crate::entity::player::Player;

// brain: BabyFollowAdult.create
#[must_use]
pub fn create<T: Clone + Send + Sync + 'static>(
    follow_min: i32,
    follow_max: i32,
    speed_modifier: f32,
    nearest_visible_type: MemoryModuleType<T>,
    target_eye: bool,
    as_entity: fn(&T) -> Arc<dyn EntityBase>,
) -> OneShot
where
    T: crate::entity::ai::brain::memory::MemoryValue,
{
    OneShot::new(
        "BabyFollowAdult",
        vec![
            (nearest_visible_type.id(), MemoryStatus::ValuePresent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
        ],
        move |tick| {
            if !tick.mob.get_mob_entity().living_entity.is_baby() {
                return false;
            }
            let Some(adult) = tick.state.get(nearest_visible_type).map(as_entity) else {
                return false;
            };
            let own = tick.mob.get_entity().pos.load();
            let distance_sq = own.squared_distance_to_vec(&adult.get_entity().pos.load());
            let max = f64::from(follow_max + 1);
            let min = f64::from(follow_min);
            if distance_sq < max * max && distance_sq >= min * min {
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(
                        Arc::new(EntityTracker::with_target_eye_height(
                            adult.clone(),
                            target_eye,
                            target_eye,
                        )),
                        speed_modifier,
                        follow_min - 1,
                    ),
                );
                tick.state.set(
                    types::LOOK_TARGET,
                    Arc::new(EntityTracker::with_target_eye_height(
                        adult, true, target_eye,
                    )),
                );
                true
            } else {
                false
            }
        },
    )
}

#[must_use]
pub fn player_as_entity(player: &Arc<Player>) -> Arc<dyn EntityBase> {
    Arc::clone(player) as Arc<dyn EntityBase>
}

#[must_use]
pub fn same_entity(entity: &Arc<dyn EntityBase>) -> Arc<dyn EntityBase> {
    Arc::clone(entity)
}
