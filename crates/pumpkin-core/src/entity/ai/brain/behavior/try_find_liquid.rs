use pumpkin_data::Block;
use pumpkin_data::tag::Taggable;
use pumpkin_util::math::position::BlockPos;

use super::super::memory::position_tracker::BlockPosTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::entity::ai::util::goal_utils::for_each_by_manhattan;

const COOLDOWN_ON_SEARCH: i64 = 40;
const COOLDOWN_ON_SKIP: i64 = 20 + 2;
const CLOSE_ENOUGH: f64 = 1.5;

// brain: state.getBlock() instanceof LiquidBlock
fn is_liquid_block(block: &Block) -> bool {
    block.id == Block::WATER.id || block.id == Block::LAVA.id
}

// brain: TryFindLiquid.create(range, speedModifier, FluidTags.AXOLOTL_TRIES_TO_FIND)
#[must_use]
pub fn create(
    range: i32,
    speed_modifier: f32,
    fluid_tag: &'static pumpkin_data::tag::Tag,
) -> OneShot {
    let mut next_ok_start_time = 0i64;
    OneShot::new(
        "TryFindLiquid",
        vec![
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let entity = tick.mob.get_entity();
            let body_block_pos = entity.block_pos.load();
            if tick.world.get_fluid(&body_block_pos).has_tag(fluid_tag) {
                return false;
            }
            if tick.time < next_ok_start_time {
                next_ok_start_time = tick.time + COOLDOWN_ON_SKIP;
                return true;
            }
            let position = entity.pos.load();
            let mut found: Option<BlockPos> = None;
            for_each_by_manhattan(&body_block_pos, range, |pos| {
                if pos.0.x == body_block_pos.0.x && pos.0.z == body_block_pos.0.z {
                    return true;
                }
                let (block, fluid) = tick.world.get_block_and_fluid(pos);
                if !is_liquid_block(block) || !fluid.has_tag(fluid_tag) {
                    return true;
                }
                if tick.world.get_block_state(&pos.up()).is_air() {
                    found = Some(*pos);
                    return false;
                }
                if found.is_none()
                    && pos.to_centered_f64().squared_distance_to_vec(&position)
                        >= CLOSE_ENOUGH * CLOSE_ENOUGH
                {
                    found = Some(*pos);
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
            next_ok_start_time = tick.time + COOLDOWN_ON_SEARCH;
            true
        },
    )
}
