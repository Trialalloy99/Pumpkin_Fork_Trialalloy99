use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{Block, BlockDirection, BlockState};
use pumpkin_inventory::Inventory;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use rustc_hash::FxHashSet;

use super::super::BrainTick;
use super::super::memory::global_pos::GlobalPos;
use super::super::memory::position_tracker::BlockPosTracker;
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::timed::Behavior;
use super::utils::set_walk_and_look_target_memories_to_block;
use crate::block::blocks::chests::{get_chest_container, is_chest_blocked_at};
use crate::block::entities::BlockEntity;
use crate::block::entities::chest::ChestBlockEntity;
use crate::block::entities::trapped_chest::TrappedChestBlockEntity;
use crate::entity::mob::Mob;
use crate::world::World;
use pumpkin_data::block_properties::{ChestLikeProperties, ChestType};
use pumpkin_data::data_component_impl::EquipmentSlot;

pub const TARGET_INTERACTION_TIME: i32 = 60;
const VISITED_POSITIONS_MEMORY_TIME: i64 = 6000;
const TRANSPORTED_ITEM_MAX_STACK_SIZE: u8 = 16;
const MAX_VISITED_POSITIONS: usize = 10;
const MAX_UNREACHABLE_POSITIONS: usize = 50;
const PASSENGER_MOB_TARGET_SEARCH_DISTANCE: i32 = 1;
const IDLE_COOLDOWN: i32 = 140;
const CLOSE_ENOUGH_TO_START_QUEUING_DISTANCE: f64 = 3.0;
const CLOSE_ENOUGH_TO_START_INTERACTING_WITH_TARGET_DISTANCE: f64 = 0.5;
const CLOSE_ENOUGH_TO_START_INTERACTING_WITH_TARGET_PATH_END_DISTANCE: f64 = 1.0;
const CLOSE_ENOUGH_TO_CONTINUE_INTERACTING_WITH_TARGET: f64 = 2.0;
const INTERACTION_VERTICAL_MARGIN: f64 = 0.5;
// timedOut is always false
const NEVER_TIMES_OUT: i32 = i32::MAX / 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContainerInteractionState {
    PickupItem,
    PickupNoItem,
    PlaceItem,
    PlaceNoItem,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TransportItemState {
    Travelling,
    Queuing,
    Interacting,
}

#[derive(Clone)]
pub struct TransportItemTarget {
    pub pos: BlockPos,
    pub container: Arc<dyn Inventory>,
    pub block_entity: Arc<dyn BlockEntity>,
    pub state: &'static BlockState,
    pub block: &'static Block,
}

pub type OnTargetReachedInteraction = fn(&dyn Mob, &TransportItemTarget, i32);

// brain: TransportItemsBetweenContainers constructor arguments
pub struct TransportItemsBetweenContainers {
    speed_modifier: f32,
    source_block_type: fn(&Block) -> bool,
    destination_block_type: fn(&Block) -> bool,
    horizontal_search_distance: i32,
    vertical_search_distance: i32,
    on_target_interaction_actions: [(ContainerInteractionState, OnTargetReachedInteraction); 4],
    on_start_travelling: fn(&dyn Mob),
    should_queue_for_target: fn(&TransportItemTarget) -> bool,
    target: Option<TransportItemTarget>,
    state: TransportItemState,
    interaction_state: Option<ContainerInteractionState>,
    ticks_since_reaching_target: i32,
}

const ENTRY_CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
    (
        types::VISITED_BLOCK_POSITIONS.id(),
        MemoryStatus::Registered,
    ),
    (
        types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS.id(),
        MemoryStatus::Registered,
    ),
    (
        types::TRANSPORT_ITEMS_COOLDOWN_TICKS.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
];

impl TransportItemsBetweenContainers {
    #[must_use]
    #[expect(clippy::too_many_arguments, reason = "mirrors the vanilla constructor")]
    pub const fn new(
        speed_modifier: f32,
        source_block_type: fn(&Block) -> bool,
        destination_block_type: fn(&Block) -> bool,
        horizontal_search_distance: i32,
        vertical_search_distance: i32,
        on_target_interaction_actions: [(ContainerInteractionState, OnTargetReachedInteraction); 4],
        on_start_travelling: fn(&dyn Mob),
        should_queue_for_target: fn(&TransportItemTarget) -> bool,
    ) -> Self {
        Self {
            speed_modifier,
            source_block_type,
            destination_block_type,
            horizontal_search_distance,
            vertical_search_distance,
            on_target_interaction_actions,
            on_start_travelling,
            should_queue_for_target,
            target: None,
            state: TransportItemState::Travelling,
            interaction_state: None,
            ticks_since_reaching_target: 0,
        }
    }

    // brain: TransportItemTarget.tryCreatePossibleTarget
    fn try_create_possible_target(
        world: &Arc<World>,
        pos: BlockPos,
        block_entity: Arc<dyn BlockEntity>,
    ) -> Option<TransportItemTarget> {
        let (block, state) = world.get_block_and_state(&pos);
        let container = get_chest_container(world, &pos)?;
        Some(TransportItemTarget {
            pos,
            container,
            block_entity,
            state,
            block,
        })
    }

    fn is_picking_up_items(body: &dyn Mob) -> bool {
        body.get_mob_entity().with_main_hand(ItemStack::is_empty)
    }

    fn is_wanted_block(&self, body: &dyn Mob, block: &Block) -> bool {
        if Self::is_picking_up_items(body) {
            (self.source_block_type)(block)
        } else {
            (self.destination_block_type)(block)
        }
    }

    fn horizontal_search(&self, body: &dyn Mob) -> i32 {
        if body.get_entity().has_vehicle() {
            PASSENGER_MOB_TARGET_SEARCH_DISTANCE
        } else {
            self.horizontal_search_distance
        }
    }

    fn vertical_search(&self, body: &dyn Mob) -> i32 {
        if body.get_entity().has_vehicle() {
            PASSENGER_MOB_TARGET_SEARCH_DISTANCE
        } else {
            self.vertical_search_distance
        }
    }

    const fn set_transporting_state(&mut self, state: TransportItemState) {
        self.state = state;
    }

    fn on_start_travelling(&mut self, body: &dyn Mob) {
        (self.on_start_travelling)(body);
        self.set_transporting_state(TransportItemState::Travelling);
        self.interaction_state = None;
        self.ticks_since_reaching_target = 0;
    }

    fn stop_in_place(body: &dyn Mob) {
        let mob_entity = body.get_mob_entity();
        mob_entity.steered().navigator().stop();
        mob_entity.set_speed(0.0);
        let living = &mob_entity.living_entity;
        let input = living.movement_input.load();
        living.movement_input.store(Vector3::new(0.0, 0.0, input.z));
        let velocity = living.entity.velocity.load();
        living
            .entity
            .velocity
            .store(Vector3::new(0.0, velocity.y, 0.0));
    }

    fn stop_targeting_current_target(&mut self, tick: &mut BrainTick<'_>) {
        self.ticks_since_reaching_target = 0;
        self.target = None;
        tick.mob.get_mob_entity().steered().navigator().stop();
        tick.state.erase(types::WALK_TARGET.id());
    }

    fn clear_memories_after_matching_target_found(&mut self, tick: &mut BrainTick<'_>) {
        self.stop_targeting_current_target(tick);
        tick.state.erase(types::VISITED_BLOCK_POSITIONS.id());
        tick.state
            .erase(types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS.id());
    }

    fn enter_cooldown_after_no_matching_target_found(&mut self, tick: &mut BrainTick<'_>) {
        self.stop_targeting_current_target(tick);
        tick.state
            .set(types::TRANSPORT_ITEMS_COOLDOWN_TICKS, IDLE_COOLDOWN);
        tick.state.erase(types::VISITED_BLOCK_POSITIONS.id());
        tick.state
            .erase(types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS.id());
    }

    fn global_pos(world: &World, pos: BlockPos) -> Option<GlobalPos> {
        Some(GlobalPos::new(
            Dimension::from_name(world.dimension.minecraft_name)?,
            pos,
        ))
    }

    // brain: setVisitedBlockPos
    fn set_visited_block_pos(&mut self, tick: &mut BrainTick<'_>, pos: BlockPos) {
        let Some(global) = Self::global_pos(tick.world, pos) else {
            return;
        };
        let mut visited: FxHashSet<GlobalPos> = tick
            .state
            .get(types::VISITED_BLOCK_POSITIONS)
            .cloned()
            .unwrap_or_default();
        visited.insert(global);
        if visited.len() > MAX_VISITED_POSITIONS {
            self.enter_cooldown_after_no_matching_target_found(tick);
        } else {
            tick.state.set_with_expiry(
                types::VISITED_BLOCK_POSITIONS,
                visited,
                VISITED_POSITIONS_MEMORY_TIME,
            );
        }
    }

    // brain: markVisitedBlockPosAsUnreachable
    fn mark_visited_block_pos_as_unreachable(&mut self, tick: &mut BrainTick<'_>, pos: BlockPos) {
        let Some(global) = Self::global_pos(tick.world, pos) else {
            return;
        };
        let mut visited: FxHashSet<GlobalPos> = tick
            .state
            .get(types::VISITED_BLOCK_POSITIONS)
            .cloned()
            .unwrap_or_default();
        visited.remove(&global);
        let mut unreachable: FxHashSet<GlobalPos> = tick
            .state
            .get(types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS)
            .cloned()
            .unwrap_or_default();
        unreachable.insert(global);
        if unreachable.len() > MAX_UNREACHABLE_POSITIONS {
            self.enter_cooldown_after_no_matching_target_found(tick);
        } else {
            tick.state.set_with_expiry(
                types::VISITED_BLOCK_POSITIONS,
                visited,
                VISITED_POSITIONS_MEMORY_TIME,
            );
            tick.state.set_with_expiry(
                types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS,
                unreachable,
                VISITED_POSITIONS_MEMORY_TIME,
            );
        }
    }

    // brain: getConnectedTargets, both halves of a double chest
    fn connected_targets(tick: &BrainTick<'_>, target: &TransportItemTarget) -> Vec<BlockPos> {
        let props = ChestLikeProperties::from_state_id(target.state.id);
        let towards = match props.r#type {
            ChestType::Single => None,
            ChestType::Left => Some(props.facing.rotate_clockwise()),
            ChestType::Right => Some(props.facing.rotate_counter_clockwise()),
        };
        let mut positions = vec![target.pos];
        if let Some(direction) = towards {
            let neighbor = target.pos.offset(direction.to_offset());
            if tick.world.get_block_entity(&neighbor).is_some() {
                positions.push(neighbor);
            }
        }
        positions
    }

    fn is_another_mob_interacting_with_target(
        &self,
        tick: &BrainTick<'_>,
        target: &TransportItemTarget,
    ) -> bool {
        Self::connected_targets(tick, target)
            .into_iter()
            .any(|pos| {
                tick.world
                    .get_block_entity(&pos)
                    .and_then(|block_entity| {
                        Self::try_create_possible_target(tick.world, pos, block_entity)
                    })
                    .is_some_and(|connected| (self.should_queue_for_target)(&connected))
            })
    }

    fn is_position_already_visited(tick: &BrainTick<'_>, target: &TransportItemTarget) -> bool {
        let visited = tick.state.get(types::VISITED_BLOCK_POSITIONS);
        let unreachable = tick.state.get(types::UNREACHABLE_TRANSPORT_BLOCK_POSITIONS);
        Self::connected_targets(tick, target)
            .into_iter()
            .any(|pos| {
                Self::global_pos(tick.world, pos).is_some_and(|global| {
                    visited.is_some_and(|set| set.contains(&global))
                        || unreachable.is_some_and(|set| set.contains(&global))
                })
            })
    }

    fn is_target_valid_to_pick(
        &self,
        tick: &BrainTick<'_>,
        block_entity: &Arc<dyn BlockEntity>,
        search_area: &BoundingBox,
    ) -> Option<TransportItemTarget> {
        let pos = block_entity.get_position();
        let inside = f64::from(pos.0.x) >= search_area.min.x
            && f64::from(pos.0.x) < search_area.max.x
            && f64::from(pos.0.y) >= search_area.min.y
            && f64::from(pos.0.y) < search_area.max.y
            && f64::from(pos.0.z) >= search_area.min.z
            && f64::from(pos.0.z) < search_area.max.z;
        if !inside {
            return None;
        }
        let target = Self::try_create_possible_target(tick.world, pos, block_entity.clone())?;
        (self.is_wanted_block(tick.mob, target.block)
            && !Self::is_position_already_visited(tick, &target))
        .then_some(target)
    }

    fn target_search_area(&self, mob: &dyn Mob) -> BoundingBox {
        let pos = mob.get_entity().block_pos.load();
        let horizontal = f64::from(self.horizontal_search(mob));
        let vertical = f64::from(self.vertical_search(mob));
        BoundingBox::from_block(&pos).expand(horizontal, vertical, horizontal)
    }

    // brain: getTransportTarget, the closest chest block entity in the loaded chunks
    fn get_transport_target(&self, tick: &BrainTick<'_>) -> Option<TransportItemTarget> {
        let search_area = self.target_search_area(tick.mob);
        let center = tick.mob.get_entity().chunk_pos.load();
        let range = self.horizontal_search(tick.mob).div_euclid(16) + 1;
        let own_pos = tick.mob.get_entity().pos.load();
        let mut best: Option<(f64, TransportItemTarget)> = None;
        for chunk_x in (center.x - range)..=(center.x + range) {
            for chunk_z in (center.y - range)..=(center.y + range) {
                let Some(entities) = tick
                    .world
                    .block_entities
                    .get(&pumpkin_util::math::vector2::Vector2::new(chunk_x, chunk_z))
                else {
                    continue;
                };
                for block_entity in entities.values() {
                    let is_chest = block_entity.as_any().is::<ChestBlockEntity>()
                        || block_entity.as_any().is::<TrappedChestBlockEntity>();
                    if !is_chest {
                        continue;
                    }
                    let distance = block_entity
                        .get_position()
                        .to_centered_f64()
                        .squared_distance_to_vec(&own_pos);
                    if best
                        .as_ref()
                        .is_some_and(|(closest, _)| distance >= *closest)
                    {
                        continue;
                    }
                    if let Some(target) =
                        self.is_target_valid_to_pick(tick, block_entity, &search_area)
                    {
                        best = Some((distance, target));
                    }
                }
            }
        }
        best.map(|(_, target)| target)
    }

    // brain: updateInvalidTarget
    fn update_invalid_target(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if self.has_valid_target(tick) {
            return false;
        }
        self.stop_targeting_current_target(tick);
        if let Some(target) = self.get_transport_target(tick) {
            let pos = target.pos;
            self.target = Some(target);
            self.on_start_travelling(tick.mob);
            self.set_visited_block_pos(tick, pos);
        } else {
            self.enter_cooldown_after_no_matching_target_found(tick);
        }
        true
    }

    fn has_valid_target(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(target) = self.target.clone() else {
            return false;
        };
        let target_is_of_valid_type = self.is_wanted_block(tick.mob, target.block)
            && tick
                .world
                .get_block_entity(&target.pos)
                .is_some_and(|current| Arc::ptr_eq(&current, &target.block_entity));
        if target_is_of_valid_type && !is_chest_blocked_at(tick.world, &target.pos) {
            if self.state != TransportItemState::Travelling {
                return true;
            }
            if Self::has_valid_travelling_path(tick, &target) {
                return true;
            }
            self.mark_visited_block_pos_as_unreachable(tick, target.pos);
        }
        false
    }

    fn has_finished_path(body: &dyn Mob) -> bool {
        body.get_mob_entity()
            .steered()
            .navigator()
            .get_path()
            .is_some_and(crate::entity::ai::pathfinder::path::Path::is_done)
    }

    fn interaction_range(body: &dyn Mob) -> f64 {
        if Self::has_finished_path(body) {
            CLOSE_ENOUGH_TO_START_INTERACTING_WITH_TARGET_PATH_END_DISTANCE
        } else {
            CLOSE_ENOUGH_TO_START_INTERACTING_WITH_TARGET_DISTANCE
        }
    }

    fn middle_y_position(body: &dyn Mob, pos: Vector3<f64>) -> Vector3<f64> {
        let bounds = body.get_entity().bounding_box.load();
        pos.add_raw(0.0, (bounds.max.y - bounds.min.y) / 2.0, 0.0)
    }

    fn center_pos(body: &dyn Mob) -> Vector3<f64> {
        Self::middle_y_position(body, body.get_entity().pos.load())
    }

    // brain: isWithinTargetDistance
    fn is_within_target_distance(
        distance: f64,
        target: &TransportItemTarget,
        body: &dyn Mob,
        from_pos: Vector3<f64>,
    ) -> bool {
        let bounds = body.get_entity().bounding_box.load();
        let moved = BoundingBox::of_size(
            from_pos,
            bounds.max.x - bounds.min.x,
            bounds.max.y - bounds.min.y,
            bounds.max.z - bounds.min.z,
        );
        target
            .state
            .get_block_collision_shapes_at(&target.pos)
            .map(|shape| shape.expand(distance, INTERACTION_VERTICAL_MARGIN, distance))
            .fold(None::<BoundingBox>, |union, shape| {
                Some(union.map_or(shape, |current| BoundingBox {
                    min: Vector3::new(
                        current.min.x.min(shape.min.x),
                        current.min.y.min(shape.min.y),
                        current.min.z.min(shape.min.z),
                    ),
                    max: Vector3::new(
                        current.max.x.max(shape.max.x),
                        current.max.y.max(shape.max.y),
                        current.max.z.max(shape.max.z),
                    ),
                }))
            })
            .is_some_and(|inflated| inflated.intersects(&moved))
    }

    // brain: hasValidTravellingPath
    fn has_valid_travelling_path(tick: &BrainTick<'_>, target: &TransportItemTarget) -> bool {
        let mob_entity = tick.mob.get_mob_entity();
        let steered = mob_entity.steered();
        let owner = &steered.mob_entity().living_entity;
        let mut navigator = steered.navigator();
        let current = navigator.get_path().cloned();
        let path = current.or_else(|| navigator.create_path(owner, target.pos.to_f64(), 0));
        drop(navigator);
        let position_to_reach_from = path
            .as_ref()
            .and_then(|path| path.get_end_node())
            .map_or_else(
                || Self::center_pos(tick.mob),
                |end| {
                    Self::middle_y_position(
                        tick.mob,
                        Vector3::new(
                            f64::from(end.pos.0.x) + 0.5,
                            f64::from(end.pos.0.y),
                            f64::from(end.pos.0.z) + 0.5,
                        ),
                    )
                },
            );
        let can_reach_target = Self::is_within_target_distance(
            Self::interaction_range(tick.mob),
            target,
            tick.mob,
            position_to_reach_from,
        );
        let has_not_yet_created_path_to_target = path.is_none() && !can_reach_target;
        has_not_yet_created_path_to_target
            || (can_reach_target
                && Self::can_see_any_target_side(target, tick, position_to_reach_from))
    }

    // brain: canSeeAnyTargetSide
    fn can_see_any_target_side(
        target: &TransportItemTarget,
        tick: &BrainTick<'_>,
        eye_position: Vector3<f64>,
    ) -> bool {
        let center = target.pos.to_centered_f64();
        BlockDirection::all().into_iter().any(|direction| {
            let offset = direction.to_offset();
            let hit_target = center.add_raw(
                0.5 * f64::from(offset.x),
                0.5 * f64::from(offset.y),
                0.5 * f64::from(offset.z),
            );
            let hit = tick.world.clip(&pumpkin_world::clip::ClipContext::new(
                eye_position,
                hit_target,
                pumpkin_world::clip::ClipBlock::Collider,
                pumpkin_world::clip::ClipFluid::None,
            ));
            !hit.is_miss && hit.block_pos == target.pos
        })
    }

    fn on_queuing_for_target(&mut self, tick: &mut BrainTick<'_>, target: &TransportItemTarget) {
        if !self.is_another_mob_interacting_with_target(tick, target) {
            self.set_transporting_state(TransportItemState::Travelling);
            self.walk_towards_target(tick);
        }
    }

    fn walk_towards_target(&self, tick: &mut BrainTick<'_>) {
        if let Some(target) = &self.target {
            set_walk_and_look_target_memories_to_block(
                tick.state,
                target.pos,
                self.speed_modifier,
                0,
            );
        }
    }

    // brain: onTravelToTarget
    fn on_travel_to_target(&mut self, tick: &mut BrainTick<'_>, target: &TransportItemTarget) {
        if Self::is_within_target_distance(
            CLOSE_ENOUGH_TO_START_QUEUING_DISTANCE,
            target,
            tick.mob,
            Self::center_pos(tick.mob),
        ) && self.is_another_mob_interacting_with_target(tick, target)
        {
            Self::stop_in_place(tick.mob);
            self.set_transporting_state(TransportItemState::Queuing);
        } else if Self::is_within_target_distance(
            Self::interaction_range(tick.mob),
            target,
            tick.mob,
            Self::center_pos(tick.mob),
        ) {
            self.start_on_reached_target_interaction(tick.mob, target);
        } else {
            self.walk_towards_target(tick);
        }
    }

    fn start_on_reached_target_interaction(
        &mut self,
        body: &dyn Mob,
        target: &TransportItemTarget,
    ) {
        self.interaction_state = Some(if Self::is_picking_up_items(body) {
            if target.container.is_empty() {
                ContainerInteractionState::PickupNoItem
            } else {
                ContainerInteractionState::PickupItem
            }
        } else if Self::matches_leaving_items_requirement(body, target.container.as_ref()) {
            ContainerInteractionState::PlaceItem
        } else {
            ContainerInteractionState::PlaceNoItem
        });
        self.set_transporting_state(TransportItemState::Interacting);
    }

    fn matches_leaving_items_requirement(body: &dyn Mob, container: &dyn Inventory) -> bool {
        if container.is_empty() {
            return true;
        }
        let hand = body.get_mob_entity().with_main_hand(Clone::clone);
        (0..container.size()).any(|slot| {
            let stack = container.get_stack(slot);
            !stack.is_empty() && stack.item == hand.item
        })
    }

    // brain: onReachedTarget
    fn on_reached_target(&mut self, tick: &mut BrainTick<'_>, target: &TransportItemTarget) {
        if !Self::is_within_target_distance(
            CLOSE_ENOUGH_TO_CONTINUE_INTERACTING_WITH_TARGET,
            target,
            tick.mob,
            Self::center_pos(tick.mob),
        ) {
            self.on_start_travelling(tick.mob);
            return;
        }
        self.ticks_since_reaching_target += 1;
        self.on_target_interaction(tick, target);
        if self.ticks_since_reaching_target >= TARGET_INTERACTION_TIME {
            self.do_reached_target_interaction(tick, target);
            self.on_start_travelling(tick.mob);
        }
    }

    // brain: onTargetInteraction
    fn on_target_interaction(&self, tick: &mut BrainTick<'_>, target: &TransportItemTarget) {
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(BlockPosTracker::new(target.pos)),
        );
        Self::stop_in_place(tick.mob);
        if let Some(state) = self.interaction_state
            && let Some((_, action)) = self
                .on_target_interaction_actions
                .iter()
                .find(|(candidate, _)| *candidate == state)
        {
            action(tick.mob, target, self.ticks_since_reaching_target);
        }
    }

    // brain: doReachedTargetInteraction with pickUpItems / putDownItem
    fn do_reached_target_interaction(
        &mut self,
        tick: &mut BrainTick<'_>,
        target: &TransportItemTarget,
    ) {
        let container = target.container.as_ref();
        if Self::is_picking_up_items(tick.mob) {
            if container.is_empty() {
                self.stop_targeting_current_target(tick);
            } else {
                self.pick_up_items(tick, container);
            }
        } else if Self::matches_leaving_items_requirement(tick.mob, container) {
            self.put_down_item(tick, container);
        } else {
            self.stop_targeting_current_target(tick);
        }
    }

    fn pick_up_items(&mut self, tick: &mut BrainTick<'_>, container: &dyn Inventory) {
        let mob_entity = tick.mob.get_mob_entity();
        mob_entity.set_item_slot(
            &EquipmentSlot::MAIN_HAND,
            Self::pickup_item_from_container(container),
        );
        mob_entity.set_guaranteed_drop(&EquipmentSlot::MAIN_HAND);
        container.mark_dirty();
        self.clear_memories_after_matching_target_found(tick);
    }

    fn put_down_item(&mut self, tick: &mut BrainTick<'_>, container: &dyn Inventory) {
        let items_left = Self::add_items_to_container(tick.mob, container);
        container.mark_dirty();
        let empty = items_left.is_empty();
        tick.mob
            .get_mob_entity()
            .set_item_slot(&EquipmentSlot::MAIN_HAND, items_left);
        if empty {
            self.clear_memories_after_matching_target_found(tick);
        } else {
            self.stop_targeting_current_target(tick);
        }
    }

    // brain: pickupItemFromContainer
    fn pickup_item_from_container(container: &dyn Inventory) -> ItemStack {
        for slot in 0..container.size() {
            let stack = container.get_stack(slot);
            if !stack.is_empty() {
                let count = stack.item_count.min(TRANSPORTED_ITEM_MAX_STACK_SIZE);
                return container.remove_stack_specific(slot, count);
            }
        }
        ItemStack::EMPTY.clone()
    }

    // brain: addItemsToContainer
    fn add_items_to_container(body: &dyn Mob, container: &dyn Inventory) -> ItemStack {
        let mut stack = body.get_mob_entity().with_main_hand(Clone::clone);
        for slot in 0..container.size() {
            let mut container_stack = container.get_stack(slot);
            if container_stack.is_empty() {
                container.set_stack(slot, stack);
                return ItemStack::EMPTY.clone();
            }
            if container_stack.are_items_and_components_equal(&stack)
                && container_stack.item_count < container_stack.get_max_stack_size()
            {
                let can_be_added =
                    container_stack.get_max_stack_size() - container_stack.item_count;
                let to_add = can_be_added.min(stack.item_count);
                container_stack.item_count += to_add;
                stack.item_count = stack.item_count.saturating_sub(can_be_added);
                container.set_stack(slot, container_stack);
                if stack.is_empty() {
                    return ItemStack::EMPTY.clone();
                }
            }
        }
        stack
    }
}

impl Behavior for TransportItemsBetweenContainers {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ENTRY_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn max_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        !tick.mob.get_entity().is_leashed()
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        !tick
            .state
            .has_memory_value(types::TRANSPORT_ITEMS_COOLDOWN_TICKS.id())
            && !tick.state.has_memory_value(types::IS_PANICKING.id())
            && !tick.mob.get_entity().is_leashed()
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob
            .get_mob_entity()
            .steered()
            .navigator()
            .set_can_path_to_targets_below_surface(true);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let updated_invalid_target = self.update_invalid_target(tick);
        let Some(target) = self.target.clone() else {
            self.stop(tick);
            return;
        };
        if updated_invalid_target {
            return;
        }
        if self.state == TransportItemState::Queuing {
            self.on_queuing_for_target(tick, &target);
        }
        if self.state == TransportItemState::Travelling {
            self.on_travel_to_target(tick, &target);
        }
        if self.state == TransportItemState::Interacting {
            self.on_reached_target(tick, &target);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        self.on_start_travelling(tick.mob);
        tick.mob
            .get_mob_entity()
            .steered()
            .navigator()
            .set_can_path_to_targets_below_surface(false);
    }

    fn debug_name(&self) -> &'static str {
        "TransportItemsBetweenContainers"
    }
}

// brain: random cooldown used by the copper golem when it spawns
#[must_use]
pub fn initial_cooldown(mob: &dyn Mob) -> i32 {
    mob.get_random().random_range(60..100)
}
