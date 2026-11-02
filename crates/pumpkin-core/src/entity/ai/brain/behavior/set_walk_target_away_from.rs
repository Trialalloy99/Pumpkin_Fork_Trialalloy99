use std::sync::Arc;

use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::super::BrainTick;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryModuleType, MemoryStatus, MemoryValue, types};
use super::OneShot;
use crate::entity::EntityBase;
use crate::entity::ai::util::default_random_pos;

const FLEE_HORIZONTAL: i32 = 16;
const FLEE_VERTICAL: i32 = 7;
const FLEE_ATTEMPTS: i32 = 10;

fn build<T: MemoryValue>(
    memory: MemoryModuleType<T>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
    to_position: fn(&BrainTick<'_>, &T) -> Vector3<f64>,
) -> OneShot {
    OneShot::new(
        "SetWalkTargetAwayFrom",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (memory.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let current = tick.state.get(types::WALK_TARGET);
            if current.is_some() && !interrupt_current_walk {
                return false;
            }
            let Some(avoid) = tick.state.get(memory).map(|value| to_position(tick, value)) else {
                return false;
            };
            let body = tick.mob.get_entity().pos.load();
            let reach = f64::from(desired_distance);
            if body.squared_distance_to_vec(&avoid) >= reach * reach {
                return false;
            }
            if let Some(current) = current
                && (current.speed_modifier() - speed_modifier).abs() < f32::EPSILON
            {
                let current_direction = current.target().current_position() - body;
                let avoid_direction = avoid - body;
                if current_direction.dot(&avoid_direction) < 0.0 {
                    return false;
                }
            }
            for _ in 0..FLEE_ATTEMPTS {
                if let Some(flee_to) = default_random_pos::get_pos_away(
                    tick.mob,
                    FLEE_HORIZONTAL,
                    FLEE_VERTICAL,
                    avoid,
                ) {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_vec(flee_to, speed_modifier, 0),
                    );
                    break;
                }
            }
            true
        },
    )
}

// brain: SetWalkTargetAwayFrom.pos
#[must_use]
pub fn pos(
    memory: MemoryModuleType<BlockPos>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
) -> OneShot {
    build(
        memory,
        speed_modifier,
        desired_distance,
        interrupt_current_walk,
        |_, pos| pos.to_centered_f64() - Vector3::new(0.0, 0.5, 0.0),
    )
}

// brain: SetWalkTargetAwayFrom.entity
#[must_use]
pub fn entity(
    memory: MemoryModuleType<Arc<dyn EntityBase>>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
) -> OneShot {
    build(
        memory,
        speed_modifier,
        desired_distance,
        interrupt_current_walk,
        |_, entity| entity.get_entity().pos.load(),
    )
}
