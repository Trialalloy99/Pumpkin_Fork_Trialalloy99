use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;

use super::super::BrainTick;
use super::super::memory::global_pos::GlobalPos;
use super::super::memory::{MemoryStatus, types};
use super::OneShot;
use crate::block::blocks::doors::DoorBlock;
use crate::entity::EntityBase;
use crate::world::World;

const COOLDOWN_BEFORE_RERUNNING_IN_SAME_NODE: i32 = 20;
const SKIP_CLOSING_DOOR_IF_FURTHER_AWAY_THAN: f64 = 3.0;
const MAX_DISTANCE_TO_HOLD_DOOR_OPEN_FOR_OTHER_MOBS: f64 = 2.0;

// brain: InteractWithDoor.create
#[must_use]
pub fn create() -> OneShot {
    let mut last_checked_node: Option<BlockPos> = None;
    let mut remaining_cooldown = 0;
    OneShot::new(
        "InteractWithDoor",
        vec![
            (types::PATH.id(), MemoryStatus::ValuePresent),
            (types::DOORS_TO_CLOSE.id(), MemoryStatus::Registered),
            (
                types::NEAREST_LIVING_ENTITIES.id(),
                MemoryStatus::Registered,
            ),
        ],
        move |tick| {
            let Some((from_pos, to_pos)) = tick.state.get(types::PATH).and_then(|path| {
                if path.not_started() || path.is_done() {
                    return None;
                }
                Some((path.get_previous_node()?.pos, path.get_next_node()?.pos))
            }) else {
                return false;
            };
            if last_checked_node == Some(to_pos) {
                remaining_cooldown = COOLDOWN_BEFORE_RERUNNING_IN_SAME_NODE;
            } else {
                remaining_cooldown -= 1;
                if remaining_cooldown > 0 {
                    return false;
                }
            }
            last_checked_node = Some(to_pos);

            let world = tick.world;
            let Some(dimension) = Dimension::from_name(world.dimension.minecraft_name) else {
                return false;
            };
            let mut doors = tick
                .state
                .get(types::DOORS_TO_CLOSE)
                .cloned()
                .unwrap_or_default();
            let mut changed = false;
            if is_interactable_door(world, &from_pos) {
                if !DoorBlock::is_open(world, &from_pos) {
                    DoorBlock::set_open(world, &from_pos, true);
                }
                changed |= doors.insert(GlobalPos::new(dimension, from_pos));
            }
            if is_interactable_door(world, &to_pos) && !DoorBlock::is_open(world, &to_pos) {
                DoorBlock::set_open(world, &to_pos, true);
                changed |= doors.insert(GlobalPos::new(dimension, to_pos));
            }
            if !doors.is_empty() {
                changed |= close_doors_that_i_have_opened_or_passed_through(
                    tick,
                    Some(from_pos),
                    Some(to_pos),
                    &mut doors,
                );
            }
            if changed {
                tick.state.set(types::DOORS_TO_CLOSE, doors);
            }
            true
        },
    )
}

fn is_interactable_door(world: &World, pos: &BlockPos) -> bool {
    world
        .get_block(pos)
        .has_tag(&tag::Block::MINECRAFT_MOB_INTERACTABLE_DOORS)
}

// brain: InteractWithDoor.closeDoorsThatIHaveOpenedOrPassedThrough
pub fn close_doors_that_i_have_opened_or_passed_through<S: std::hash::BuildHasher>(
    tick: &BrainTick<'_>,
    moving_from: Option<BlockPos>,
    moving_to: Option<BlockPos>,
    doors: &mut std::collections::HashSet<GlobalPos, S>,
) -> bool {
    let world = tick.world;
    let body = tick.mob.get_entity();
    let before = doors.len();
    doors.retain(|door| {
        let pos = door.pos;
        if moving_from == Some(pos) || moving_to == Some(pos) {
            return true;
        }
        if door.dimension.id != world.dimension.id
            || !closer_to_center_than(
                &pos,
                body.pos.load(),
                SKIP_CLOSING_DOOR_IF_FURTHER_AWAY_THAN,
            )
        {
            return false;
        }
        if !is_interactable_door(world, &pos) || !DoorBlock::is_open(world, &pos) {
            return false;
        }
        if !are_other_mobs_coming_through_door(tick, &pos) {
            DoorBlock::set_open(world, &pos, false);
        }
        false
    });
    doors.len() != before
}

fn closer_to_center_than(
    pos: &BlockPos,
    other: pumpkin_util::math::vector3::Vector3<f64>,
    distance: f64,
) -> bool {
    pos.to_centered_f64().squared_distance_to_vec(&other) < distance * distance
}

// brain: InteractWithDoor.areOtherMobsComingThroughDoor, a busy other brain counts as not coming
fn are_other_mobs_coming_through_door(tick: &BrainTick<'_>, door_pos: &BlockPos) -> bool {
    let body_type = tick.mob.get_entity().entity_type;
    let Some(nearby) = tick.state.get(types::NEAREST_LIVING_ENTITIES) else {
        return false;
    };
    nearby
        .iter()
        .filter(|other| other.get_entity().entity_type == body_type)
        .filter(|other| {
            closer_to_center_than(
                door_pos,
                other.get_entity().pos.load(),
                MAX_DISTANCE_TO_HOLD_DOOR_OPEN_FOR_OTHER_MOBS,
            )
        })
        .any(|other| is_mob_coming_through_door(other, door_pos))
}

fn is_mob_coming_through_door(other: &Arc<dyn EntityBase>, door_pos: &BlockPos) -> bool {
    let Some(mob) = other.get_mob() else {
        return false;
    };
    mob.get_mob_entity()
        .try_with_brain(mob, |brain| {
            brain.get(types::PATH).is_some_and(|path| {
                !path.is_done()
                    && path.get_previous_node().is_some_and(|from| {
                        from.pos == *door_pos
                            || path.get_next_node().is_some_and(|to| to.pos == *door_pos)
                    })
            })
        })
        .unwrap_or(false)
}
