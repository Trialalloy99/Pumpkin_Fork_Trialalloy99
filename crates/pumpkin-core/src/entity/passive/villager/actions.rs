//! Ported from `Villager`: sleeping, gossip, golem summoning, food, POI release and item handling.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::block_properties::{BedPart, WhiteBedLikeProperties as BedProperties};
use pumpkin_data::entity::{EntityPose, EntityStatus, EntityType};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use uuid::Uuid;

use super::{GossipType, VillagerEntity, VillagerProfession};
use crate::block::blocks::bed::BedBlock;
use crate::entity::ai::brain::memory::{GlobalPos, MemoryModuleType, types};
use crate::entity::ai::brain::sensing::villager::GolemSensor;
use crate::entity::ai::brain::{BrainState, memory::MemoryModuleId};
use crate::entity::item::ItemEntity;
use crate::entity::mob::Mob;
use crate::entity::passive::iron_golem::IronGolemEntity;
use crate::entity::spawn_util::{SpawnStrategy, try_spawn_mob};
use crate::entity::{Entity, EntityBase};
use crate::world::World;

const TIME_SINCE_SLEEPING_FOR_GOLEM_SPAWNING: i64 = 24_000;
const GOSSIP_COOLDOWN: i64 = 1200;
const GOSSIP_DECAY_INTERVAL: i64 = 24_000;
const MAX_GOSSIP_TOPICS: usize = 10;
const GOLEM_TALK_RANGE: f64 = 10.0;
const GOLEM_VILLAGERS_THAT_AGREE: usize = 5;
const GOSSIP_DISCARD_THRESHOLD: i32 = 2;
const EXCESS_FOOD_POINTS: i32 = 24;
const WANTED_FOOD_POINTS: i32 = 12;
const THROW_VELOCITY: f64 = 0.3;
const HAND_Y_DISTANCE_FROM_EYE: f64 = 0.3;

/// Vanilla `ReputationEventType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReputationEvent {
    ZombieVillagerCured,
    GolemKilled,
    VillagerHurt,
    VillagerKilled,
    Trade,
}

impl GossipType {
    // gossip: GossipType.decayPerTransfer
    #[must_use]
    pub const fn decay_per_transfer(self) -> i32 {
        match self {
            Self::MajorNegative => 10,
            Self::MinorNegative | Self::MajorPositive | Self::Trading => 20,
            Self::MinorPositive => 5,
        }
    }
}

/// Crop growth the farmer behaviors read (`CropBlock.isMaxAge`).
#[must_use]
pub fn crop_age(state_id: pumpkin_data::BlockStateId) -> Option<(i32, i32)> {
    use pumpkin_data::Block;
    use pumpkin_data::block_properties::{
        NetherWartLikeProperties, TorchflowerCropLikeProperties, WheatLikeProperties,
    };
    let block = Block::from_state_id(state_id);
    if block == &Block::WHEAT || block == &Block::CARROTS || block == &Block::POTATOES {
        Some((
            i32::from(WheatLikeProperties::from_state_id(state_id).age),
            7,
        ))
    } else if block == &Block::BEETROOTS {
        Some((
            i32::from(NetherWartLikeProperties::from_state_id(state_id).age),
            3,
        ))
    } else if block == &Block::TORCHFLOWER_CROP {
        Some((
            i32::from(TorchflowerCropLikeProperties::from_state_id(state_id).age),
            1,
        ))
    } else {
        None
    }
}

impl VillagerEntity {
    #[must_use]
    pub fn profession(&self) -> VillagerProfession {
        self.villager_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .profession_enum()
    }

    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .level
            .0
    }

    #[must_use]
    pub fn is_baby_villager(&self) -> bool {
        self.mob_entity.living_entity.is_baby()
    }

    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.xp.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_villager_sleeping(&self) -> bool {
        self.get_entity().pose.load() == EntityPose::Sleeping
    }

    // villager: asks the tick that owns the brain to run Villager.refreshBrain
    pub fn request_brain_refresh(&self) {
        self.brain_refresh_requested.store(true, Ordering::Relaxed);
    }

    // villager: Villager.playWorkSound
    pub fn play_work_sound(&self) {
        if let Some(sound) = self.profession().work_sound() {
            self.get_entity().play_sound(sound);
        }
    }

    pub fn broadcast_status(&self, status: EntityStatus, bedrock: ActorEventID) {
        let entity = self.get_entity();
        entity
            .world
            .load()
            .send_entity_status(entity, status, bedrock);
    }

    // villager: LivingEntity.startSleeping
    pub fn start_sleeping(&self, pos: BlockPos) -> bool {
        let entity = self.get_entity();
        let world = entity.world.load();
        let (block, state) = world.get_block_and_state(&pos);
        if block.has_tag(&tag::Block::MINECRAFT_BEDS) {
            BedBlock::set_occupied(true, &world, block, &pos, state.id);
        }
        entity.set_pose(EntityPose::Sleeping);
        entity.set_synced_data(tracked_data::villager::SLEEPING_POS_ID, Some(pos));
        entity.set_pos(pos.to_f64().add_raw(0.0, 0.6875, 0.0));
        entity.set_velocity(Vector3::default());
        true
    }

    // villager: LivingEntity.stopSleeping plus Villager.stopSleeping (LAST_WOKEN)
    pub fn stop_sleeping_in(&self, state: &mut BrainState) {
        self.stop_sleeping_body();
        state.set(
            types::LAST_WOKEN,
            self.get_entity().world.load().get_world_age(),
        );
    }

    pub fn stop_sleeping_outside(&self) {
        if !self.is_villager_sleeping() {
            return;
        }
        self.stop_sleeping_body();
        let time = self.get_entity().world.load().get_world_age();
        self.mob_entity.try_with_brain(self, |brain| {
            brain.set(types::LAST_WOKEN, time);
        });
    }

    fn stop_sleeping_body(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let sleeping_pos = entity.block_pos.load();
        for candidate in [
            sleeping_pos,
            sleeping_pos.up(),
            sleeping_pos.down(),
            sleeping_pos.north(),
            sleeping_pos.south(),
            sleeping_pos.east(),
            sleeping_pos.west(),
        ] {
            let (block, state) = world.get_block_and_state(&candidate);
            if block.has_tag(&tag::Block::MINECRAFT_BEDS)
                && BedProperties::from_state_id(state.id).occupied
                && BedProperties::from_state_id(state.id).part == BedPart::Head
            {
                BedBlock::set_occupied(false, &world, block, &candidate, state.id);
                let up = candidate.to_f64().add_raw(0.0, 0.1, 0.0);
                entity.set_pos(up);
                break;
            }
        }
        entity.set_pose(EntityPose::Standing);
        entity.set_synced_data(tracked_data::villager::SLEEPING_POS_ID, None::<BlockPos>);
    }

    // villager: releasePoi(memoryType) with the memory read from `state`
    pub fn release_poi_in(&self, state: &BrainState, memory: MemoryModuleType<GlobalPos>) {
        let Some(global) = state.get(memory) else {
            return;
        };
        let world = self.get_entity().world.load();
        if !global.is_in_world(&world) {
            return;
        }
        let Some(poi_type) = world.poi_type_at(&global.pos) else {
            return;
        };
        if self.poi_matches_memory(memory.id(), &poi_type) {
            world.poi_release(&global.pos);
        }
    }

    // villager: Villager.POI_MEMORIES predicates
    pub(super) fn poi_matches_memory(&self, memory: MemoryModuleId, poi_type: &str) -> bool {
        use crate::world::villager_poi::{poi_type_for_profession, profession_for_poi_type};
        if memory == types::HOME.id() {
            poi_type == "minecraft:home"
        } else if memory == types::JOB_SITE.id() {
            poi_type_for_profession(self.profession()) == Some(poi_type)
        } else if memory == types::POTENTIAL_JOB_SITE.id() {
            profession_for_poi_type(poi_type).is_some()
        } else if memory == types::MEETING_POINT.id() {
            poi_type == "minecraft:meeting"
        } else {
            false
        }
    }

    // villager: releaseAllPois
    pub fn release_all_pois_in(&self, state: &BrainState) {
        self.release_poi_in(state, types::HOME);
        self.release_poi_in(state, types::JOB_SITE);
        self.release_poi_in(state, types::POTENTIAL_JOB_SITE);
        self.release_poi_in(state, types::MEETING_POINT);
    }

    pub fn release_all_pois(&self) {
        self.mob_entity.try_with_brain(self, |brain| {
            self.release_all_pois_in(brain);
        });
    }

    // villager: Villager.getPlayerReputation / GossipContainer.getReputation
    #[must_use]
    pub fn gossip_reputation(&self, target: &Uuid) -> i32 {
        self.get_player_reputation(target)
    }

    // gossip: GossipContainer.add with its merge and clamp rules
    pub fn add_gossip(&self, target: Uuid, kind: GossipType, amount: i32) {
        let mut gossips = self
            .gossips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = gossips.entry(target).or_default();
        let old = entries.get(&kind).copied().unwrap_or(0);
        let sum = old + amount;
        let merged = if sum > kind.max_value() {
            kind.max_value().max(old)
        } else {
            sum
        };
        entries.insert(kind, merged);
        if merged > kind.max_value() {
            entries.insert(kind, kind.max_value());
        }
        if merged < GOSSIP_DISCARD_THRESHOLD {
            entries.remove(&kind);
        }
        if entries.is_empty() {
            gossips.remove(&target);
        }
    }

    // gossip: GossipContainer.decay
    pub fn decay_gossip_container(&self) {
        let mut gossips = self
            .gossips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for values in gossips.values_mut() {
            values.retain(|kind, value| {
                *value -= kind.daily_decay();
                *value >= GOSSIP_DISCARD_THRESHOLD
            });
        }
        gossips.retain(|_, values| !values.is_empty());
    }

    // villager: Villager.maybeDecayGossip
    pub fn maybe_decay_gossip(&self, timestamp: i64) {
        let last = self.last_gossip_decay_time.load(Ordering::Relaxed);
        if last == 0 {
            self.last_gossip_decay_time
                .store(timestamp, Ordering::Relaxed);
        } else if timestamp >= last + GOSSIP_DECAY_INTERVAL {
            self.decay_gossip_container();
            self.last_gossip_decay_time
                .store(timestamp, Ordering::Relaxed);
        }
    }

    // gossip: GossipContainer.transferFrom(source, random, maxCount)
    fn transfer_gossip_from(&self, source: &Self, max_count: usize) -> usize {
        use rand::RngExt;
        let mut entries: Vec<(Uuid, GossipType, i32)> = Vec::new();
        for (target, values) in source
            .gossips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            for (kind, value) in values {
                entries.push((*target, *kind, *value));
            }
        }
        if entries.is_empty() {
            return 0;
        }
        let mut ranges = Vec::with_capacity(entries.len());
        let mut ranges_end = 0;
        for (_, kind, value) in &entries {
            ranges_end += (value * kind.weight()).abs();
            ranges.push(ranges_end - 1);
        }
        if ranges_end <= 0 {
            return 0;
        }
        let mut selected: Vec<usize> = Vec::new();
        {
            let mut random = self.get_random();
            for _ in 0..max_count {
                let choice = random.random_range(0..ranges_end);
                let index = match ranges.binary_search(&choice) {
                    Ok(index) | Err(index) => index,
                };
                if !selected.contains(&index) {
                    selected.push(index);
                }
            }
        }
        let mut gossips = self
            .gossips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for index in &selected {
            let (target, kind, value) = entries[*index];
            let decayed = value - kind.decay_per_transfer();
            if decayed >= GOSSIP_DISCARD_THRESHOLD {
                let slot = gossips.entry(target).or_default().entry(kind).or_insert(0);
                *slot = (*slot).max(decayed);
            }
        }
        selected.len()
    }

    // villager: Villager.gossip
    pub fn gossip_with(
        &self,
        world: &Arc<World>,
        state: &mut BrainState,
        target: &Self,
        timestamp: i64,
    ) {
        let mine = self.last_gossip_share_time.load(Ordering::Relaxed);
        let theirs = target.last_gossip_share_time.load(Ordering::Relaxed);
        if (timestamp < mine || timestamp >= mine + GOSSIP_COOLDOWN)
            && (timestamp < theirs || timestamp >= theirs + GOSSIP_COOLDOWN)
        {
            let new_gossip = self.transfer_gossip_from(target, MAX_GOSSIP_TOPICS);
            self.last_gossip_share_time
                .store(timestamp, Ordering::Relaxed);
            target
                .last_gossip_share_time
                .store(timestamp, Ordering::Relaxed);
            self.spawn_golem_if_needed(world, state, timestamp, GOLEM_VILLAGERS_THAT_AGREE);
            if new_gossip > 0
                && let Some(player) = self.get_trading_player_arc()
            {
                self.update_special_prices_for(&player);
            }
        }
    }

    // villager: Villager.onReputationEventFrom
    pub fn on_reputation_event_from(&self, kind: ReputationEvent, source: &dyn EntityBase) {
        let uuid = source.get_entity().entity_uuid;
        match kind {
            ReputationEvent::ZombieVillagerCured => {
                self.add_gossip(uuid, GossipType::MajorPositive, 20);
                self.add_gossip(uuid, GossipType::MinorPositive, 25);
            }
            ReputationEvent::Trade => self.add_gossip(uuid, GossipType::Trading, 2),
            ReputationEvent::VillagerHurt => self.add_gossip(uuid, GossipType::MinorNegative, 25),
            ReputationEvent::VillagerKilled => {
                self.add_gossip(uuid, GossipType::MajorNegative, 25);
            }
            ReputationEvent::GolemKilled => {}
        }
        if let Some(player) = self.get_trading_player_arc()
            && player.get_entity().entity_uuid == uuid
        {
            self.update_special_prices_for(&player);
        }
    }

    fn golem_spawn_conditions_met(state: &BrainState, game_time: i64) -> bool {
        state
            .get(types::LAST_SLEPT)
            .is_some_and(|slept| game_time - *slept < TIME_SINCE_SLEEPING_FOR_GOLEM_SPAWNING)
    }

    // villager: Villager.wantsToSpawnGolem
    #[must_use]
    pub fn wants_to_spawn_golem_in(state: &BrainState, game_time: i64) -> bool {
        Self::golem_spawn_conditions_met(state, game_time)
            && !state.has_memory_value(types::GOLEM_DETECTED_RECENTLY.id())
    }

    // villager: Villager.spawnGolemIfNeeded
    pub fn spawn_golem_if_needed(
        &self,
        world: &Arc<World>,
        state: &mut BrainState,
        timestamp: i64,
        villagers_needed_to_agree: usize,
    ) {
        let game_time = world.get_world_age();
        if !Self::wants_to_spawn_golem_in(state, game_time) {
            return;
        }
        let area: BoundingBox = self.get_entity().bounding_box.load().expand(
            GOLEM_TALK_RANGE,
            GOLEM_TALK_RANGE,
            GOLEM_TALK_RANGE,
        );
        let nearby: Vec<Arc<dyn EntityBase>> = world
            .get_entities_at_box(&area)
            .into_iter()
            .filter(|entity| entity.get_entity().entity_type == &EntityType::VILLAGER)
            .collect();
        let own_id = self.get_entity().entity_id;
        let wanting = nearby
            .iter()
            .filter(|entity| {
                if entity.get_entity().entity_id == own_id {
                    return true;
                }
                entity
                    .cast_any()
                    .downcast_ref::<Self>()
                    .and_then(|other| {
                        other.mob_entity.try_with_brain(other, |brain| {
                            Self::wants_to_spawn_golem_in(brain, game_time)
                        })
                    })
                    .unwrap_or(false)
            })
            .take(GOLEM_VILLAGERS_THAT_AGREE)
            .count();
        let _ = timestamp;
        if wanting < villagers_needed_to_agree {
            return;
        }
        let spawned = try_spawn_mob(
            &EntityType::IRON_GOLEM,
            IronGolemEntity::new,
            world,
            &self.get_entity().block_pos.load(),
            10,
            8,
            6,
            SpawnStrategy::LegacyIronGolem,
            false,
        );
        if spawned.is_some() {
            for villager in &nearby {
                if villager.get_entity().entity_id == own_id {
                    GolemSensor::golem_detected(state);
                } else if let Some(other) = villager.cast_any().downcast_ref::<Self>() {
                    other.mob_entity.try_with_brain(other, |brain| {
                        GolemSensor::golem_detected(brain);
                    });
                }
            }
        }
    }

    // villager: Villager.hasExcessFood
    #[must_use]
    pub fn has_excess_food(&self) -> bool {
        self.count_food_points_in_inventory() >= EXCESS_FOOD_POINTS
    }

    // villager: Villager.wantsMoreFood
    #[must_use]
    pub fn wants_more_food(&self) -> bool {
        self.count_food_points_in_inventory() < WANTED_FOOD_POINTS
    }

    // villager: Villager.hasFarmSeeds
    #[must_use]
    pub fn has_farm_seeds(&self) -> bool {
        self.inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|stack| {
                !stack.is_empty()
                    && stack
                        .item
                        .has_tag(&tag::Item::MINECRAFT_VILLAGER_PLANTABLE_SEEDS)
            })
    }

    // villager: Villager.canBreed
    #[must_use]
    pub fn can_breed_villager(&self) -> bool {
        self.food_level.load(Ordering::Relaxed) + self.count_food_points_in_inventory()
            >= WANTED_FOOD_POINTS
            && !self.is_villager_sleeping()
            && self.get_entity().age.load(Ordering::Relaxed) == 0
    }

    // villager: Villager.eatAndDigestFood
    pub fn eat_and_digest_food(&self) {
        self.eat_until_full();
        self.food_level
            .fetch_sub(WANTED_FOOD_POINTS, Ordering::Relaxed);
    }

    // inventory: SimpleContainer.countItem
    #[must_use]
    pub fn count_inventory_item(&self, item: &Item) -> i32 {
        self.inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|stack| stack.item.id == item.id)
            .map(|stack| i32::from(stack.item_count))
            .sum()
    }

    // inventory: SimpleContainer.removeItemType
    pub fn remove_inventory_item_type(&self, item: &Item, mut amount: i32) {
        let mut inventory = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for stack in inventory.iter_mut() {
            if amount <= 0 {
                break;
            }
            if stack.item.id == item.id && !stack.is_empty() {
                let take = amount.min(i32::from(stack.item_count));
                stack.item_count -= take as u8;
                amount -= take;
                if stack.item_count == 0 {
                    *stack = ItemStack::EMPTY.clone();
                }
            }
        }
    }

    // inventory: SimpleContainer.addItem, returns what did not fit
    pub fn add_inventory_item(&self, mut stack: ItemStack) -> ItemStack {
        let mut inventory = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for slot in inventory.iter_mut() {
            if stack.is_empty() {
                break;
            }
            if !slot.is_empty()
                && slot.are_items_and_components_equal(&stack)
                && slot.item_count < slot.get_max_stack_size()
            {
                let room = slot.get_max_stack_size() - slot.item_count;
                let moved = room.min(stack.item_count);
                slot.item_count += moved;
                stack.item_count -= moved;
            }
        }
        for slot in inventory.iter_mut() {
            if stack.is_empty() {
                break;
            }
            if slot.is_empty() {
                *slot = stack.clone();
                stack.item_count = 0;
            }
        }
        if stack.item_count == 0 {
            ItemStack::EMPTY.clone()
        } else {
            stack
        }
    }

    // inventory: SimpleContainer.canAddItem
    #[must_use]
    pub fn can_add_inventory_item(&self, stack: &ItemStack) -> bool {
        self.inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|slot| {
                slot.is_empty()
                    || (slot.are_items_and_components_equal(stack)
                        && slot.item_count < slot.get_max_stack_size())
            })
    }

    // villager: Villager.wantsToPickUp
    #[must_use]
    pub fn villager_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        use pumpkin_data::data_component_impl::VillagerFoodImpl;
        (stack.item.has_tag(&tag::Item::MINECRAFT_VILLAGER_PICKS_UP)
            || stack.get_data_component::<VillagerFoodImpl>().is_some()
            || self
                .profession()
                .requested_items()
                .iter()
                .any(|item| item.id == stack.item.id))
            && self.can_add_inventory_item(stack)
    }

    // behavior utils: BehaviorUtils.throwItem
    pub fn throw_item(&self, stack: ItemStack, target: Vector3<f64>) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let hand_y = pos.y + entity.get_eye_height() - HAND_Y_DISTANCE_FROM_EYE;
        let spawn = Vector3::new(pos.x, hand_y, pos.z);
        let direction = (target - pos).normalize();
        let item_entity = Entity::new(world.clone(), spawn, &EntityType::ITEM);
        let item = ItemEntity::new(item_entity, stack);
        item.set_thrower(entity.entity_uuid);
        item.get_entity().set_velocity(Vector3::new(
            direction.x * THROW_VELOCITY,
            direction.y * THROW_VELOCITY,
            direction.z * THROW_VELOCITY,
        ));
        item.set_pickup_delay(ItemEntity::DEFAULT_PICKUP_DELAY);
        world.spawn_entity(Arc::new(item));
    }

    pub fn play_celebrate_sound(&self) {
        self.get_entity()
            .play_sound(pumpkin_data::sound::Sound::EntityVillagerCelebrate);
    }

    fn get_trading_player_arc(&self) -> Option<Arc<crate::entity::player::Player>> {
        use crate::entity::mob::Mob;
        self.get_trading_player()
    }

    fn update_special_prices_for(&self, player: &crate::entity::player::Player) {
        self.update_special_prices(player);
    }
}

// profession: secondaryPoi blocks, only the farmer has one
#[must_use]
pub fn secondary_poi_block(profession: VillagerProfession) -> Option<&'static pumpkin_data::Block> {
    (profession == VillagerProfession::Farmer).then_some(&pumpkin_data::Block::FARMLAND)
}

// villager type: VillagerType.byBiome
#[must_use]
pub fn villager_type_for_biome(registry_id: &str) -> super::VillagerType {
    use super::VillagerType;
    let name = registry_id
        .strip_prefix("minecraft:")
        .unwrap_or(registry_id);
    match name {
        "badlands" | "desert" | "eroded_badlands" | "wooded_badlands" => VillagerType::Desert,
        "bamboo_jungle" | "jungle" | "sparse_jungle" => VillagerType::Jungle,
        "savanna_plateau" | "savanna" | "windswept_savanna" => VillagerType::Savanna,
        "deep_frozen_ocean" | "frozen_ocean" | "frozen_river" | "ice_spikes" | "snowy_beach"
        | "snowy_taiga" | "snowy_plains" | "grove" | "snowy_slopes" | "frozen_peaks"
        | "jagged_peaks" => VillagerType::Snow,
        "swamp" | "mangrove_swamp" => VillagerType::Swamp,
        "old_growth_spruce_taiga"
        | "old_growth_pine_taiga"
        | "windswept_gravelly_hills"
        | "windswept_hills"
        | "taiga"
        | "windswept_forest" => VillagerType::Taiga,
        _ => VillagerType::Plains,
    }
}

const FIREWORK_COLORS: [i32; 16] = [
    0x00F0_F0F0,
    0x00EB_8844,
    0x00C3_54CD,
    0x0066_89D3,
    0x00DE_CF2A,
    0x0041_CD34,
    0x00D8_8198,
    0x0043_4343,
    0x00AB_ABAB,
    0x0028_7697,
    0x007B_2FBE,
    0x0025_3192,
    0x0051_301A,
    0x003B_511A,
    0x00B3_312C,
    0x001E_1B1B,
];

impl VillagerEntity {
    // villager: Villager.change of profession through a job site, the brain is rebuilt after the tick
    pub fn change_profession(&self, profession: VillagerProfession) {
        let data = *self
            .villager_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.set_villager_data(super::VillagerData::new(
            data.type_enum(),
            profession,
            data.level.0,
        ));
        self.request_brain_refresh();
    }

    #[must_use]
    pub fn was_hurt_recently(&self) -> bool {
        self.mob_entity
            .living_entity
            .hurt_cooldown
            .load(Ordering::Relaxed)
            > 0
    }

    pub(super) fn clear_trading_player_state(&self) {
        self.is_trading.store(false, Ordering::Relaxed);
        *self
            .trading_player
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.reset_special_prices();
    }

    // villager: tellWitnessesThatIWasMurdered
    fn tell_witnesses_that_i_was_murdered(&self, murderer: &dyn EntityBase) {
        let witnesses: Vec<Arc<dyn EntityBase>> = self
            .mob_entity
            .try_with_brain(self, |brain| {
                brain
                    .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                    .map(|visible| visible.nearby().to_vec())
            })
            .flatten()
            .unwrap_or_default();
        for witness in witnesses {
            if let Some(villager) = witness.cast_any().downcast_ref::<Self>() {
                villager.on_reputation_event_from(ReputationEvent::VillagerKilled, murderer);
            }
        }
    }

    // villager: Villager.die
    pub(super) fn villager_died(&self, killer: Option<&dyn EntityBase>) {
        if let Some(killer) = killer {
            self.tell_witnesses_that_i_was_murdered(killer);
        }
        self.stop_sleeping_outside();
        self.release_all_pois();
    }

    // villager: Villager.getBreedOffspring and VillagerMakeLove.breed
    pub fn breed_with(&self, world: &Arc<World>, partner: &Self) -> Option<Arc<Self>> {
        use rand::RngExt;
        let roll = rand::rng().random::<f64>();
        let own_type = self.villager_data.lock().ok()?.type_enum();
        let partner_type = partner.villager_data.lock().ok()?.type_enum();
        let village_type = if roll < 0.5 {
            villager_type_for_biome(
                world
                    .get_biome(&self.get_entity().block_pos.load())
                    .registry_id,
            )
        } else if roll < 0.75 {
            own_type
        } else {
            partner_type
        };
        let pos = self.get_entity().pos.load();
        let child = Self::new(Entity::new(world.clone(), pos, &EntityType::VILLAGER));
        child.set_villager_data(super::VillagerData::new(
            village_type,
            VillagerProfession::None,
            1,
        ));
        child.villager_data_finalized.store(true, Ordering::Relaxed);
        self.get_entity().age.store(6000, Ordering::Relaxed);
        partner.get_entity().age.store(6000, Ordering::Relaxed);
        crate::entity::ageable::AgeableMob::set_age(child.as_ref(), -24_000);
        world.spawn_entity(child.clone());
        world.send_entity_status(
            child.get_entity(),
            EntityStatus::LoveHearts,
            ActorEventID::LoveHearts,
        );
        Some(child)
    }

    // behavior: CelebrateVillagersSurvivedRaid.getFirework
    pub fn launch_celebration_firework(&self) {
        use pumpkin_data::data_component::DataComponent;
        use pumpkin_data::data_component_impl::{
            DataComponentImpl, FireworkExplosionImpl, FireworkExplosionShape, FireworksImpl,
        };
        use rand::RngExt;
        let (color, flight) = {
            let mut random = rand::rng();
            (
                FIREWORK_COLORS[random.random_range(0..FIREWORK_COLORS.len())],
                random.random_range(0..3),
            )
        };
        let mut stack = ItemStack::new(1, &Item::FIREWORK_ROCKET);
        stack.patch.push((
            DataComponent::Fireworks,
            Some(
                FireworksImpl::new(
                    flight,
                    vec![FireworkExplosionImpl::new(
                        FireworkExplosionShape::Burst,
                        vec![color],
                        Vec::new(),
                        false,
                        false,
                    )],
                )
                .to_dyn(),
            ),
        ));
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let rocket =
            crate::entity::projectile::firework_rocket::FireworkRocketEntity::new_shot_with_stack(
                Entity::new(
                    world.clone(),
                    entity.pos.load(),
                    &EntityType::FIREWORK_ROCKET,
                ),
                entity,
                stack,
            );
        world.spawn_entity(Arc::new(rocket));
    }
}

impl crate::entity::ageable::AgeableMob for VillagerEntity {
    fn get_ageable_data(&self) -> &crate::entity::ageable::AgeableData {
        &self.ageable_data
    }

    // villager: ageBoundaryReached rebuilds the brain for the new age group
    fn age_boundary_reached(&self) {
        self.request_brain_refresh();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use pumpkin_nbt::compound::NbtCompound;

    use super::*;
    use crate::entity::ageable::AgeableMob;
    use crate::entity::ai::test_support::world;
    use crate::entity::mob::Mob;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn villager_in(world: &Arc<World>) -> Arc<VillagerEntity> {
        VillagerEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::VILLAGER,
        ))
    }

    #[tokio::test]
    async fn the_brain_follows_the_age_group_and_ticks_without_a_world() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let adult = villager_in(&world);
        for _ in 0..5 {
            adult.mob_entity.tick_brain(adult.as_ref());
        }
        let adult_core = adult.mob_entity.with_brain(adult.as_ref(), |brain| {
            brain.activity_requirements_are_met(pumpkin_data::environment_attribute::Activity::Work)
        });
        // work needs a job site, the baby brain has no work activity at all
        assert!(!adult_core);

        let baby = villager_in(&world);
        baby.set_age(-24_000);
        baby.mob_entity.refresh_brain(baby.as_ref());
        let plays = baby.mob_entity.with_brain(baby.as_ref(), |brain| {
            brain.activity_requirements_are_met(pumpkin_data::environment_attribute::Activity::Play)
        });
        world.level.shutdown().await;
        assert!(plays);
        Ok(())
    }

    #[tokio::test]
    async fn legacy_home_and_job_site_move_into_the_brain() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let villager = villager_in(&world);
        let mut nbt = NbtCompound::new();
        for (key, value) in [
            ("HomeX", 4),
            ("HomeY", 65),
            ("HomeZ", 6),
            ("JobSiteX", 9),
            ("JobSiteY", 64),
            ("JobSiteZ", 9),
        ] {
            nbt.put_int(key, value);
        }
        villager.mob_read_nbt(&nbt);
        let (home, job_site) = villager.mob_entity.with_brain(villager.as_ref(), |brain| {
            (
                brain.get(types::HOME).map(|home| home.pos),
                brain.get(types::JOB_SITE).map(|site| site.pos),
            )
        });
        world.level.shutdown().await;
        assert_eq!(home, Some(BlockPos::new(4, 65, 6)));
        assert_eq!(job_site, Some(BlockPos::new(9, 64, 9)));
        Ok(())
    }

    #[tokio::test]
    async fn gossip_is_capped_and_forgotten_below_the_threshold() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let villager = villager_in(&world);
        let player = Uuid::from_u128(7);
        villager.add_gossip(player, GossipType::MajorPositive, 15);
        villager.add_gossip(player, GossipType::MajorPositive, 15);
        assert_eq!(villager.get_player_reputation(&player), 5 * 20);
        villager.add_gossip(player, GossipType::MinorPositive, 2);
        villager.decay_gossip_container();
        // minor positive decays by one per day and is dropped below two
        assert_eq!(villager.get_player_reputation(&player), 5 * 20);
        world.level.shutdown().await;
        Ok(())
    }
}
