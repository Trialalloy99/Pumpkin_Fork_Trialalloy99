use pumpkin_util::math::position::BlockPos;
use rand::RngExt;

use super::super::memory::{MemoryModuleType, MemoryStatus, types};
use super::{OneShot, utils};

// brain: GoToTargetLocation.create
#[must_use]
pub fn create(
    location_memory: MemoryModuleType<BlockPos>,
    close_enough_dist: i32,
    speed_modifier: f32,
) -> OneShot {
    OneShot::new(
        "GoToTargetLocation",
        vec![
            (location_memory.id(), MemoryStatus::ValuePresent),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let Some(location) = tick.state.get(location_memory).copied() else {
                return false;
            };
            let own = tick.mob.get_entity().block_pos.load();
            if location.squared_distance(&own) >= close_enough_dist * close_enough_dist {
                // brain: GoToTargetLocation.getNearbyPos
                let (dx, dz) = {
                    let mut rng = tick.mob.get_random();
                    (rng.random_range(0..3) - 1, rng.random_range(0..3) - 1)
                };
                let nearby = BlockPos::new(location.0.x + dx, location.0.y, location.0.z + dz);
                utils::set_walk_and_look_target_memories_to_block(
                    tick.state,
                    nearby,
                    speed_modifier,
                    close_enough_dist,
                );
            }
            true
        },
    )
}
