//! Ported from the villager work, social and raid behaviors in
//! `net.minecraft.world.entity.ai.behavior`.

use std::sync::Arc;

use pumpkin_data::block_properties::ComposterLikeProperties;
use pumpkin_data::data_component_impl::{CompostableImpl, EquipmentSlot};
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_data::world::WorldEvent;
use pumpkin_data::{Block, tag};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

use super::actions::crop_age;
use super::behaviors::{
    block_dist_sq, body_pos, closer_to_center_than, has_no_blocks_above, villager,
};
use super::{VillagerEntity, VillagerProfession};
use crate::block::blocks::composter::ComposterBlock;
use crate::entity::EntityBase;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, Timed, utils};
use crate::entity::ai::brain::memory::position_tracker::{
    BlockPosTracker, EntityTracker, PositionTracker,
};
use crate::entity::ai::brain::memory::walk_target::WalkTarget;
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, types};
use crate::entity::ai::brain::{BrainState, BrainTick};
use crate::entity::ai::util::land_random_pos;

use crate::world::loot::{LootContextParameters, generate_loot_from_handle};

fn entity_tracker(entity: Arc<dyn EntityBase>) -> Arc<dyn PositionTracker> {
    Arc::new(EntityTracker::new(entity, true))
}

// behavior util: BehaviorUtils.lockGazeAndWalkToEachOther
fn lock_gaze_and_walk_to_each_other(
    tick: &mut BrainTick<'_>,
    other: &Arc<dyn EntityBase>,
    speed_modifier: f32,
    close_enough_distance: i32,
) {
    let Some(me) = tick
        .mob
        .get_entity()
        .world
        .load()
        .get_entity_by_id(tick.mob.get_entity().entity_id)
    else {
        return;
    };
    tick.state
        .set(types::LOOK_TARGET, entity_tracker(other.clone()));
    utils::set_walk_and_look_target_memories_to_entity(
        tick.state,
        other.clone(),
        speed_modifier,
        close_enough_distance,
    );
    if let Some(other_villager) = other.cast_any().downcast_ref::<VillagerEntity>() {
        other_villager
            .mob_entity
            .try_with_brain(other_villager, |brain| {
                brain.set(types::LOOK_TARGET, entity_tracker(me.clone()));
                utils::set_walk_and_look_target_memories_to_entity(
                    brain,
                    me.clone(),
                    speed_modifier,
                    close_enough_distance,
                );
            });
    }
}

// behavior: WorkAtPoi / WorkAtComposter
struct WorkAtPoi {
    composter: bool,
    last_check: i64,
}

const WORK_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::JOB_SITE.id(), MemoryStatus::ValuePresent),
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
];
const WORK_CHECK_COOLDOWN: i64 = 300;
const WORK_DISTANCE: f64 = 1.73;
const COMPOSTABLE_SEEDS: [&Item; 2] = [&Item::WHEAT_SEEDS, &Item::BEETROOT_SEEDS];

impl WorkAtPoi {
    fn at_job_site(tick: &BrainTick<'_>) -> bool {
        tick.state.get(types::JOB_SITE).is_some_and(|target| {
            target.is_in_world(tick.world)
                && closer_to_center_than(target.pos, body_pos(tick), WORK_DISTANCE)
        })
    }

    // work at composter: makeBread
    fn make_bread(villager: &VillagerEntity, world: &Arc<crate::world::World>) {
        if villager.count_inventory_item(&Item::BREAD) > 36 {
            return;
        }
        let wheat = villager.count_inventory_item(&Item::WHEAT);
        let bread = (wheat / 3).min(3);
        if bread == 0 {
            return;
        }
        villager.remove_inventory_item_type(&Item::WHEAT, bread * 3);
        let leftover = villager.add_inventory_item(ItemStack::new(bread as u8, &Item::BREAD));
        if !leftover.is_empty() {
            world.drop_stack(&villager.get_entity().block_pos.load(), leftover);
        }
    }

    // work at composter: compostItems
    fn compost_items(villager: &VillagerEntity, world: &Arc<crate::world::World>, pos: BlockPos) {
        let composter = ComposterBlock;
        let (block, state_id) = world.get_block_and_state_id(&pos);
        let mut state_id = state_id;
        let original = state_id;
        if ComposterLikeProperties::from_state_id(state_id).level == 8 {
            composter.clear_composter(world, &pos, state_id, block);
            state_id = world.get_block_state_id(&pos);
        }
        let mut total_to_use = 20;
        let min_stack_size = 10;
        let mut seen = [0i32; 2];
        let mut inventory = villager
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut temp_state = state_id;
        'slots: for slot in (0..inventory.len()).rev() {
            if total_to_use <= 0 {
                break;
            }
            let Some(index) = COMPOSTABLE_SEEDS
                .iter()
                .position(|item| item.id == inventory[slot].item.id)
            else {
                continue;
            };
            if inventory[slot].is_empty() {
                continue;
            }
            let stack_size = i32::from(inventory[slot].item_count);
            seen[index] += stack_size;
            let to_use = (seen[index] - min_stack_size)
                .min(total_to_use)
                .min(stack_size);
            if to_use <= 0 {
                continue;
            }
            total_to_use -= to_use;
            for _ in 0..to_use {
                let level = ComposterLikeProperties::from_state_id(temp_state).level;
                let chance = inventory[slot]
                    .get_data_component::<CompostableImpl>()
                    .map_or(0.0, |compostable| compostable.chance);
                inventory[slot].item_count -= 1;
                if level < 7
                    && ((level == 0 && chance > 0.0) || rand::rng().random::<f32>() < chance)
                {
                    composter.update_level_composter(world, &pos, temp_state, block, level + 1);
                    temp_state = world.get_block_state_id(&pos);
                }
                if inventory[slot].item_count == 0 {
                    inventory[slot] = ItemStack::EMPTY.clone();
                }
                if ComposterLikeProperties::from_state_id(temp_state).level == 7 {
                    world.sync_world_event(
                        WorldEvent::ComposterFill,
                        pos,
                        i32::from(temp_state != original),
                    );
                    return;
                }
                if inventory[slot].is_empty() {
                    continue 'slots;
                }
            }
        }
        world.sync_world_event(
            WorldEvent::ComposterFill,
            pos,
            i32::from(temp_state != original),
        );
    }
}

impl Behavior for WorkAtPoi {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &WORK_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let game_time = tick.world.get_world_age();
        if game_time - self.last_check < WORK_CHECK_COOLDOWN {
            return false;
        }
        if tick.mob.get_random().random_range(0..2) != 0 {
            return false;
        }
        self.last_check = game_time;
        Self::at_job_site(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let Some(villager) = villager(tick) else {
            return;
        };
        tick.state.set(types::LAST_WORKED_AT_POI, tick.time);
        if let Some(site) = tick.state.get(types::JOB_SITE).copied() {
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(BlockPosTracker::new(site.pos)) as Arc<dyn PositionTracker>,
            );
            villager.play_work_sound();
            if self.composter && tick.world.get_block(&site.pos) == &Block::COMPOSTER {
                Self::make_bread(villager, tick.world);
                Self::compost_items(villager, tick.world, site.pos);
            }
        }
        if villager.should_restock(tick.world) {
            villager.restock();
        }
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::at_job_site(tick)
    }

    fn debug_name(&self) -> &'static str {
        "WorkAtPoi"
    }
}

pub fn work_at_poi(composter: bool) -> Timed<impl Behavior> {
    Timed::new(WorkAtPoi {
        composter,
        last_check: 0,
    })
}

// behavior: HarvestFarmland
struct HarvestFarmland {
    above_farmland_pos: Option<BlockPos>,
    next_ok_start_time: i64,
    time_worked_so_far: i32,
    valid_farmland: Vec<BlockPos>,
}

const HARVEST_CONDITIONS: [(MemoryModuleId, MemoryStatus); 3] = [
    (types::LOOK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::SECONDARY_JOB_SITE.id(), MemoryStatus::ValuePresent),
];
const HARVEST_DURATION: i32 = 200;
const FARM_SPEED_MODIFIER: f32 = 0.5;

impl HarvestFarmland {
    fn valid_pos(world: &crate::world::World, pos: &BlockPos) -> bool {
        let state_id = world.get_block_state_id(pos);
        let state = pumpkin_data::BlockState::from_id(state_id);
        let below = world.get_block(&pos.down());
        crop_age(state_id).is_some_and(|(age, max)| age >= max)
            || (state.is_air() && below == &Block::FARMLAND)
    }

    fn random_valid(&self, tick: &BrainTick<'_>) -> Option<BlockPos> {
        if self.valid_farmland.is_empty() {
            None
        } else {
            let index = tick
                .mob
                .get_random()
                .random_range(0..self.valid_farmland.len());
            Some(self.valid_farmland[index])
        }
    }

    fn walk_and_look(tick: &mut BrainTick<'_>, pos: BlockPos) {
        let tracker: Arc<dyn PositionTracker> = Arc::new(BlockPosTracker::new(pos));
        tick.state.set(types::LOOK_TARGET, tracker.clone());
        tick.state.set(
            types::WALK_TARGET,
            WalkTarget::new(tracker, FARM_SPEED_MODIFIER, 1),
        );
    }

    const fn seed_block(item: &Item) -> Option<&'static Block> {
        if item.id == Item::WHEAT_SEEDS.id {
            Some(&Block::WHEAT)
        } else if item.id == Item::BEETROOT_SEEDS.id {
            Some(&Block::BEETROOTS)
        } else if item.id == Item::CARROT.id {
            Some(&Block::CARROTS)
        } else if item.id == Item::POTATO.id {
            Some(&Block::POTATOES)
        } else if item.id == Item::TORCHFLOWER_SEEDS.id {
            Some(&Block::TORCHFLOWER_CROP)
        } else {
            None
        }
    }
}

impl Behavior for HarvestFarmland {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &HARVEST_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if !tick.world.level_info.load().game_rules.mob_griefing {
            return false;
        }
        let Some(villager) = villager(tick) else {
            return false;
        };
        if villager.profession() != VillagerProfession::Farmer {
            return false;
        }
        let body = tick.mob.get_entity().block_pos.load();
        self.valid_farmland.clear();
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = BlockPos::new(body.0.x + x, body.0.y + y, body.0.z + z);
                    if Self::valid_pos(tick.world, &pos) {
                        self.valid_farmland.push(pos);
                    }
                }
            }
        }
        self.above_farmland_pos = self.random_valid(tick);
        self.above_farmland_pos.is_some()
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if tick.time > self.next_ok_start_time
            && let Some(pos) = self.above_farmland_pos
        {
            Self::walk_and_look(tick, pos);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::LOOK_TARGET.id());
        tick.state.erase(types::WALK_TARGET.id());
        self.time_worked_so_far = 0;
        self.next_ok_start_time = tick.time + 40;
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(villager) = villager(tick) else {
            return;
        };
        if let Some(pos) = self.above_farmland_pos
            && !closer_to_center_than(pos, body_pos(tick), 1.0)
        {
            return;
        }
        if let Some(pos) = self.above_farmland_pos
            && tick.time > self.next_ok_start_time
        {
            let world = tick.world;
            let state_id = world.get_block_state_id(&pos);
            let is_air = pumpkin_data::BlockState::from_id(state_id).is_air();
            let below_farmland = world.get_block(&pos.down()) == &Block::FARMLAND;
            let crop = crop_age(state_id);
            if crop.is_some_and(|(age, max)| age >= max) {
                world.break_block(&pos, None, BlockFlags::NOTIFY_ALL);
            }
            if is_air && below_farmland && villager.has_farm_seeds() {
                let mut inventory = villager
                    .inventory
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for slot in inventory.iter_mut() {
                    if slot.is_empty()
                        || !slot
                            .item
                            .has_tag(&tag::Item::MINECRAFT_VILLAGER_PLANTABLE_SEEDS)
                    {
                        continue;
                    }
                    let Some(block) = Self::seed_block(slot.item) else {
                        continue;
                    };
                    world.set_block_state(&pos, block.default_state.id, BlockFlags::NOTIFY_ALL);
                    world.emit_game_event(
                        pumpkin_data::game_event::GameEvent::BlockPlace.name(),
                        pos.to_centered_f64(),
                    );
                    world.play_sound_fine(
                        Sound::ItemCropPlant,
                        SoundCategory::Blocks,
                        &pos.to_centered_f64(),
                        1.0,
                        1.0,
                    );
                    slot.item_count -= 1;
                    if slot.item_count == 0 {
                        *slot = ItemStack::EMPTY.clone();
                    }
                    break;
                }
            }
            if crop.is_some_and(|(age, max)| age < max) {
                self.valid_farmland.retain(|candidate| *candidate != pos);
                self.above_farmland_pos = self.random_valid(tick);
                if let Some(next) = self.above_farmland_pos {
                    self.next_ok_start_time = tick.time + 20;
                    Self::walk_and_look(tick, next);
                }
            }
        }
        self.time_worked_so_far += 1;
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        self.time_worked_so_far < HARVEST_DURATION
    }

    fn debug_name(&self) -> &'static str {
        "HarvestFarmland"
    }
}

pub fn harvest_farmland() -> Timed<impl Behavior> {
    Timed::new(HarvestFarmland {
        above_farmland_pos: None,
        next_ok_start_time: 0,
        time_worked_so_far: 0,
        valid_farmland: Vec::new(),
    })
}

// behavior: UseBonemeal
struct UseBonemeal {
    next_work_cycle_time: i64,
    last_bonemealing_session: i64,
    time_worked_so_far: i32,
    crop_pos: Option<BlockPos>,
}

const BONEMEAL_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::LOOK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
];
const BONEMEALING_DURATION: i32 = 80;

impl UseBonemeal {
    fn valid_pos(world: &crate::world::World, pos: &BlockPos) -> bool {
        crop_age(world.get_block_state_id(pos)).is_some_and(|(age, max)| age < max)
    }

    // behavior: pickNextTarget, reservoir sampling over the 3x3x3 cube
    fn pick_next_target(tick: &BrainTick<'_>) -> Option<BlockPos> {
        let body = tick.mob.get_entity().block_pos.load();
        let mut result = None;
        let mut count = 0;
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = BlockPos::new(body.0.x + x, body.0.y + y, body.0.z + z);
                    if Self::valid_pos(tick.world, &pos) {
                        count += 1;
                        if tick.mob.get_random().random_range(0..count) == 0 {
                            result = Some(pos);
                        }
                    }
                }
            }
        }
        result
    }

    fn set_current_crop_as_target(&self, tick: &mut BrainTick<'_>) {
        if let Some(pos) = self.crop_pos {
            let tracker: Arc<dyn PositionTracker> = Arc::new(BlockPosTracker::new(pos));
            tick.state.set(types::LOOK_TARGET, tracker.clone());
            tick.state
                .set(types::WALK_TARGET, WalkTarget::new(tracker, 0.5, 1));
        }
    }
}

impl Behavior for UseBonemeal {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &BONEMEAL_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(villager) = villager(tick) else {
            return false;
        };
        let tick_count = i64::from(
            tick.mob
                .get_entity()
                .tick_count
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        if tick_count % 10 == 0
            && (self.last_bonemealing_session == 0
                || self.last_bonemealing_session + 160 <= tick_count)
        {
            if villager.count_inventory_item(&Item::BONE_MEAL) <= 0 {
                return false;
            }
            self.crop_pos = Self::pick_next_target(tick);
            self.crop_pos.is_some()
        } else {
            false
        }
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        self.time_worked_so_far < BONEMEALING_DURATION && self.crop_pos.is_some()
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.set_current_crop_as_target(tick);
        if let Some(villager) = villager(tick) {
            villager.mob_entity.set_item_slot(
                &EquipmentSlot::MAIN_HAND,
                ItemStack::new(1, &Item::BONE_MEAL),
            );
        }
        self.next_work_cycle_time = tick.time;
        self.time_worked_so_far = 0;
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(villager) = villager(tick) {
            villager
                .mob_entity
                .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::EMPTY.clone());
        }
        self.last_bonemealing_session = i64::from(
            tick.mob
                .get_entity()
                .tick_count
                .load(std::sync::atomic::Ordering::Relaxed),
        );
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = self.crop_pos else {
            return;
        };
        if tick.time < self.next_work_cycle_time
            || !closer_to_center_than(target, body_pos(tick), 1.0)
        {
            return;
        }
        let Some(villager) = villager(tick) else {
            return;
        };
        if villager.count_inventory_item(&Item::BONE_MEAL) > 0 {
            let world = tick.world;
            let (block, state_id) = world.get_block_and_state_id(&target);
            let grown = world.server.upgrade().is_some_and(|server| {
                server
                    .block_registry
                    .bone_meal(block, world, &target, state_id)
            });
            if grown {
                villager.remove_inventory_item_type(&Item::BONE_MEAL, 1);
                world.sync_world_event(WorldEvent::ParticlesAndSoundPlantGrowth, target, 15);
                self.crop_pos = Self::pick_next_target(tick);
                self.set_current_crop_as_target(tick);
                self.next_work_cycle_time = tick.time + 40;
            }
        }
        self.time_worked_so_far += 1;
    }

    fn debug_name(&self) -> &'static str {
        "UseBonemeal"
    }
}

pub fn use_bonemeal() -> Timed<impl Behavior> {
    Timed::new(UseBonemeal {
        next_work_cycle_time: 0,
        last_bonemealing_session: 0,
        time_worked_so_far: 0,
        crop_pos: None,
    })
}

// behavior: ShowTradesToPlayer
struct ShowTradesToPlayer {
    min_duration: i32,
    max_duration: i32,
    player_item: Option<ItemStack>,
    display_items: Vec<ItemStack>,
    cycle_counter: i32,
    display_index: usize,
    look_time: i32,
}

const SHOW_TRADES_CONDITIONS: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::INTERACTION_TARGET.id(), MemoryStatus::ValuePresent)];
const MAX_LOOK_TIME: i32 = 900;
const STARTING_LOOK_TIME: i32 = 40;

impl ShowTradesToPlayer {
    fn target(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        tick.state.get(types::INTERACTION_TARGET).cloned()
    }

    fn can_show(tick: &BrainTick<'_>) -> bool {
        let Some(villager) = villager(tick) else {
            return false;
        };
        let Some(target) = Self::target(tick) else {
            return false;
        };
        target.get_player().is_some()
            && tick.mob.get_entity().is_alive()
            && target.get_entity().is_alive()
            && !villager.is_baby_villager()
            && tick
                .mob
                .get_entity()
                .pos
                .load()
                .squared_distance_to_vec(&target.get_entity().pos.load())
                <= 17.0
    }

    fn look_at_target(tick: &mut BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        let target = Self::target(tick)?;
        tick.state
            .set(types::LOOK_TARGET, entity_tracker(target.clone()));
        Some(target)
    }

    fn display_as_held_item(villager: &VillagerEntity, stack: ItemStack) {
        villager
            .mob_entity
            .set_item_slot(&EquipmentSlot::MAIN_HAND, stack);
        villager
            .mob_entity
            .set_drop_chance(&EquipmentSlot::MAIN_HAND, 0.0);
    }

    fn clear_held_item(villager: &VillagerEntity) {
        villager
            .mob_entity
            .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::EMPTY.clone());
        villager
            .mob_entity
            .set_drop_chance(&EquipmentSlot::MAIN_HAND, 0.085);
    }

    fn matches_cost(
        player_item: &ItemStack,
        offer: &pumpkin_protocol::java::client::play::MerchantOffer,
    ) -> bool {
        let cost_a: &ItemStack = offer.base_cost_a.0.as_ref();
        let cost_b = offer
            .cost_b
            .as_ref()
            .map(|cost| -> &ItemStack { cost.0.as_ref() });
        player_item.item.id == cost_a.item.id
            || cost_b.is_some_and(|cost| player_item.item.id == cost.item.id)
    }

    fn find_items_to_display(&mut self, target: &Arc<dyn EntityBase>, villager: &VillagerEntity) {
        let current = target.get_player().map_or_else(
            || ItemStack::EMPTY.clone(),
            |player| {
                player
                    .inventory()
                    .get_stack_in_hand(pumpkin_util::Hand::Right)
            },
        );
        let changed = self
            .player_item
            .as_ref()
            .is_none_or(|previous| previous.item.id != current.item.id);
        if changed {
            self.player_item = Some(current.clone());
            self.display_items.clear();
            if !current.is_empty() {
                let offers = villager
                    .offers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for offer in offers.iter() {
                    if !offer.is_out_of_stock() && Self::matches_cost(&current, offer) {
                        let output: &ItemStack = offer.output.0.as_ref();
                        self.display_items.push(output.clone());
                    }
                }
                if let Some(first) = self.display_items.first() {
                    self.look_time = MAX_LOOK_TIME;
                    Self::display_as_held_item(villager, first.clone());
                }
            }
        }
    }
}

impl Behavior for ShowTradesToPlayer {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SHOW_TRADES_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        self.min_duration
    }

    fn max_duration(&self) -> i32 {
        self.max_duration
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::can_show(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::can_show(tick) && self.look_time > 0
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        Self::look_at_target(tick);
        self.cycle_counter = 0;
        self.display_index = 0;
        self.look_time = STARTING_LOOK_TIME;
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = Self::look_at_target(tick) else {
            return;
        };
        let Some(villager) = villager(tick) else {
            return;
        };
        self.find_items_to_display(&target, villager);
        if self.display_items.is_empty() {
            Self::clear_held_item(villager);
            self.look_time = self.look_time.min(STARTING_LOOK_TIME);
        } else if self.display_items.len() >= 2 {
            self.cycle_counter += 1;
            if self.cycle_counter >= 40 {
                self.display_index += 1;
                self.cycle_counter = 0;
                if self.display_index > self.display_items.len() - 1 {
                    self.display_index = 0;
                }
                Self::display_as_held_item(
                    villager,
                    self.display_items[self.display_index].clone(),
                );
            }
        }
        self.look_time -= 1;
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::INTERACTION_TARGET.id());
        if let Some(villager) = villager(tick) {
            Self::clear_held_item(villager);
        }
        self.player_item = None;
    }

    fn debug_name(&self) -> &'static str {
        "ShowTradesToPlayer"
    }
}

pub fn show_trades_to_player(min_duration: i32, max_duration: i32) -> Timed<impl Behavior> {
    Timed::new(ShowTradesToPlayer {
        min_duration,
        max_duration,
        player_item: None,
        display_items: Vec::new(),
        cycle_counter: 0,
        display_index: 0,
        look_time: 0,
    })
}

// behavior: GiveGiftToHero
struct GiveGiftToHero {
    timeout: i32,
    time_until_next_gift: i32,
    gift_given_during_this_run: bool,
    time_since_start: i64,
}

const GIFT_CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::WALK_TARGET.id(), MemoryStatus::Registered),
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::INTERACTION_TARGET.id(), MemoryStatus::Registered),
    (
        types::NEAREST_VISIBLE_PLAYER.id(),
        MemoryStatus::ValuePresent,
    ),
];
const THROW_GIFT_AT_DISTANCE: i64 = 5;
const MIN_TIME_BETWEEN_GIFTS: i32 = 600;
const TIME_TO_DELAY_FOR_HEAD_TO_FINISH_TURNING: i64 = 20;

impl GiveGiftToHero {
    fn hero(state: &BrainState) -> Option<Arc<crate::entity::player::Player>> {
        state
            .get(types::NEAREST_VISIBLE_PLAYER)
            .filter(|player| {
                player
                    .living_entity
                    .has_effect(&StatusEffect::HERO_OF_THE_VILLAGE)
            })
            .cloned()
    }

    fn gift_table(villager: &VillagerEntity) -> String {
        if villager.is_baby_villager() {
            return "minecraft:gameplay/hero_of_the_village/baby_gift".to_owned();
        }
        let name = match villager.profession() {
            VillagerProfession::None | VillagerProfession::Nitwit => "unemployed",
            VillagerProfession::Armorer => "armorer",
            VillagerProfession::Butcher => "butcher",
            VillagerProfession::Cartographer => "cartographer",
            VillagerProfession::Cleric => "cleric",
            VillagerProfession::Farmer => "farmer",
            VillagerProfession::Fisherman => "fisherman",
            VillagerProfession::Fletcher => "fletcher",
            VillagerProfession::Leatherworker => "leatherworker",
            VillagerProfession::Librarian => "librarian",
            VillagerProfession::Mason => "mason",
            VillagerProfession::Shepherd => "shepherd",
            VillagerProfession::Toolsmith => "toolsmith",
            VillagerProfession::Weaponsmith => "weaponsmith",
        };
        format!("minecraft:gameplay/hero_of_the_village/{name}_gift")
    }

    fn throw_gift(villager: &VillagerEntity, target: Vector3<f64>) {
        let world = villager.get_entity().world.load_full();
        let Some(handle) = world.get_loot_table(&Self::gift_table(villager)) else {
            return;
        };
        let params = LootContextParameters {
            position: Some(villager.get_entity().pos.load()),
            this_entity: Some(villager),
            world: Some(&world),
            ..Default::default()
        };
        for stack in generate_loot_from_handle(&handle, rand::random(), &params) {
            villager.throw_item(stack, target);
        }
    }
}

impl Behavior for GiveGiftToHero {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &GIFT_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        self.timeout
    }

    fn max_duration(&self) -> i32 {
        self.timeout
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if Self::hero(tick.state).is_none() {
            return false;
        }
        if self.time_until_next_gift > 0 {
            self.time_until_next_gift -= 1;
            return false;
        }
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.gift_given_during_this_run = false;
        self.time_since_start = tick.time;
        if let Some(player) = Self::hero(tick.state) {
            let entity: Arc<dyn EntityBase> = player;
            tick.state.set(types::INTERACTION_TARGET, entity.clone());
            utils::look_at_entity(tick.state, entity);
        }
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::hero(tick.state).is_some() && !self.gift_given_during_this_run
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(player) = Self::hero(tick.state) else {
            return;
        };
        let Some(villager) = villager(tick) else {
            return;
        };
        let entity: Arc<dyn EntityBase> = player.clone();
        utils::look_at_entity(tick.state, entity.clone());
        let within = block_dist_sq(
            player.get_entity().block_pos.load(),
            tick.mob.get_entity().block_pos.load(),
        ) < THROW_GIFT_AT_DISTANCE * THROW_GIFT_AT_DISTANCE;
        if within {
            if tick.time - self.time_since_start > TIME_TO_DELAY_FOR_HEAD_TO_FINISH_TURNING {
                Self::throw_gift(villager, player.get_entity().pos.load());
                self.gift_given_during_this_run = true;
            }
        } else {
            utils::set_walk_and_look_target_memories_to_entity(tick.state, entity, 0.5, 5);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        self.time_until_next_gift =
            MIN_TIME_BETWEEN_GIFTS + tick.mob.get_random().random_range(0..=6000);
        tick.state.erase(types::INTERACTION_TARGET.id());
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::LOOK_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "GiveGiftToHero"
    }
}

pub fn give_gift_to_hero(timeout: i32) -> Timed<impl Behavior> {
    Timed::new(GiveGiftToHero {
        timeout,
        time_until_next_gift: MIN_TIME_BETWEEN_GIFTS,
        gift_given_during_this_run: false,
        time_since_start: 0,
    })
}

// behavior: PlayTagWithOtherKids
fn interaction_target_id(kid: &Arc<dyn EntityBase>) -> Option<i32> {
    let villager = kid.cast_any().downcast_ref::<VillagerEntity>()?;
    villager
        .mob_entity
        .try_with_brain(villager, |brain| {
            brain
                .get(types::INTERACTION_TARGET)
                .map(|target| target.get_entity().entity_id)
        })
        .flatten()
}

pub fn play_tag_with_other_kids() -> OneShot {
    const MAX_CHASERS_PER_TARGET: i32 = 5;
    OneShot::new(
        "PlayTagWithOtherKids",
        vec![
            (
                types::VISIBLE_VILLAGER_BABIES.id(),
                MemoryStatus::ValuePresent,
            ),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::INTERACTION_TARGET.id(), MemoryStatus::Registered),
        ],
        |tick| {
            if tick.mob.get_random().random_range(0..10) != 0 {
                return false;
            }
            let friends: Vec<Arc<dyn EntityBase>> = tick
                .state
                .get(types::VISIBLE_VILLAGER_BABIES)
                .cloned()
                .unwrap_or_default();
            let my_id = tick.mob.get_entity().entity_id;
            let chasing: Vec<(Arc<dyn EntityBase>, Option<i32>)> = friends
                .iter()
                .map(|friend| (friend.clone(), interaction_target_id(friend)))
                .collect();
            let chased_by_me = chasing.iter().any(|(_, target)| *target == Some(my_id));
            let chase_kid = |tick: &mut BrainTick<'_>, kid: Arc<dyn EntityBase>| {
                tick.state.set(types::INTERACTION_TARGET, kid.clone());
                tick.state
                    .set(types::LOOK_TARGET, entity_tracker(kid.clone()));
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(Arc::new(EntityTracker::new(kid, false)), 0.6, 1),
                );
            };
            if chased_by_me {
                for _ in 0..10 {
                    if let Some(pos) = land_random_pos::get_pos(tick.mob, 20, 8)
                        && tick.world.is_village(&BlockPos::floored_v(pos))
                    {
                        tick.state
                            .set(types::WALK_TARGET, WalkTarget::from_vec(pos, 0.6, 0));
                        break;
                    }
                }
            } else {
                let mut counts: Vec<(i32, i32)> = Vec::new();
                for (_, target) in &chasing {
                    if let Some(id) = target {
                        match counts.iter_mut().find(|(known, _)| known == id) {
                            Some((_, count)) => *count += 1,
                            None => counts.push((*id, 1)),
                        }
                    }
                }
                counts.sort_by_key(|(_, count)| *count);
                let being_chased = counts
                    .iter()
                    .find(|(_, count)| *count > 0 && *count <= MAX_CHASERS_PER_TARGET)
                    .and_then(|(id, _)| {
                        friends
                            .iter()
                            .find(|friend| friend.get_entity().entity_id == *id)
                            .cloned()
                            .or_else(|| tick.world.get_entity_by_id(*id))
                    });
                if let Some(kid) = being_chased {
                    chase_kid(tick, kid);
                } else if let Some(kid) = friends.first().cloned() {
                    chase_kid(tick, kid);
                }
            }
            true
        },
    )
}

// behavior: JumpOnBed
struct JumpOnBed {
    speed_modifier: f32,
    target_bed: Option<BlockPos>,
    remaining_time_to_reach_bed: i32,
    remaining_jumps: i32,
    remaining_cooldown_until_next_jump: i32,
}

const JUMP_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::NEAREST_BED.id(), MemoryStatus::ValuePresent),
    (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
];

impl JumpOnBed {
    fn is_jumpable(world: &crate::world::World, pos: &BlockPos) -> bool {
        use pumpkin_data::tag::Taggable;
        world
            .get_block(pos)
            .has_tag(&tag::Block::MINECRAFT_VILLAGER_BABIES_CAN_JUMP_ON_BED)
    }

    fn on_or_over_bed(tick: &BrainTick<'_>) -> bool {
        let body = tick.mob.get_entity().block_pos.load();
        Self::is_jumpable(tick.world, &body) || Self::is_jumpable(tick.world, &body.down())
    }

    fn near_bed(tick: &BrainTick<'_>) -> bool {
        Self::on_or_over_bed(tick) || tick.state.has_memory_value(types::NEAREST_BED.id())
    }
}

impl Behavior for JumpOnBed {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &JUMP_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        i32::MAX / 2
    }

    fn max_duration(&self) -> i32 {
        i32::MAX / 2
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        villager(tick).is_some_and(VillagerEntity::is_baby_villager) && Self::near_bed(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(bed) = tick.state.get(types::NEAREST_BED).copied() {
            self.target_bed = Some(bed);
            self.remaining_time_to_reach_bed = 100;
            self.remaining_jumps = 3 + tick.mob.get_random().random_range(0..4);
            self.remaining_cooldown_until_next_jump = 0;
            tick.state.set(
                types::WALK_TARGET,
                WalkTarget::from_block_pos(bed, self.speed_modifier, 0),
            );
        }
    }

    fn stop(&mut self, _tick: &mut BrainTick<'_>) {
        self.target_bed = None;
        self.remaining_time_to_reach_bed = 0;
        self.remaining_jumps = 0;
        self.remaining_cooldown_until_next_jump = 0;
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let on_or_over = Self::on_or_over_bed(tick);
        villager(tick).is_some_and(VillagerEntity::is_baby_villager)
            && self
                .target_bed
                .is_some_and(|bed| Self::is_jumpable(tick.world, &bed))
            && (on_or_over || self.remaining_time_to_reach_bed > 0)
            && (!on_or_over || self.remaining_jumps > 0)
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if !Self::on_or_over_bed(tick) {
            self.remaining_time_to_reach_bed -= 1;
        } else if self.remaining_cooldown_until_next_jump > 0 {
            self.remaining_cooldown_until_next_jump -= 1;
        } else if Self::is_jumpable(tick.world, &tick.mob.get_entity().block_pos.load()) {
            tick.mob
                .get_mob_entity()
                .jump_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .jump();
            self.remaining_jumps -= 1;
            self.remaining_cooldown_until_next_jump = 5;
        }
    }

    fn debug_name(&self) -> &'static str {
        "JumpOnBed"
    }
}

pub fn jump_on_bed(speed_modifier: f32) -> Timed<impl Behavior> {
    Timed::new(JumpOnBed {
        speed_modifier,
        target_bed: None,
        remaining_time_to_reach_bed: 0,
        remaining_jumps: 0,
        remaining_cooldown_until_next_jump: 0,
    })
}

// behavior: TradeWithVillager
struct TradeWithVillager {
    trades: Vec<&'static Item>,
}

const TRADE_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::INTERACTION_TARGET.id(), MemoryStatus::ValuePresent),
    (
        types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
        MemoryStatus::ValuePresent,
    ),
];

fn is_villager_entity(entity: &Arc<dyn EntityBase>) -> bool {
    entity.get_entity().entity_type == &EntityType::VILLAGER
}

impl TradeWithVillager {
    fn throw_half_stack(
        villager: &VillagerEntity,
        predicate: impl Fn(&ItemStack) -> bool,
        target: Vector3<f64>,
    ) {
        let to_throw = {
            let mut inventory = villager
                .inventory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut thrown = None;
            for slot in inventory.iter_mut() {
                if slot.is_empty() || !predicate(slot) {
                    continue;
                }
                let count = i32::from(slot.item_count);
                let to_split = if count > i32::from(slot.get_max_stack_size()) / 2 {
                    count / 2
                } else if count > 24 {
                    count - 24
                } else {
                    continue;
                };
                let mut split = slot.clone();
                split.item_count = to_split as u8;
                slot.item_count -= to_split as u8;
                if slot.item_count == 0 {
                    *slot = ItemStack::EMPTY.clone();
                }
                thrown = Some(split);
                break;
            }
            thrown
        };
        if let Some(stack) = to_throw {
            villager.throw_item(stack, target);
        }
    }
}

impl Behavior for TradeWithVillager {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &TRADE_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        utils::target_is_valid(
            &tick.visibility(),
            types::INTERACTION_TARGET,
            is_villager_entity,
        )
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        utils::target_is_valid(
            &tick.visibility(),
            types::INTERACTION_TARGET,
            is_villager_entity,
        )
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::INTERACTION_TARGET).cloned() else {
            return;
        };
        lock_gaze_and_walk_to_each_other(tick, &target, 0.5, 2);
        if let (Some(me), Some(other)) = (
            villager(tick),
            target.cast_any().downcast_ref::<VillagerEntity>(),
        ) {
            let self_items = me.profession().requested_items();
            self.trades = other
                .profession()
                .requested_items()
                .iter()
                .copied()
                .filter(|item| !self_items.iter().any(|own| own.id == item.id))
                .collect();
        }
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::INTERACTION_TARGET).cloned() else {
            return;
        };
        let Some(me) = villager(tick) else {
            return;
        };
        let Some(other) = target.cast_any().downcast_ref::<VillagerEntity>() else {
            return;
        };
        if tick
            .mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load())
            > 5.0
        {
            return;
        }
        lock_gaze_and_walk_to_each_other(tick, &target, 0.5, 2);
        let time = tick.time;
        me.gossip_with(tick.world, tick.state, other, time);
        let is_farmer = me.profession() == VillagerProfession::Farmer;
        let target_pos = target.get_entity().pos.load();
        if me.has_excess_food() && (is_farmer || other.wants_more_food()) {
            Self::throw_half_stack(
                me,
                |stack| {
                    stack
                        .get_data_component::<pumpkin_data::data_component_impl::VillagerFoodImpl>()
                        .is_some()
                },
                target_pos,
            );
        }
        if is_farmer
            && me.count_inventory_item(&Item::WHEAT)
                > i32::from(ItemStack::new(1, &Item::WHEAT).get_max_stack_size()) / 2
        {
            Self::throw_half_stack(me, |stack| stack.item.id == Item::WHEAT.id, target_pos);
        }
        if !self.trades.is_empty()
            && self
                .trades
                .iter()
                .any(|item| me.count_inventory_item(item) > 0)
        {
            let trades = self.trades.clone();
            Self::throw_half_stack(
                me,
                |stack| trades.iter().any(|item| item.id == stack.item.id),
                target_pos,
            );
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::INTERACTION_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "TradeWithVillager"
    }
}

pub fn trade_with_villager() -> Timed<impl Behavior> {
    Timed::new(TradeWithVillager { trades: Vec::new() })
}

// behavior: VillagerMakeLove
struct VillagerMakeLove {
    birth_timestamp: i64,
}

const MAKE_LOVE_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::BREED_TARGET.id(), MemoryStatus::ValuePresent),
    (
        types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
        MemoryStatus::ValuePresent,
    ),
];

impl VillagerMakeLove {
    fn breeding_possible(tick: &BrainTick<'_>) -> bool {
        let Some(me) = villager(tick) else {
            return false;
        };
        let Some(target) = tick.state.get(types::BREED_TARGET) else {
            return false;
        };
        if !is_villager_entity(target) {
            return false;
        }
        utils::target_is_valid(&tick.visibility(), types::BREED_TARGET, is_villager_entity)
            && me.can_breed_villager()
            && target
                .cast_any()
                .downcast_ref::<VillagerEntity>()
                .is_some_and(VillagerEntity::can_breed_villager)
    }

    // behavior: takeVacantBed
    fn take_vacant_bed(tick: &BrainTick<'_>) -> Option<BlockPos> {
        let center = tick.mob.get_entity().block_pos.load();
        for (pos, poi_type) in tick.world.poi_find_all_closest_first(
            center,
            48,
            |poi_type| poi_type == "minecraft:home",
            crate::world::poi_manager::PoiOccupancy::HasSpace,
        ) {
            let _ = poi_type;
            let reachable = {
                let mob = tick.mob.get_mob_entity();
                mob.navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .create_path(&mob.living_entity, pos.0.to_f64(), 1)
                    .is_some_and(|path| path.can_reach())
            };
            if reachable && tick.world.poi_take(&pos) {
                return Some(pos);
            }
        }
        None
    }

    fn try_to_give_birth(
        tick: &BrainTick<'_>,
        me: &VillagerEntity,
        target: &Arc<dyn EntityBase>,
        other: &VillagerEntity,
    ) {
        let Some(bed) = Self::take_vacant_bed(tick) else {
            target.broadcast_status_angry();
            me.broadcast_status(EntityStatus::VillagerAngry, ActorEventID::VillagerAngry);
            return;
        };
        match me.breed_with(tick.world, other) {
            Some(child) => {
                if let Some(global) =
                    crate::entity::ai::brain::memory::GlobalPos::of(tick.world, bed)
                {
                    child.mob_entity.with_brain(child.as_ref(), |brain| {
                        brain.set(types::HOME, global);
                    });
                }
            }
            None => {
                tick.world.poi_release(&bed);
            }
        }
    }
}

trait BroadcastAngry {
    fn broadcast_status_angry(&self);
}

impl BroadcastAngry for Arc<dyn EntityBase> {
    fn broadcast_status_angry(&self) {
        let entity = self.get_entity();
        entity.world.load().send_entity_status(
            entity,
            EntityStatus::VillagerAngry,
            ActorEventID::VillagerAngry,
        );
    }
}

impl Behavior for VillagerMakeLove {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &MAKE_LOVE_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        350
    }

    fn max_duration(&self) -> i32 {
        350
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::breeding_possible(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.time <= self.birth_timestamp && Self::breeding_possible(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::BREED_TARGET).cloned() else {
            return;
        };
        lock_gaze_and_walk_to_each_other(tick, &target, 0.5, 2);
        let world = tick.world;
        world.send_entity_status(
            target.get_entity(),
            EntityStatus::InLoveHearts,
            ActorEventID::InLoveHearts,
        );
        world.send_entity_status(
            tick.mob.get_entity(),
            EntityStatus::InLoveHearts,
            ActorEventID::InLoveHearts,
        );
        let duration = 275 + tick.mob.get_random().random_range(0..50);
        self.birth_timestamp = tick.time + i64::from(duration);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::BREED_TARGET).cloned() else {
            return;
        };
        let (Some(me), Some(other)) = (
            villager(tick),
            target.cast_any().downcast_ref::<VillagerEntity>(),
        ) else {
            return;
        };
        if tick
            .mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load())
            > 5.0
        {
            return;
        }
        lock_gaze_and_walk_to_each_other(tick, &target, 0.5, 2);
        if tick.time >= self.birth_timestamp {
            me.eat_and_digest_food();
            other.eat_and_digest_food();
            Self::try_to_give_birth(tick, me, &target, other);
        } else if tick.mob.get_random().random_range(0..35) == 0 {
            tick.world.send_entity_status(
                target.get_entity(),
                EntityStatus::LoveHearts,
                ActorEventID::LoveHearts,
            );
            tick.world.send_entity_status(
                tick.mob.get_entity(),
                EntityStatus::LoveHearts,
                ActorEventID::LoveHearts,
            );
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::BREED_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "VillagerMakeLove"
    }
}

pub fn villager_make_love() -> Timed<impl Behavior> {
    Timed::new(VillagerMakeLove { birth_timestamp: 0 })
}

// behavior: CelebrateVillagersSurvivedRaid
struct CelebrateVillagersSurvivedRaid {
    min_duration: i32,
    max_duration: i32,
}

impl CelebrateVillagersSurvivedRaid {
    fn raid_is_victory_here(tick: &BrainTick<'_>) -> bool {
        let pos = tick.mob.get_entity().block_pos.load();
        tick.world
            .raid_state_at(&pos)
            .is_some_and(|raid| raid.victory)
    }
}

impl Behavior for CelebrateVillagersSurvivedRaid {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &[]
    }

    fn min_duration(&self) -> i32 {
        self.min_duration
    }

    fn max_duration(&self) -> i32 {
        self.max_duration
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let pos = tick.mob.get_entity().block_pos.load();
        Self::raid_is_victory_here(tick)
            && has_no_blocks_above(tick.world, tick.mob.get_entity().pos.load().y, &pos)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let pos = tick.mob.get_entity().block_pos.load();
        tick.world
            .raid_state_at(&pos)
            .is_some_and(|raid| !raid.stopped)
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let pos = tick.mob.get_entity().block_pos.load();
        tick.state.update_activity_from_schedule(tick.world, &pos);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(villager) = villager(tick) else {
            return;
        };
        if tick.mob.get_random().random_range(0..100) == 0 {
            villager.play_celebrate_sound();
        }
        let pos = tick.mob.get_entity().block_pos.load();
        if tick.mob.get_random().random_range(0..200) == 0
            && has_no_blocks_above(tick.world, tick.mob.get_entity().pos.load().y, &pos)
        {
            villager.launch_celebration_firework();
        }
    }

    fn debug_name(&self) -> &'static str {
        "CelebrateVillagersSurvivedRaid"
    }
}

pub fn celebrate_villagers_survived_raid(
    min_duration: i32,
    max_duration: i32,
) -> Timed<impl Behavior> {
    Timed::new(CelebrateVillagersSurvivedRaid {
        min_duration,
        max_duration,
    })
}
