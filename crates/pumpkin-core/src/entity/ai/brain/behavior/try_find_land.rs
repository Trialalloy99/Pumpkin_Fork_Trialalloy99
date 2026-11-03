use pumpkin_data::BlockDirection;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;

use super::super::memory::position_tracker::BlockPosTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::ai::util::goal_utils::for_each_by_manhattan;

const COOLDOWN_TICKS: i64 = 60;
const NEAR_LIQUID_COOLDOWN_TICKS: i64 = 40;

fn has_no_collision(tick: &super::super::BrainTick<'_>, pos: &BlockPos) -> bool {
    tick.world
        .get_block_state(pos)
        .get_block_collision_shapes_at(pos)
        .next()
        .is_none()
}

// brain: TryFindLand.create(range, speedModifier)
#[must_use]
pub fn create(range: i32, speed_modifier: f32) -> OneShot {
    let mut next_ok_start_time = 0i64;
    OneShot::new(
        "TryFindLand",
        vec![
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let body_block_pos = tick.mob.get_entity().block_pos.load();
            if !tick
                .world
                .get_fluid(&body_block_pos)
                .has_tag(&tag::Fluid::MINECRAFT_WATER)
            {
                return false;
            }
            if tick.time < next_ok_start_time {
                next_ok_start_time = tick.time + COOLDOWN_TICKS;
                return true;
            }
            let mut found: Option<BlockPos> = None;
            for_each_by_manhattan(&body_block_pos, range, |pos| {
                if pos.0.x == body_block_pos.0.x && pos.0.z == body_block_pos.0.z {
                    return true;
                }
                if !tick.world.get_fluid_and_fluid_state(pos).1.is_empty {
                    return true;
                }
                // TryFindLand.canStandOn
                if has_no_collision(tick, pos)
                    && tick
                        .world
                        .get_block_state(&pos.down())
                        .is_side_solid(BlockDirection::Up)
                {
                    found = Some(*pos);
                    return false;
                }
                true
            });
            if let Some(pos) = found {
                let tracker = std::sync::Arc::new(BlockPosTracker::new(pos));
                tick.state.set(types::LOOK_TARGET, tracker.clone());
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(tracker, speed_modifier, 1),
                );
            }
            next_ok_start_time = tick.time + COOLDOWN_TICKS;
            true
        },
    )
}

// brain: TryFindLandNearLiquid.create(range, speedModifier, fluidTag)
#[must_use]
pub fn create_near_liquid(
    range: i32,
    speed_modifier: f32,
    fluid_tag: &'static pumpkin_data::tag::Tag,
) -> OneShot {
    let mut next_ok_start_time = 0i64;
    OneShot::new(
        "TryFindLandNearLiquid",
        vec![
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let body_block_pos = tick.mob.get_entity().block_pos.load();
            if tick.world.get_fluid(&body_block_pos).has_tag(fluid_tag) {
                return false;
            }
            if tick.time < next_ok_start_time {
                next_ok_start_time = tick.time + NEAR_LIQUID_COOLDOWN_TICKS;
                return true;
            }
            let mut found: Option<BlockPos> = None;
            for_each_by_manhattan(&body_block_pos, range, |pos| {
                if pos.0.x == body_block_pos.0.x && pos.0.z == body_block_pos.0.z {
                    return true;
                }
                if !has_no_collision(tick, pos) || has_no_collision(tick, &pos.down()) {
                    return true;
                }
                let near_liquid = BlockDirection::horizontal().into_iter().any(|direction| {
                    let side = pos.offset(direction.to_offset());
                    tick.world.get_block_state(&side).is_air()
                        && tick.world.get_fluid(&side.down()).has_tag(fluid_tag)
                });
                if near_liquid {
                    found = Some(*pos);
                    return false;
                }
                true
            });
            if let Some(pos) = found {
                let tracker = std::sync::Arc::new(BlockPosTracker::new(pos));
                tick.state.set(types::LOOK_TARGET, tracker.clone());
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(tracker, speed_modifier, 0),
                );
            }
            next_ok_start_time = tick.time + NEAR_LIQUID_COOLDOWN_TICKS;
            true
        },
    )
}
