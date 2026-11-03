use super::{Entity, EntityBase, ai::pathfinder::Navigator, living::LivingEntity};
use crate::block::blocks::bubble_column::SURFACE_UPWARD_ACCELERATION;
use crate::entity::ai::behavior::temperament::Temperament;
use crate::entity::ai::brain::Brain;
use crate::entity::ai::brain::memory::PackedMemories;
use crate::entity::ai::brain::memory::codec::{block_pos_from_tag, block_pos_to_tag};
use crate::entity::ai::control::body_rotation_control::BodyRotationControl;
use crate::entity::ai::control::jump_control::JumpControl;
use crate::entity::ai::control::look_control::LookControl;
use crate::entity::ai::control::move_control::MoveControl;
use crate::entity::ai::control::{LookControlTrait, MoveControlTrait};
use crate::entity::ai::goal::{Controls, GoalReach, goal_selector::GoalSelector};
use crate::entity::ai::sensing::Sensing;
use crate::entity::player::Player;
use crate::entity::predicate::EntityPredicate;
use crate::entity::projectile::arrow::ArrowEntity;
use crate::entity::vehicle::boat::BoatEntity;
use crate::plugin::api::events::entity::entity_target::{EntityTargetEvent, TargetReason};
use crate::server::Server;
use crate::world::World;
use crate::world::brightness::DAYLIGHT_BRIGHTNESS;
use crate::world::entity_islands::{self, AuditedReach};
use crate::world::game_event::notified_area;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_data::{Block, BlockDirection, BlockId, BlockState};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::Difficulty;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::random::RandomGenerator;
use pumpkin_util::random::xoroshiro128::Xoroshiro;
use pumpkin_util::version::JavaMinecraftVersion;
use rand::RngExt;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock};
use uuid::Uuid;

pub mod bat;
pub mod blaze;
pub mod breeze;
pub mod breeze_ai;
pub mod cave_spider;
pub mod conversion;
pub mod creaking;
pub mod creaking_ai;
pub mod creeper;
pub mod crossbow_attack_mob;
pub mod enderman;
pub mod endermite;
pub mod equipment;
pub mod evoker;
pub mod ghast;
pub mod giant;
pub mod guardian;
pub mod hoglin;
pub mod illusioner;
pub mod looting;
pub mod magma_cube;
pub mod metadata;
pub mod neutral;
pub mod patrol;
pub mod phantom;
pub mod piglin;
pub mod piglin_ai;
pub mod piglin_brute;
pub mod pillager;
pub mod raider;
pub mod ravager;
pub mod shulker;
pub mod silverfish;
pub mod skeleton;
pub mod slime;
pub mod spawn;
pub mod spellcaster;
pub mod spider;
pub mod sulfur_cube;
pub mod sulfur_cube_ai;
pub mod sun_burn;
pub mod vex;
pub mod vindicator;
pub mod warden;
pub mod warden_spawn_tracker;
pub mod witch;
pub mod zoglin;
pub mod zombie;
pub mod zombified_piglin;

pub use metadata::MobMetadata;

/// The mob whose navigation and movement control a mob drives: its own, or the mob it steers.
pub struct SteeredMob<'a> {
    rider: &'a MobEntity,
    vehicle: Option<Arc<dyn EntityBase>>,
}

impl SteeredMob<'_> {
    #[must_use]
    pub fn mob_entity(&self) -> &MobEntity {
        self.vehicle
            .as_deref()
            .and_then(EntityBase::get_mob)
            .map_or(self.rider, |mob| mob.get_mob_entity())
    }

    pub fn navigator(&self) -> std::sync::MutexGuard<'_, Navigator> {
        self.mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn move_control(&self) -> std::sync::MutexGuard<'_, Box<dyn MoveControlTrait>> {
        self.mob_entity()
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub struct MobEntity {
    pub living_entity: LivingEntity,
    pub goals_selector: GoalSelector,
    pub target_selector: GoalSelector,
    /// Move and look controls lock this while held, so never lock a control while holding it.
    pub navigator: std::sync::Mutex<Navigator>,
    pending_navigation_updates: crossbeam::queue::SegQueue<(BlockPos, Uuid, u64)>,
    pub target: std::sync::Mutex<Option<Arc<dyn EntityBase>>>,
    pub look_control: std::sync::Mutex<Box<dyn LookControlTrait>>,
    pub sensing: std::sync::Mutex<Sensing>,
    pub move_control: std::sync::Mutex<Box<dyn MoveControlTrait>>,
    pub jump_control: std::sync::Mutex<JumpControl>,
    pub body_rotation_control: std::sync::Mutex<BodyRotationControl>,
    brain: std::sync::Mutex<Option<Brain>>,
    brain_memory_clear_requested: AtomicBool,
    removal_cleanup_done: AtomicBool,
    pub position_target: AtomicCell<BlockPos>,
    pub position_target_range: AtomicI32,
    pub love_ticks: AtomicI32,
    pub breeding_cooldown: AtomicI32,
    pub breeder: AtomicCell<Option<Uuid>>,
    pub persistence_required: AtomicBool,
    pub death_loot_table: std::sync::Mutex<Option<String>>,
    pub death_loot_table_seed: AtomicI64,
    pub ambient_sound_time: AtomicI32,
    pending_riders: std::sync::Mutex<Vec<Arc<dyn EntityBase>>>,
    pending_vehicle: std::sync::Mutex<Option<(Arc<dyn EntityBase>, bool)>>,
    pub spawn_reason: std::sync::atomic::AtomicU8,
    applied_max_fall_distance: std::sync::atomic::AtomicI32,
    mob_flags: AtomicU8,
}
impl MobEntity {
    /// Vanilla `Mob.getNavigation`/`getMoveControl`: a rider steering a mob drives that mob.
    /// Only the mob's own AI step ticks its own navigator and move control directly.
    pub fn steered(&self) -> SteeredMob<'_> {
        let entity = &self.living_entity.entity;
        let vehicle = entity.get_vehicle().filter(|vehicle| {
            vehicle.get_mob().is_some()
                && vehicle
                    .get_controlling_passenger()
                    .is_some_and(|rider| rider.get_entity().entity_id == entity.entity_id)
        });
        SteeredMob {
            rider: self,
            vehicle,
        }
    }

    /// Vanilla `Mob.onAttributeUpdated`: follow and tempt range changes resize the path search.
    pub(crate) fn on_attribute_updated(&self, attribute: u8) {
        if attribute == Attributes::FOLLOW_RANGE.id || attribute == Attributes::TEMPT_RANGE.id {
            self.steered()
                .navigator()
                .update_pathfinder_max_visited_nodes();
        }
    }

    pub fn set_speed(&self, speed: f32) {
        self.living_entity.set_speed(speed);
        let input = self.living_entity.movement_input.load();
        self.living_entity
            .movement_input
            .store(Vector3::new(input.x, input.y, f64::from(speed)));
    }

    // mob: Mob.stopInPlace
    pub fn stop_in_place(&self) {
        self.steered().navigator().stop();
        self.living_entity.set_speed(0.0);
        self.living_entity
            .movement_input
            .store(Vector3::new(0.0, 0.0, 0.0));
        self.living_entity
            .entity
            .velocity
            .store(Vector3::new(0.0, 0.0, 0.0));
        self.living_entity.entity.leash_angular_momentum.store(0.0);
    }

    const CONTROL_FLAGS_UPDATE_INTERVAL: i32 = 5;
    const AI_DISABLED_FLAG: u8 = 1;
    const LEFT_HANDED_FLAG: u8 = 2;
    const ATTACKING_FLAG: u8 = 4;
    const CAN_PICK_UP_LOOT_FLAG: u8 = 8;

    pub const MAX_WEARING_ARMOR_CHANCE: f32 = 0.15;
    pub const DEFAULT_AMBIENT_SOUND_INTERVAL: i32 = 80;
    pub const WEARING_ARMOR_UPGRADE_MATERIAL_CHANCE: f32 = 0.1087;
    pub const WEARING_ARMOR_UPGRADE_MATERIAL_ATTEMPTS: f32 = 3.0;
    pub const MAX_PICKUP_LOOT_CHANCE: f32 = 0.55;
    pub const MAX_ENCHANTED_ARMOR_CHANCE: f32 = 0.5;
    pub const MAX_ENCHANTED_WEAPON_CHANCE: f32 = 0.25;
    pub const GUARANTEED_DROP_CHANCE: f32 = 2.0;
    pub const EQUIPMENT_POPULATION_ORDER: [EquipmentSlot; 4] = [
        EquipmentSlot::HEAD,
        EquipmentSlot::CHEST,
        EquipmentSlot::LEGS,
        EquipmentSlot::FEET,
    ];

    #[must_use]
    pub const fn get_equipment_for_slot(
        slot: &EquipmentSlot,
        armor_type: i32,
    ) -> Option<&'static Item> {
        match slot {
            EquipmentSlot::Head(_) => match armor_type {
                0 => Some(&Item::LEATHER_HELMET),
                1 => Some(&Item::COPPER_HELMET),
                2 => Some(&Item::GOLDEN_HELMET),
                3 => Some(&Item::CHAINMAIL_HELMET),
                4 => Some(&Item::IRON_HELMET),
                5 => Some(&Item::DIAMOND_HELMET),
                _ => None,
            },
            EquipmentSlot::Chest(_) => match armor_type {
                0 => Some(&Item::LEATHER_CHESTPLATE),
                1 => Some(&Item::COPPER_CHESTPLATE),
                2 => Some(&Item::GOLDEN_CHESTPLATE),
                3 => Some(&Item::CHAINMAIL_CHESTPLATE),
                4 => Some(&Item::IRON_CHESTPLATE),
                5 => Some(&Item::DIAMOND_CHESTPLATE),
                _ => None,
            },
            EquipmentSlot::Legs(_) => match armor_type {
                0 => Some(&Item::LEATHER_LEGGINGS),
                1 => Some(&Item::COPPER_LEGGINGS),
                2 => Some(&Item::GOLDEN_LEGGINGS),
                3 => Some(&Item::CHAINMAIL_LEGGINGS),
                4 => Some(&Item::IRON_LEGGINGS),
                5 => Some(&Item::DIAMOND_LEGGINGS),
                _ => None,
            },
            EquipmentSlot::Feet(_) => match armor_type {
                0 => Some(&Item::LEATHER_BOOTS),
                1 => Some(&Item::COPPER_BOOTS),
                2 => Some(&Item::GOLDEN_BOOTS),
                3 => Some(&Item::CHAINMAIL_BOOTS),
                4 => Some(&Item::IRON_BOOTS),
                5 => Some(&Item::DIAMOND_BOOTS),
                _ => None,
            },
            _ => None,
        }
    }

    #[must_use]
    pub fn new(entity: Entity) -> Self {
        Self {
            living_entity: LivingEntity::new(entity),
            goals_selector: GoalSelector::default(),
            target_selector: GoalSelector::default(),
            navigator: std::sync::Mutex::new(Navigator::default()),
            pending_navigation_updates: crossbeam::queue::SegQueue::new(),
            target: std::sync::Mutex::new(None),
            look_control: std::sync::Mutex::new(Box::new(LookControl::default())),
            sensing: std::sync::Mutex::new(Sensing::default()),
            move_control: std::sync::Mutex::new(Box::new(MoveControl::default())),
            jump_control: std::sync::Mutex::new(JumpControl::default()),
            body_rotation_control: std::sync::Mutex::new(BodyRotationControl::default()),
            brain: std::sync::Mutex::new(None),
            brain_memory_clear_requested: AtomicBool::new(false),
            removal_cleanup_done: AtomicBool::new(false),
            position_target: AtomicCell::new(BlockPos::ZERO),
            position_target_range: AtomicI32::new(-1),
            love_ticks: AtomicI32::new(0),
            breeding_cooldown: AtomicI32::new(0),
            breeder: AtomicCell::new(None),
            persistence_required: AtomicBool::new(false),
            death_loot_table: std::sync::Mutex::new(None),
            death_loot_table_seed: AtomicI64::new(0),
            ambient_sound_time: AtomicI32::new(0),
            pending_riders: std::sync::Mutex::new(Vec::new()),
            pending_vehicle: std::sync::Mutex::new(None),
            spawn_reason: std::sync::atomic::AtomicU8::new(
                crate::entity::mob::spawn::SpawnReason::Command as u8,
            ),
            applied_max_fall_distance: std::sync::atomic::AtomicI32::new(3),
            mob_flags: AtomicU8::new(0),
        }
    }

    pub fn has_position_target(&self) -> bool {
        self.position_target_range.load(Relaxed) != -1
    }

    pub fn is_in_position_target_range(&self) -> bool {
        self.is_in_position_target_range_pos(&self.living_entity.entity.block_pos.load())
    }

    pub fn is_in_position_target_range_pos(&self, block_pos: &BlockPos) -> bool {
        let position_target_range = self.position_target_range.load(Relaxed);
        if position_target_range == -1 {
            true
        } else {
            self.position_target.load().squared_distance(block_pos)
                < position_target_range * position_target_range
        }
    }

    pub fn set_attacking(&self, attacking: bool) {
        self.set_mob_flag(Self::ATTACKING_FLAG, attacking);
    }

    pub fn is_attacking(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::ATTACKING_FLAG) != 0
    }

    pub fn set_left_handed(&self, left_handed: bool) {
        self.set_mob_flag(Self::LEFT_HANDED_FLAG, left_handed);
    }

    pub fn can_pick_up_loot(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::CAN_PICK_UP_LOOT_FLAG) != 0
    }

    pub fn set_can_pick_up_loot(&self, value: bool) {
        self.set_mob_flag(Self::CAN_PICK_UP_LOOT_FLAG, value);
    }

    pub fn is_left_handed(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::LEFT_HANDED_FLAG) != 0
    }

    pub fn set_no_ai(&self, no_ai: bool) {
        self.set_mob_flag(Self::AI_DISABLED_FLAG, no_ai);
    }

    pub fn is_no_ai(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::AI_DISABLED_FLAG) != 0
    }

    pub(crate) fn on_navigation_block_changed(
        &self,
        position: BlockPos,
        world: Uuid,
        sequence: u64,
    ) {
        self.pending_navigation_updates
            .push((position, world, sequence));
        self.process_navigation_updates();
    }

    pub(crate) fn process_navigation_updates(&self) {
        if self.pending_navigation_updates.is_empty() {
            return;
        }
        let mut navigator = match self.navigator.try_lock() {
            Ok(navigator) => navigator,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        self.apply_navigation_updates(&mut navigator);
    }

    fn apply_navigation_updates(&self, navigator: &mut Navigator) {
        // A search can enqueue more changes; leave those for the next owner boundary.
        for _ in 0..self.pending_navigation_updates.len() {
            let Some((position, world, sequence)) = self.pending_navigation_updates.pop() else {
                break;
            };
            if self.living_entity.entity.world.load().uuid == world
                && navigator.get_path().is_some_and(|path| {
                    path.world_change
                        .is_none_or(|(path_world, seen)| path_world == world && seen < sequence)
                })
                && navigator.should_recompute_path(&self.living_entity, position)
            {
                navigator.recompute_path(&self.living_entity);
            }
        }
    }

    /// Vanilla `Mob.serverAiStep`: sensing, goals, navigation and controls.
    pub fn server_ai_step(&self, mob: &dyn Mob, caller: &dyn EntityBase) {
        self.living_entity.no_action_time.fetch_add(1, Relaxed);
        let tick_count = self.living_entity.entity.tick_count.load(Relaxed);
        let entity_id = self.living_entity.entity.entity_id;

        self.process_navigation_updates();

        self.sensing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tick();

        if (tick_count + entity_id) % 2 != 0 && tick_count > 1 {
            GoalSelector::tick_goals(&self.target_selector, mob, false);
            GoalSelector::tick_goals(&self.goals_selector, mob, false);
        } else {
            GoalSelector::tick(&self.target_selector, mob);
            GoalSelector::tick(&self.goals_selector, mob);
        }

        let navigation_target = {
            let mut navigator = self
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.apply_navigation_updates(&mut navigator);
            // bee: navigation pauses while pollinating (Bee.createNavigation tick)
            if mob.ticks_navigation() {
                navigator.tick(&self.living_entity)
            } else {
                None
            }
        };

        // Vanilla PathNavigation.tick hands its waypoint to `mob.getMoveControl()`.
        if let Some(target) = navigation_target {
            self.steered().move_control().set_wanted_position(
                target.position.x,
                target.position.y,
                target.position.z,
                target.speed_modifier,
            );
        }

        mob.custom_server_ai_step(caller);

        {
            let mut move_control = self
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            move_control.tick(mob);
        };

        {
            let mut look_control = self
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            look_control.tick(mob);
        };

        self.jump_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tick(mob);
    }

    fn update_control_flags(&self, mob: &dyn Mob) {
        let no_controller = mob
            .get_controlling_passenger()
            .is_none_or(|passenger| passenger.get_mob().is_none());
        let not_in_boat = self
            .living_entity
            .entity
            .get_vehicle()
            .is_none_or(|vehicle| !(vehicle.as_ref() as &dyn std::any::Any).is::<BoatEntity>());
        let goals = &self.goals_selector;
        goals.set_control_enabled(Controls::MOVE, no_controller);
        goals.set_control_enabled(Controls::LOOK, no_controller);
        goals.set_control_enabled(Controls::JUMP, no_controller && not_in_boat);
    }

    pub fn clear_ai_goals(&self, mob: &dyn Mob) {
        self.goals_selector.clear(mob);
        self.target_selector.clear(mob);
    }

    #[must_use]
    pub fn drop_chance(&self, slot: &EquipmentSlot) -> f32 {
        self.living_entity
            .equipment_drop_chances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot)
            .copied()
            .unwrap_or(crate::entity::mob::equipment::DEFAULT_EQUIPMENT_DROP_CHANCE)
    }

    pub fn set_guaranteed_drop(&self, slot: &EquipmentSlot) {
        self.set_drop_chance(slot, Self::GUARANTEED_DROP_CHANCE);
    }

    pub fn set_drop_chance(&self, slot: &EquipmentSlot, chance: f32) {
        self.living_entity
            .equipment_drop_chances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slot.clone(), chance);
    }

    /// Equips `stack` and tells tracking clients, returning what was in the slot.
    pub fn set_item_slot(&self, slot: &EquipmentSlot, stack: ItemStack) -> ItemStack {
        let previous = self
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(slot, stack.clone());
        self.living_entity
            .send_equipment_changes(&[(slot.clone(), stack)]);
        previous
    }

    /// Vanilla `Mob.dropPreservedEquipment`: slots with a preserved drop chance fall to the ground.
    pub fn drop_preserved_equipment(&self) {
        let slots: Vec<EquipmentSlot> = self
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .equipment
            .iter()
            .filter(|(_, stack)| !stack.is_empty())
            .map(|(slot, _)| slot.clone())
            .collect();
        for slot in slots {
            if self.drop_chance(&slot) > 1.0 {
                let stack = self.set_item_slot(&slot, ItemStack::EMPTY.clone());
                self.living_entity.entity.spawn_at_location(stack);
            }
        }
    }

    // mob: read main hand without cloning
    pub fn with_main_hand<R>(&self, f: impl FnOnce(&ItemStack) -> R) -> R {
        let equipment = self
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(equipment
            .equipment
            .get(&EquipmentSlot::MAIN_HAND)
            .unwrap_or(ItemStack::EMPTY))
    }

    // mob: turn body + pitch towards entity (Mob.lookAt)
    pub fn look_at_entity_body(&self, target: &dyn EntityBase, max_yaw: f32, max_pitch: f32) {
        let entity = &self.living_entity.entity;
        let pos = entity.pos.load();
        let is_living = target.get_living_entity().is_some();
        let target = target.get_entity();
        let target_pos = target.pos.load();
        let xd = target_pos.x - pos.x;
        let zd = target_pos.z - pos.z;
        let yd = if is_living {
            target.get_eye_y() - entity.get_eye_y()
        } else {
            let bb = target.bounding_box.load();
            f64::midpoint(bb.min.y, bb.max.y) - entity.get_eye_y()
        };
        let sd = xd.hypot(zd);
        let yaw = (zd.atan2(xd).to_degrees() as f32) - 90.0;
        let pitch = -(yd.atan2(sd).to_degrees() as f32);
        let rotlerp = |from: f32, to: f32, max: f32| {
            from + pumpkin_util::math::wrap_degrees(to - from).clamp(-max, max)
        };
        entity
            .pitch
            .store(rotlerp(entity.pitch.load(), pitch, max_pitch));
        entity.yaw.store(rotlerp(entity.yaw.load(), yaw, max_yaw));
    }

    // mob: equipment durability loss + break event (ItemStack.hurtAndBreak, Mob.onEquippedItemBroken)
    pub fn damage_item_in_slot(&self, slot: &EquipmentSlot, amount: i32) {
        let mut stack = self
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot);
        match stack.damage_item(amount) {
            pumpkin_data::item_stack::DamageResult::Untouched => {}
            pumpkin_data::item_stack::DamageResult::Damaged => {
                self.set_item_slot(slot, stack);
            }
            pumpkin_data::item_stack::DamageResult::Broken => {
                self.set_item_slot(slot, stack);
                let entity = &self.living_entity.entity;
                entity.world.load().send_entity_status(
                    entity,
                    crate::entity::equipment_break_status(slot),
                    pumpkin_protocol::bedrock::server::actor_event::ActorEventID::None,
                );
            }
        }
    }

    pub fn set_item_slot_and_drop_when_killed(&self, slot: &EquipmentSlot, stack: ItemStack) {
        self.set_item_slot(slot, stack);
        self.set_guaranteed_drop(slot);
    }

    fn write_drop_chances(&self, nbt: &mut NbtCompound) {
        let chances = self
            .living_entity
            .equipment_drop_chances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut compound = NbtCompound::new();
        for (slot, chance) in chances.iter() {
            if *chance != crate::entity::mob::equipment::DEFAULT_EQUIPMENT_DROP_CHANCE {
                compound.put_float(slot.to_name(), *chance);
            }
        }
        if !compound.child_tags.is_empty() {
            nbt.put("drop_chances", pumpkin_nbt::tag::NbtTag::Compound(compound));
        }
    }

    fn read_drop_chances(&self, nbt: &NbtCompound) {
        let mut chances = self
            .living_entity
            .equipment_drop_chances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        chances.clear();
        let Some(compound) = nbt.get_compound("drop_chances") else {
            return;
        };
        for (name, tag) in &compound.child_tags {
            if let Some(slot) = EquipmentSlot::get_from_name(name)
                && let Some(chance) = tag.extract_float()
            {
                chances.insert(slot.clone(), chance);
            }
        }
    }

    pub(crate) fn finish_removal(&self, mob: &dyn Mob) {
        if !self.removal_cleanup_done.swap(true, Ordering::AcqRel) {
            self.clear_brain_memories();
            mob.on_removed();
        }
    }

    pub(crate) fn clear_brain_memories(&self) {
        self.brain_memory_clear_requested
            .store(true, Ordering::Release);
        self.process_brain_memory_clear();
    }

    fn process_brain_memory_clear(&self) {
        while self.brain_memory_clear_requested.load(Ordering::Acquire) {
            let mut brain = match self.brain.try_lock() {
                Ok(brain) => brain,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return,
            };
            let Some(brain) = brain.as_mut() else {
                return;
            };
            if self.take_brain_memory_clear_request() {
                brain.clear_memories();
            }
        }
    }

    pub(crate) fn take_brain_memory_clear_request(&self) -> bool {
        self.brain_memory_clear_requested.load(Ordering::Acquire)
            && self
                .brain_memory_clear_requested
                .swap(false, Ordering::AcqRel)
    }

    /// Accesses the species' brain, building it on first use.
    /// Neither `make_brain` nor the callback may lock this brain again; Brain callbacks use `BrainTick`.
    pub fn with_brain<R>(&self, mob: &dyn Mob, callback: impl FnOnce(&mut Brain) -> R) -> R {
        let result = {
            let mut slot = self
                .brain
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let brain = slot.get_or_insert_with(|| mob.make_brain(&PackedMemories::empty()));
            if self.take_brain_memory_clear_request() {
                brain.clear_memories();
            }
            callback(brain)
        };
        self.process_brain_memory_clear();
        result
    }

    /// Reads another mob's brain without waiting, `None` while that brain is busy (it may be ticking in
    /// parallel and holding its own lock).
    pub fn try_with_brain<R>(
        &self,
        mob: &dyn Mob,
        callback: impl FnOnce(&mut Brain) -> R,
    ) -> Option<R> {
        let mut slot = self.brain.try_lock().ok()?;
        let brain = slot.get_or_insert_with(|| mob.make_brain(&PackedMemories::empty()));
        Some(callback(brain))
    }

    /// Installs an already built brain, such as one restored from saved memories.
    pub fn set_brain(&self, brain: Brain) {
        *self
            .brain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(brain);
        self.process_brain_memory_clear();
    }

    /// Vanilla `refreshBrain`: stops running behaviors, then rebuilds the brain from its memories.
    pub fn refresh_brain(&self, mob: &dyn Mob) {
        let world = self.living_entity.entity.world.load_full();
        let time = world.get_world_age();
        self.with_brain(mob, |brain| {
            brain.stop_all(&world, mob, time);
            let packed = brain.pack();
            *brain = mob.make_brain(&packed);
        });
    }

    pub fn tick_brain(&self, mob: &dyn Mob) {
        let world = self.living_entity.entity.world.load_full();
        let time = world.get_world_age();
        self.with_brain(mob, |brain| brain.tick(&world, mob, time));
    }

    pub fn write_mob_nbt(&self, mob: &dyn Mob, nbt: &mut NbtCompound) {
        nbt.put_compound(
            "Brain",
            self.with_brain(mob, |brain| brain.pack().into_nbt()),
        );
        nbt.put_bool("CanPickUpLoot", self.can_pick_up_loot());
        nbt.put_bool(
            "PersistenceRequired",
            self.persistence_required.load(Relaxed),
        );
        self.write_drop_chances(nbt);
        self.living_entity.entity.write_leash_data(nbt);
        if self.has_position_target() {
            nbt.put_int("home_radius", self.position_target_range.load(Relaxed));
            nbt.put("home_pos", block_pos_to_tag(self.position_target.load()));
        }
        nbt.put_bool("LeftHanded", self.is_left_handed());
        if let Some(loot_table) = &*self
            .death_loot_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            nbt.put_string("DeathLootTable", loot_table.clone());
        }
        let death_loot_table_seed = self.death_loot_table_seed.load(Relaxed);
        if death_loot_table_seed != 0 {
            nbt.put_long("DeathLootTableSeed", death_loot_table_seed);
        }
        if self.is_no_ai() {
            nbt.put_bool("NoAI", true);
        }
    }

    pub fn read_mob_nbt(&self, nbt: &NbtCompound) {
        self.set_can_pick_up_loot(nbt.get_bool("CanPickUpLoot").unwrap_or(false));
        self.persistence_required.store(
            nbt.get_bool("PersistenceRequired").unwrap_or(false),
            Relaxed,
        );
        self.read_drop_chances(nbt);
        self.living_entity.entity.read_leash_data(nbt);
        let home_radius = nbt.get_int("home_radius").unwrap_or(-1);
        self.position_target_range.store(home_radius, Relaxed);
        if home_radius >= 0 {
            self.position_target.store(
                nbt.get("home_pos")
                    .and_then(block_pos_from_tag)
                    .unwrap_or(BlockPos::ZERO),
            );
        }
        self.set_left_handed(nbt.get_bool("LeftHanded").unwrap_or(false));
        *self
            .death_loot_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            nbt.get_string("DeathLootTable").map(str::to_owned);
        self.death_loot_table_seed
            .store(nbt.get_long("DeathLootTableSeed").unwrap_or(0), Relaxed);
        self.set_no_ai(nbt.get_bool("NoAI").unwrap_or(false));
        self.living_entity
            .apply_current_equipment_attribute_modifiers();
    }

    pub fn add_goal<G: crate::entity::ai::goal::Goal + 'static>(&self, priority: u8, goal: G) {
        self.goals_selector.add_goal(priority, Box::new(goal));
    }

    pub fn add_target_goal<G: crate::entity::ai::goal::Goal + 'static>(
        &self,
        priority: u8,
        goal: G,
    ) {
        self.target_selector.add_goal(priority, Box::new(goal));
    }

    pub fn set_target(&self, target: Option<Arc<dyn EntityBase>>) {
        let mut t = self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *t = target;
    }

    pub fn get_target(&self) -> Option<Arc<dyn EntityBase>> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_mob_flag(&self, flag: u8, value: bool) {
        let old_b = self.mob_flags.load(Ordering::Relaxed);

        let new_b = if value { old_b | flag } else { old_b & !flag };

        if new_b != old_b {
            self.mob_flags.store(new_b, Ordering::Relaxed);

            self.living_entity
                .entity
                .set_synced_data(tracked_data::mob::DATA_MOB_FLAGS_ID, new_b);
        }
    }

    pub fn is_in_love(&self) -> bool {
        self.love_ticks.load(Relaxed) > 0
    }

    pub fn set_love_ticks(&self, ticks: i32, breeder: Option<Uuid>) {
        self.love_ticks.store(ticks, Relaxed);
        self.breeder.store(breeder);
    }

    pub fn reset_love_ticks(&self) {
        self.love_ticks.store(0, Relaxed);
    }

    pub fn is_breeding_ready(&self) -> bool {
        self.living_entity.entity.age.load(Relaxed) >= 0
            && self.breeding_cooldown.load(Relaxed) <= 0
    }

    pub fn is_in_attack_range(&self, target: &dyn EntityBase) -> bool {
        const DEFAULT_ATTACK_RANGE: f64 = 0.828_427_12; // sqrt(2.04) - 0.6

        // TODO: Implement DataComponent lookup for ATTACK_RANGE when components are ready
        let max_range = DEFAULT_ATTACK_RANGE;
        let min_range = 0.0;

        let target_hitbox = target.get_entity().bounding_box.load();

        if !self.get_attack_box(max_range).intersects(&target_hitbox) {
            return false;
        }

        min_range <= 0.0 || !self.get_attack_box(min_range).intersects(&target_hitbox)
    }

    pub fn is_dark_enough_to_spawn(world: &World, pos: &BlockPos, is_thundering: bool) -> bool {
        let sky_light = world.get_sky_light_level(pos);
        if sky_light > rand::random_range(0..32) {
            return false;
        }

        let dimension = &world.dimension;
        let block_light_limit = dimension.monster_spawn_block_light_limit;

        let block_light = world.get_block_light_level(pos).unwrap_or(0);
        if block_light_limit < 15 && block_light > block_light_limit {
            return false;
        }

        // monster: sky darken counts, thunder darkens by 10 (Monster.isDarkEnoughToSpawn)
        let current_brightness = if is_thundering {
            world.get_raw_brightness(pos, 10)
        } else {
            world.get_max_local_raw_brightness(pos)
        };

        let mut random = RandomGenerator::Xoroshiro(Xoroshiro::from_seed(rand::random()));
        current_brightness <= dimension.monster_spawn_light_level.get(&mut random) as u8
    }

    pub fn check_mob_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        let below = pos.down();
        let state = world.get_block_state(&below);
        state.is_side_solid(BlockDirection::Up)
    }

    pub fn check_monster_spawn_rules(world: &World, pos: &BlockPos, is_thundering: bool) -> bool {
        if world.level_info.load().difficulty == Difficulty::Peaceful {
            return false;
        }

        if !Self::is_dark_enough_to_spawn(world, pos, is_thundering) {
            return false;
        }

        Self::check_mob_spawn_rules(world, pos)
    }

    pub fn check_any_light_monster_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        if world.level_info.load().difficulty == Difficulty::Peaceful {
            return false;
        }

        Self::check_mob_spawn_rules(world, pos)
    }

    pub fn check_surface_monsters_spawn_rules(
        world: &World,
        pos: &BlockPos,
        is_thundering: bool,
    ) -> bool {
        Self::check_monster_spawn_rules(world, pos, is_thundering) && world.can_see_sky(pos)
    }

    pub fn check_animal_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        let below = pos.down();
        world
            .get_block(&below)
            .has_tag(&tag::Block::MINECRAFT_ANIMALS_SPAWNABLE_ON)
            && Self::is_bright_enough_to_spawn(world, pos)
    }

    pub fn is_bright_enough_to_spawn(world: &World, pos: &BlockPos) -> bool {
        world.get_raw_brightness(pos, 0) > 8
    }

    pub fn check_surface_water_animal_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        let sea_level = world.sea_level;
        let min_spawn_level = sea_level - 13;
        pos.0.y >= min_spawn_level
            && pos.0.y <= sea_level
            && world
                .get_fluid(&pos.down())
                .has_tag(&tag::Fluid::MINECRAFT_WATER)
            && world.get_block(&pos.up()) == &Block::WATER
    }

    pub fn check_surface_ageable_water_creature_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        Self::check_surface_water_animal_spawn_rules(world, pos)
    }

    pub fn try_attack(&self, caller: &dyn EntityBase, target: &dyn EntityBase) -> bool {
        if self.living_entity.dead.load(Relaxed) {
            return false;
        }

        let attack_damage: f32 =
            self.living_entity
                .get_attribute_value(&Attributes::ATTACK_DAMAGE) as f32;
        let attack_damage = caller.get_mob().map_or(attack_damage, |mob| {
            mob.outgoing_attack_damage(mob.mob_attack_damage(attack_damage))
        });

        let damage_type = caller
            .get_mob()
            .map_or(DamageType::MOB_ATTACK, Mob::melee_damage_type);
        let damaged = target.damage_with_context(
            target,
            attack_damage,
            damage_type,
            None,
            Some(caller),
            Some(caller),
        );

        if damaged {
            self.living_entity
                .last_attacking_id
                .store(target.get_entity().entity_id, Relaxed);
            self.living_entity
                .last_attack_time
                .store(self.living_entity.entity.tick_count.load(Relaxed), Relaxed);
        }

        damaged
    }

    fn get_attack_box(&self, attack_range: f64) -> BoundingBox {
        let vehicle_opt = self.living_entity.entity.get_vehicle();

        let base_box = vehicle_opt.as_ref().map_or_else(
            || self.living_entity.entity.bounding_box.load(),
            |vehicle| {
                let vehicle_box = vehicle.get_entity().bounding_box.load();
                let my_box = self.living_entity.entity.bounding_box.load();

                BoundingBox {
                    min: Vector3::new(
                        my_box.min.x.min(vehicle_box.min.x),
                        my_box.min.y,
                        my_box.min.z.min(vehicle_box.min.z),
                    ),
                    max: Vector3::new(
                        my_box.max.x.max(vehicle_box.max.x),
                        my_box.max.y,
                        my_box.max.z.max(vehicle_box.max.z),
                    ),
                }
            },
        );

        base_box.expand(attack_range, 0.0, attack_range)
    }

    /// Brightness at the mob's eye (sky and block light) reaches daylight.
    pub fn is_in_daylight(&self) -> bool {
        let entity = &self.living_entity.entity;
        entity
            .world
            .load()
            .get_light_level_dependent_magic_value(&entity.get_eye_pos().to_block_pos())
            >= DAYLIGHT_BRIGHTNESS
    }

    pub fn mob_interact(
        &self,
        player: &Arc<Player>,
        item_stack: &mut ItemStack,
        can_be_leashed: bool,
    ) -> bool {
        // mob: leash handling shared with every leashable (Entity.interact)
        crate::entity::leash_interact(
            &self.living_entity.entity,
            player,
            item_stack,
            can_be_leashed,
            self.living_entity.is_baby(),
        )
    }

    pub fn check_despawn(&self, mob: &dyn Mob) {
        let entity = &self.living_entity.entity;

        let world = entity.world.load();
        let pos = entity.pos.load();
        let players = world.players.load();

        let nearest_dist_sq = players
            .iter()
            .filter(|p| p.gamemode.load() != pumpkin_util::GameMode::Spectator)
            .map(|p| {
                let pp = p.get_entity().pos.load();
                let dx = pp.x - pos.x;
                let dy = pp.y - pos.y;
                let dz = pp.z - pos.z;
                dx * dx + dy * dy + dz * dz
            })
            .fold(f64::MAX, f64::min);

        if nearest_dist_sq == f64::MAX {
            return;
        }

        let no_despawn_distance = f64::from(pumpkin_data::entity::MobCategory::NO_DESPAWN_DISTANCE);
        if nearest_dist_sq < no_despawn_distance * no_despawn_distance {
            self.living_entity.no_action_time.store(0, Relaxed);
        }

        if self.persistence_required.load(Relaxed) || mob.requires_custom_persistence() {
            return;
        }

        if (**entity.custom_name.load()).is_some() {
            return;
        }

        // Mobs like a converting zombie villager refuse to despawn (`removeWhenFarAway`).
        if !mob.remove_when_far_away(nearest_dist_sq) {
            return;
        }

        let despawn_distance = f64::from(entity.entity_type.category.despawn_distance);
        if nearest_dist_sq > despawn_distance * despawn_distance {
            mob.get_entity().remove();
            return;
        }

        if self.living_entity.no_action_time.load(Relaxed) > RANDOM_DESPAWN_IDLE_TICKS
            && mob.get_random().random_range(0..RANDOM_DESPAWN_CHANCE) == 0
            && nearest_dist_sq > no_despawn_distance * no_despawn_distance
        {
            mob.get_entity().remove();
        }
    }
}

const RANDOM_DESPAWN_IDLE_TICKS: i32 = 600;
const RANDOM_DESPAWN_CHANCE: u32 = 800;

/// Least slippery block in the game data, which lets walking speed up the most.
static LEAST_SLIPPERINESS: LazyLock<f32> = LazyLock::new(|| {
    (0..BlockId::COUNT)
        .filter_map(BlockId::new)
        .map(|id| id.to_block().slipperiness)
        .fold(f32::MAX, f32::min)
});

fn has_game_event_listener(mob: &dyn Mob) -> bool {
    let mut listens = false;
    mob.for_each_game_event_listener(&mut |_| listens = true);
    listens
}

/// Links the listener mobs that an entity's game events can reach from anywhere in `moved`:
/// the splash of entering water, plus step, swim and flap when its movement emits events.
pub(crate) fn game_event_listener_links(
    world: &World,
    emits_movement_events: bool,
    moved: &BoundingBox,
    links: &mut Vec<i32>,
) {
    let emitted: &[GameEvent] = if emits_movement_events {
        &[
            GameEvent::Splash,
            GameEvent::Step,
            GameEvent::Swim,
            GameEvent::Flap,
        ]
    } else {
        &[GameEvent::Splash]
    };
    let radius = emitted
        .iter()
        .copied()
        .map(GameEvent::notification_radius)
        .max()
        .unwrap_or(0);
    let area = BoundingBox::new(
        notified_area(moved.min, radius).min,
        notified_area(moved.max, radius).max,
    );
    links.extend(
        world
            .entity_index
            .query(world.entities.load_full(), area)
            .filter(|other| other.get_mob().is_some_and(has_game_event_listener))
            .map(|other| other.get_entity().entity_id),
    );
}

impl MobEntity {
    /// What this mob can touch next tick, if its species and goals are audited.
    pub fn audited_reach(&self, mob: &dyn Mob) -> Option<AuditedReach> {
        let living = &self.living_entity;
        let entity = &living.entity;
        // Effects, enchanted boots and attachments can act outside the audited movement path.
        if !mob.has_audited_tick()
            || !living
                .active_effects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            || !living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&EquipmentSlot::FEET)
                .is_empty()
            || entity.has_vehicle()
            || entity.has_passengers()
            || entity
                .leashed_to
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        {
            return None;
        }

        let mut goals = GoalReach::default();
        GoalSelector::tick_reach(&self.goals_selector, mob, &mut goals)?;
        GoalSelector::tick_reach(&self.target_selector, mob, &mut goals)?;

        let (route, path_length) = {
            let navigator = self
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let route = navigator
                .get_path()
                .filter(|path| !path.is_done())
                .and_then(|path| {
                    // A stopped goal can leave its faster route active.
                    goals.speed = goals.speed.max(navigator.get_speed_modifier());
                    // Area checked by `shouldRecomputePath`.
                    let end = path.get_end_node()?.pos.0.to_f64();
                    let middle = (end + entity.pos.load()) * 0.5;
                    let radius = (path.get_node_count() - path.get_next_node_index()) as f64 + 1.0;
                    Some(BoundingBox::new(middle, middle).expand(radius, radius, radius))
                });
            (route, f64::from(navigator.max_path_length(living)))
        };

        // `Mob.setSpeed` also sets the forward input to the speed.
        let speed = living.get_attribute_value(&Attributes::MOVEMENT_SPEED) * goals.speed.max(1.0);
        let block_friction = LivingEntity::compute_modified_friction(
            *LEAST_SLIPPERINESS,
            living.get_attribute_value(&Attributes::FRICTION_MODIFIER) as f32,
        );
        let acceleration =
            LivingEntity::friction_influenced_speed(speed, block_friction) * speed.min(1.0);
        let world = entity.world.load();
        let fluid_push = super::max_fluid_push_speed(world.dimension.fast_lava);
        let velocity = entity.velocity.load();
        let spawns = mob.audited_spawn_distance();
        let horizontal = velocity.x.hypot(velocity.z) + acceleration + fluid_push + spawns;
        let vertical = velocity.y.abs()
            + living.get_jump_velocity(1.0)
            + living.get_attribute_value(&Attributes::GRAVITY)
            + SURFACE_UPWARD_ACCELERATION
            + fluid_push
            + spawns;

        let mut links = entity_islands::links_of(mob);
        links.extend(goals.links);
        let moved = entity
            .bounding_box
            .load()
            .expand(horizontal, vertical, horizontal);
        game_event_listener_links(
            &world,
            mob.movement_emission().emits_events(),
            &moved,
            &mut links,
        );
        let mut reach = AuditedReach {
            bounding_box: entity.bounding_box.load(),
            motion: (horizontal, vertical),
            entities: goals.entities,
            players: goals.players,
            // Searches also check the nodes next to their furthest ones.
            block_reads: path_length + 1.0,
            route,
            changes_blocks: goals.changes_blocks,
            global: false,
            links,
        };
        reach.scan_contact_blocks(&world);
        Some(reach)
    }
}

pub trait Mob: EntityBase + Send + Sync {
    fn get_random(&self) -> rand::rngs::ThreadRng {
        rand::rng()
    }

    /// Whether the species' own tick code stays within the reach `MobEntity` computes.
    fn has_audited_tick(&self) -> bool {
        false
    }

    /// How far outside its box the species' tick code spawns entities this tick.
    fn audited_spawn_distance(&self) -> f64 {
        0.0
    }

    fn can_attack(&self, target: &dyn EntityBase) -> bool {
        let target_entity = target.get_entity();
        if target_entity.entity_type == &EntityType::GHAST {
            return false;
        }
        if let Some(tamable) = self.as_tamable()
            && tamable.is_owned_by(&target_entity.entity_uuid)
        {
            return false;
        }
        self.get_mob_entity().living_entity.can_attack(target)
    }

    /// Takes the navigation lock, so callers must not already hold it.
    fn is_navigator_idle(&self) -> bool {
        self.get_mob_entity().steered().navigator().is_idle()
    }

    fn has_line_of_sight(&self, target: &crate::entity::Entity) -> bool {
        let mob_entity = self.get_mob_entity();
        mob_entity
            .sensing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_line_of_sight(&mob_entity.living_entity.entity, target)
    }

    fn requires_custom_persistence(&self) -> bool {
        false
    }

    /// Whether daylight burns this mob. Default: entity tag `burn_in_daylight`.
    fn burns_in_daylight(&self) -> bool {
        self.get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_BURN_IN_DAYLIGHT)
    }

    /// Slot whose item shields the mob from the sun and takes the wear instead.
    fn sun_protection_slot(&self) -> EquipmentSlot {
        EquipmentSlot::HEAD
    }

    fn can_be_leashed(&self) -> bool {
        true
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        self.as_animal().is_none()
    }

    fn get_max_look_yaw_change(&self) -> f32 {
        10.0
    }

    fn get_max_look_pitch_change(&self) -> f32 {
        40.0
    }

    /// Replaces the generic `BodyRotationControl.clientTick` (Phantom.PhantomBodyRotationControl).
    /// Returns `true` when the mob handled the rotation itself.
    fn custom_body_rotation(&self) -> bool {
        false
    }

    fn get_max_head_rotation(&self) -> f32 {
        75.0
    }

    // parrot: LivingEntity.setRecordPlayingNearby (jukebox song start and stop)
    fn set_record_playing_nearby(&self, _jukebox: BlockPos, _playing: bool) {}

    // frog: Frog.calculateFallDamage reduction
    fn fall_damage_reduction(&self) -> i32 {
        0
    }

    // turtle: replaces BreedGoal.breed, true when handled (TurtleBreedGoal.breed)
    fn custom_breed(&self, _mate: &dyn EntityBase) -> bool {
        false
    }

    // cat: Animal.canMate partner check (Cat.canMate)
    fn can_mate(&self, _partner: &dyn EntityBase) -> bool {
        true
    }

    // bat: Bat.isIgnoringBlockTriggers (pressure plates, tripwires)
    fn ignores_block_triggers(&self) -> bool {
        false
    }

    // bat: Bat.isPushable false
    fn mob_is_pushable(&self) -> bool {
        true
    }

    fn ticks_navigation(&self) -> bool {
        true
    }

    // mob: melee damage source (Bee.doHurtTarget sting)
    fn melee_damage_type(&self) -> DamageType {
        DamageType::MOB_ATTACK
    }

    /// Vanilla `LookControl.resetXRotOnTick`: the look control levels the pitch each tick.
    fn look_resets_pitch(&self) -> bool {
        true
    }

    fn get_mob_entity(&self) -> &MobEntity;

    fn is_mob_immobile(&self) -> bool {
        self.get_mob_entity().living_entity.is_dead_or_dying()
    }

    // mob: player rider steers, client moves the vehicle (LivingEntity.tickRidden)
    fn tick_ridden(&self, _controller: &Arc<dyn EntityBase>) {}

    // mob: player starts/stops vehicle jump (PlayerRideableJumping.handleStartJump)
    fn on_player_start_jump(&self, _jump_scale: i32) {}
    fn on_player_stop_jump(&self) {}

    // mob: player opens custom vehicle inventory (HasCustomInventoryScreen)
    fn open_custom_inventory(&self, _player: &Arc<Player>) {}

    // mob: horse temper and taming (AbstractHorse)
    fn get_temper(&self) -> i32 {
        0
    }
    fn max_temper(&self) -> i32 {
        100
    }
    fn modify_temper(&self, _delta: i32) {}
    fn tame_with_name(&self, _player: &Player) -> bool {
        false
    }
    fn make_mad(&self) {}

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        if self.get_mob_entity().is_no_ai() {
            return None;
        }
        let first = self
            .get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .cloned();
        first.filter(|passenger| passenger.get_mob().is_some() && passenger.can_control_vehicle())
    }

    fn omnidirectional_air_mover(&self) -> bool {
        false
    }

    fn mob_bedrock_identifier(&self) -> Option<&'static str> {
        None
    }

    /// Unified mob metadata abstraction. When implemented, both Java and Bedrock
    /// spawn metadata are generated automatically.
    fn mob_metadata(&self) -> Option<MobMetadata> {
        let is_saddled = self.is_saddled();
        let is_no_ai = self.get_mob_entity().is_no_ai();
        if !is_saddled && !is_no_ai {
            return None;
        }
        let mut meta = MobMetadata::new();
        if is_saddled {
            meta = meta.saddled(true);
        }
        if is_no_ai {
            meta = meta.no_ai(true);
        }
        Some(meta)
    }

    /// Metadata which must accompany this mob whenever it is spawned for a Java client.
    fn mob_java_spawn_metadata(&self, version: JavaMinecraftVersion) -> Option<Box<[u8]>> {
        self.mob_metadata()
            .and_then(|m| m.to_java_spawn_metadata(version))
    }

    /// Metadata which must accompany this mob whenever it is spawned for a Bedrock client.
    fn mob_bedrock_spawn_metadata(
        &self,
    ) -> Option<pumpkin_protocol::bedrock::client::set_actor_data::SyncedActorDataList> {
        self.mob_metadata().map(|m| m.to_bedrock_actor_data())
    }

    fn get_job_site(&self) -> Option<BlockPos> {
        None
    }

    fn get_trading_player(&self) -> Option<Arc<Player>> {
        None
    }

    fn clear_trading_player(&self) {}

    fn get_home(&self) -> Option<BlockPos> {
        None
    }

    fn get_path_aware_entity(&self) -> Option<&dyn PathAwareEntity> {
        None
    }

    fn get_item_steerable(&self) -> Option<&dyn crate::entity::item_steerable::ItemSteerable> {
        None
    }

    fn is_saddled(&self) -> bool {
        false
    }

    fn can_be_saddled(&self) -> bool {
        false
    }

    fn set_saddled(&self, _saddled: bool) {}

    fn check_spawn_obstruction(&self, world: &World) -> bool {
        let bounding_box = self.get_entity().bounding_box.load();
        !world.contains_any_liquid(bounding_box)
            && world.get_entities_at_box(&bounding_box).is_empty()
    }

    /// Per-mob tick hook called each tick before AI runs. Override for mob-specific logic.
    fn mob_tick(&self, _caller: &dyn EntityBase) {}

    // freeze: LivingEntity.canFreeze override (Sulfur Cube)
    fn mob_can_freeze(&self) -> bool {
        true
    }

    // spear: Entity.isInvulnerableToPiercingWeapon (Sulfur Cube)
    fn mob_invulnerable_to_piercing_weapon(&self) -> bool {
        self.get_entity().invulnerable.load(Relaxed)
    }

    // projectile: Entity.canBePickedFromInside (Sulfur Cube)
    fn mob_can_be_picked_from_inside(&self) -> bool {
        true
    }

    // projectile deflection: Entity.deflection (Breeze)
    fn projectile_deflection(
        &self,
        _projectile: &dyn EntityBase,
    ) -> crate::entity::projectile_deflection::ProjectileDeflectionType {
        crate::entity::projectile_deflection::ProjectileDeflectionType::None
    }

    fn mob_is_pushed_by_fluids(&self) -> bool {
        true
    }

    fn mob_can_be_seen_as_enemy(&self) -> bool {
        true
    }

    /// See [`EntityBase::sound_source`].
    fn mob_sound_source(&self) -> SoundCategory {
        self.get_entity().entity_type.sound_source
    }

    /// See [`EntityBase::is_flapping`].
    fn mob_is_flapping(&self) -> bool {
        false
    }

    /// See [`EntityBase::on_flap`].
    fn mob_on_flap(&self) {}

    /// See [`EntityBase::is_affected_by_blocks`].
    fn mob_is_affected_by_blocks(&self) -> bool {
        self.get_entity().is_affected_by_blocks()
    }

    /// See [`EntityBase::play_step_sound`].
    fn mob_play_step_sound(&self, _pos: &BlockPos, state: &BlockState) {
        self.play_block_step_sound(state);
    }

    /// See [`EntityBase::get_swim_sound`].
    fn mob_get_swim_sound(&self) -> Option<Sound> {
        self.get_entity().entity_type.swim_sound
    }

    /// See [`EntityBase::play_swim_sound`].
    fn mob_play_swim_sound(&self, volume: f32) {
        self.play_default_swim_sound(volume);
    }

    fn before_mob_ai_step(&self) {}

    // mob: Entity.canBeCollidedWith (happy ghast)
    fn mob_is_collidable(&self, _other: Option<&dyn EntityBase>) -> bool {
        true
    }

    // mob: Leashable.onElasticLeashPull
    fn on_elastic_leash_pull(&self) {}

    fn mob_ai_step(&self) {}

    // mob: own jump control, true = handled (Rabbit.RabbitJumpControl.tick)
    fn jump_control_tick(&self, _jump: bool) -> bool {
        false
    }

    // mob: getJumpPower multiplier (LivingEntity.getJumpPower(float))
    fn jump_power_multiplier(&self) -> f64 {
        1.0
    }

    // mob: after jumpFromGround (Rabbit.jumpFromGround)
    fn on_jump_from_ground(&self) {}

    fn travel_in_water(&self, caller: &dyn EntityBase, gravity: f64, falling: bool) {
        self.get_mob_entity()
            .living_entity
            .travel_in_water(caller, gravity, falling);
    }

    fn post_tick(&self) {}

    fn base_tick(&self) {}

    fn travel(&self, _caller: &dyn EntityBase) -> bool {
        false
    }

    /// Vanilla `LivingEntity.handleKillingBlow`: `false` keeps the mob alive after lethal damage.
    fn handle_killing_blow(&self) -> bool {
        true
    }

    /// Vanilla `die(source)` override: runs once when the mob dies, `killer` is the causing entity.
    fn mob_on_death(&self, _killer: Option<&dyn EntityBase>) {}

    /// Vanilla `dropCustomDeathLoot`: extra drops after the loot table.
    fn drop_custom_death_loot(&self) {}

    /// Vanilla `LivingEntity.tickDeath` override: `true` replaces the generic 20 tick death animation.
    fn custom_tick_death(&self) -> bool {
        false
    }

    fn wants_to_pick_up(&self, _stack: &ItemStack) -> bool {
        false
    }

    // mob: wantsToPickUp for sensors that already hold the brain
    fn wants_to_pick_up_in_brain(
        &self,
        stack: &ItemStack,
        _brain: &crate::entity::ai::brain::BrainState,
    ) -> bool {
        self.wants_to_pick_up(stack)
    }

    /// Generic `Mob.aiStep` looting; mobs with their own pickup (allay) opt out.
    fn uses_mob_looting(&self) -> bool {
        true
    }

    /// Vanilla `Mob.wantsToPickUp` for the generic looting (default `canHoldItem`).
    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        self.can_hold_item(stack)
    }

    /// Species inventory pickup before generic equipment looting.
    fn mob_pick_up_item(
        &self,
        _item: &crate::entity::item::ItemEntity,
        _stack: &ItemStack,
    ) -> bool {
        false
    }

    /// Vanilla `Mob.canHoldItem`.
    fn can_hold_item(&self, _stack: &ItemStack) -> bool {
        true
    }

    /// Vanilla `Mob.getMaxFallDistance`: 3, or more with a target and spare health.
    fn max_fall_distance(&self) -> i32 {
        let mob_entity = self.get_mob_entity();
        if mob_entity.get_target().is_none() {
            return 3;
        }
        let living = &mob_entity.living_entity;
        let difficulty = living.entity.world.load().level_info.load().difficulty as i32;
        let sacrifice =
            (living.health.load() - living.get_max_health() * 0.33) as i32 - (3 - difficulty) * 4;
        (sacrifice.max(0) as f32 + 3.0).floor() as i32
    }

    /// Bow attack interval (hard, other difficulties), vanilla `getHardAttackInterval`/`getAttackInterval`.
    fn bow_attack_intervals(&self) -> (i32, i32) {
        (20, 40)
    }

    /// Vanilla `canReplaceCurrentItem` veto, e.g. the drowned nautilus shell.
    fn keeps_equipped_item(&self, _current: &ItemStack) -> bool {
        false
    }

    /// End of vanilla `travelInFluid`, e.g. the buoyancy of a sulfur cube.
    fn after_travel_in_fluid(&self, _water: bool) {}

    /// Vanilla `Mob.getPreferredWeaponType`.
    fn preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        None
    }

    fn can_pick_up_loot(&self, _brain: Option<&crate::entity::ai::brain::BrainState>) -> bool {
        self.get_mob_entity().can_pick_up_loot()
    }

    fn for_each_game_event_listener(
        &self,
        _visit: &mut dyn FnMut(&dyn crate::world::game_event::GameEventListener),
    ) {
    }

    fn has_global_tick_reach(&self) -> bool {
        false
    }

    fn mob_considers_entity_as_ally(
        &self,
        _other: &dyn EntityBase,
        _brain: Option<&crate::entity::ai::brain::BrainState>,
    ) -> Option<bool> {
        None
    }

    fn drop_inventory(&self) {}

    fn can_dispenser_equip_into_slot(&self, _slot: &EquipmentSlot) -> bool {
        true
    }

    fn should_check_fall_damage(&self) -> bool {
        true
    }

    fn on_removed(&self) {}

    fn get_preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        None
    }

    fn can_replace_current_item(
        &self,
        new_item: &ItemStack,
        current_item: &ItemStack,
        slot: &EquipmentSlot,
    ) -> bool {
        equipment::can_replace_current_item(
            self.get_mob_entity(),
            self.get_preferred_weapon_type(),
            new_item,
            current_item,
            slot,
        )
    }

    /// Called before damage is applied. Return `false` to cancel the damage entirely.
    /// Used by endermen to dodge projectiles via teleportation.
    fn pre_damage(&self, _damage_type: DamageType, _source: Option<&dyn EntityBase>) -> bool {
        true
    }

    fn pre_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.pre_damage(damage_type, source)
    }

    /// Vanilla `hurtServer` override that ends the hit early; `Some(result)` skips the damage.
    fn damage_override(
        &self,
        _amount: f32,
        _damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        _source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> Option<bool> {
        None
    }

    /// Vanilla `LivingEntity.knockback` override for a hit; `true` replaces the default push.
    fn knockback_override(
        &self,
        _power: f64,
        _dx: f64,
        _dz: f64,
        _damage: f32,
        _attacker: Option<&dyn EntityBase>,
    ) -> bool {
        false
    }

    /// Vanilla `hurtArmor`: equipment durability loss from a hit that armor applies to.
    fn hurt_armor(&self, _damage_type: DamageType, _damage: f32) {}

    /// Replaces vanilla `actuallyHurt` when it returns `true` (health is then left untouched).
    /// `raw_damage` is the damage before armor, after the hurt cooldown.
    fn actually_hurt_override(&self, _damage_type: DamageType, _raw_damage: f32) -> bool {
        false
    }

    fn on_damage(&self, _damage_type: DamageType, _source: Option<&dyn EntityBase>) {}

    fn on_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.on_damage(damage_type, source);
        true
    }

    fn on_attack(&self, _target: &dyn EntityBase) {}

    // mob: melee damage roll (IronGolem.doHurtTarget)
    fn mob_attack_damage(&self, base: f32) -> f32 {
        base
    }

    // mob: before pushing a touching entity (Mob.doPush)
    fn on_push_entity(&self, _other: &Arc<dyn EntityBase>) {}

    /// Vanilla `BABY_DIMENSIONS` of a mob with its own baby hitbox; `None` halves the adult one.
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        None
    }

    /// Vanilla `onClimbable` override, e.g. a spider on a wall; `None` checks the block.
    fn climbable_override(&self) -> Option<bool> {
        None
    }

    /// End of vanilla `causeFallDamage` (creeper fuse).
    fn on_cause_fall_damage(&self, _fall_distance: f32) {}

    /// Vanilla `killedEntity`: this mob killed `victim`; `false` cancels the victim's drops.
    fn killed_entity(&self, _victim: &dyn EntityBase) -> bool {
        true
    }

    /// Start of vanilla `doHurtTarget`, run on every melee attempt even when it misses or is blocked.
    fn pre_attack(&self, _target: &dyn EntityBase) {}

    // melee attack: species damage roll (HoglinBase.hurtAndThrowTarget)
    fn outgoing_attack_damage(&self, base_damage: f32) -> f32 {
        base_damage
    }

    // melee attack: species preparation (PolarBearMeleeAttackGoal)
    fn melee_attack_cooldown_tick(
        &self,
        _target: &dyn EntityBase,
        cooldown: i32,
        _in_attack_range: bool,
        _max_cooldown: i32,
    ) -> i32 {
        cooldown
    }

    // melee attack: species cleanup (PolarBearMeleeAttackGoal.stop)
    fn melee_attack_stopped(&self) {}

    /// Vanilla `blockedByItem`: this mob's attack hit a raised shield of `defender`.
    fn blocked_by_item(&self, _defender: &dyn EntityBase) {}

    // getArrow in vanilla, lets skeletons change the arrow before it's shot
    fn get_arrow(&self, arrow: ArrowEntity) -> ArrowEntity {
        arrow
    }

    fn on_eating_grass(&self) {}

    fn modify_incoming_damage(&self, amount: f32, _damage_type: DamageType) -> f32 {
        amount
    }

    fn can_attack_with_owner(&self, _target: &dyn EntityBase, _owner: &dyn EntityBase) -> bool {
        true
    }

    fn get_mob_gravity(&self) -> f64 {
        self.get_mob_entity().living_entity.get_gravity()
    }

    fn get_mob_y_velocity_drag(&self) -> Option<f64> {
        None
    }

    fn as_ageable(&self) -> Option<&dyn crate::entity::ageable::AgeableMob> {
        None
    }

    /// turns the mob into a baby. False when it has no baby form.
    fn spawn_as_baby(&self) -> bool {
        self.as_ageable().is_some_and(|ageable| {
            ageable.set_baby(true);
            true
        })
    }

    // mob age: non-AgeableMob baby state (AbstractPiglin.isBaby)
    fn is_baby_mob(&self) -> bool {
        self.as_ageable()
            .is_some_and(crate::entity::ageable::AgeableMob::is_baby)
    }

    // brain state: pacified by a repellent (HoglinAi.isPacified)
    fn is_pacified(&self) -> bool {
        false
    }

    // animal breeding: species gate (Hoglin.canFallInLove)
    fn can_fall_in_love(&self) -> bool {
        true
    }

    // piglin hunt: target eligibility (Hoglin.canBeHunted)
    fn can_be_hunted(&self) -> bool {
        true
    }

    // brain target: expiring combat memory (HoglinAi.setAttackTarget)
    fn set_temporary_attack_target(
        &self,
        target: Arc<dyn EntityBase>,
        _ticks: i32,
        reason: TargetReason,
    ) {
        self.set_mob_target(Some(target), reason);
    }

    // brain avoid: expiring flee memory (HoglinAi.setAvoidTarget)
    fn begin_avoiding_entity(&self, _target: Arc<dyn EntityBase>) {}

    fn as_custom_sound(&self) -> Option<&dyn crate::entity::custom_sound::CustomSound> {
        None
    }

    fn get_ambient_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        None
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        MobEntity::DEFAULT_AMBIENT_SOUND_INTERVAL
    }

    fn play_ambient_sound(&self) {
        if let Some(sound) = self.get_ambient_sound() {
            self.make_sound(sound);
        }
    }

    fn make_sound(&self, sound: pumpkin_data::sound::Sound) {
        let custom_sound = self.as_custom_sound();
        self.get_mob_entity().living_entity.make_sound(
            sound,
            custom_sound.map_or(
                self.get_entity().entity_type.sound_source,
                crate::entity::custom_sound::CustomSound::sound_category,
            ),
            custom_sound.map_or(1.0, crate::entity::custom_sound::CustomSound::sound_volume),
        );
    }

    fn reset_ambient_sound_time(&self) {
        self.get_mob_entity()
            .ambient_sound_time
            .store(-self.get_ambient_sound_interval(), Relaxed);
    }

    fn tick_ambient_sound(&self) {
        let mob = self.get_mob_entity();
        if mob.living_entity.is_alive()
            && self.get_random().random_range(0..1000)
                < mob.ambient_sound_time.fetch_add(1, Relaxed)
        {
            self.reset_ambient_sound_time();
            self.play_ambient_sound();
        }
    }

    fn as_schooling_fish(
        &self,
    ) -> Option<&dyn crate::entity::passive::abstract_schooling_fish::AbstractSchoolingFish> {
        None
    }

    fn as_bucketable(&self) -> Option<&dyn crate::entity::bucketable::Bucketable> {
        None
    }

    fn as_fish(&self) -> Option<&dyn crate::entity::passive::abstract_fish::AbstractFish> {
        None
    }

    fn as_animal(&self) -> Option<&dyn crate::entity::passive::animal::Animal> {
        None
    }

    // mob: mount speed factor for spear charges (Mob.chargeSpeedModifier)
    fn charge_speed_modifier(&self) -> f32 {
        1.0
    }

    /// How much this mob likes standing on `pos`, used to rank stroll candidates.
    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        if let Some(animal) = self.as_animal() {
            return animal.animal_walk_target_value(pos);
        }
        // monster: prefer dark spots (Monster.getWalkTargetValue)
        if crate::entity::ai::target_match::TargetMatch::Monster
            .matches(self.get_entity().entity_type)
        {
            return 0.5
                - self
                    .get_entity()
                    .world
                    .load()
                    .get_light_level_dependent_magic_value(pos);
        }
        0.0
    }

    fn as_tamable(&self) -> Option<&dyn crate::entity::passive::tamable::TamableAnimal> {
        None
    }

    fn as_patrolling_monster(&self) -> Option<&dyn patrol::PatrollingMonster> {
        None
    }

    fn as_raider(&self) -> Option<&dyn raider::Raider> {
        None
    }

    /// Must return `Some(self)` for every `NeutralMob` implementor. Not compiler-enforced.
    fn as_neutral(&self) -> Option<&dyn neutral::NeutralMob> {
        None
    }

    /// How this mob treats players by default. Derived, no per-mob data.
    fn temperament(&self) -> Temperament {
        Temperament::classify(
            self.as_neutral().is_some(),
            self.get_entity().entity_type.category,
        )
    }

    fn as_iron_golem(&self) -> Option<&crate::entity::passive::iron_golem::IronGolemEntity> {
        None
    }

    fn as_crossbow_attack_mob(&self) -> Option<&dyn crossbow_attack_mob::CrossbowAttackMob> {
        None
    }

    /// Vanilla `Mob.finalizeSpawn`, run before the mob enters the world on spawns that
    /// vanilla finalizes. Overrides call `finalize_spawn_base` first. Riders queued with
    /// `add_pending_rider` are added by the world together with the mob.
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<spawn::SpawnGroupData>,
    ) -> Option<spawn::SpawnGroupData> {
        self.finalize_spawn_super(group_data)
    }

    fn finalize_spawn_super(
        &self,
        group_data: Option<spawn::SpawnGroupData>,
    ) -> Option<spawn::SpawnGroupData> {
        if let Some(ageable) = self.as_ageable() {
            return ageable.finalize_ageable_spawn(group_data);
        }
        self.get_mob_entity().finalize_spawn_base();
        group_data
    }

    fn is_max_group_size_reached(&self, _group_size: i32) -> bool {
        false
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        difficulty: &crate::entity::mob::equipment::RegionalDifficulty,
    ) {
        if rand::random::<f32>()
            < MobEntity::MAX_WEARING_ARMOR_CHANCE * difficulty.special_multiplier
        {
            let mut armor_type = rand::random_range(0..3);
            for _ in 1..=3 {
                if rand::random::<f32>() < MobEntity::WEARING_ARMOR_UPGRADE_MATERIAL_CHANCE {
                    armor_type += 1;
                }
            }

            let partial_chance = if difficulty.base_difficulty == Difficulty::Hard {
                0.1f32
            } else {
                0.25f32
            };

            let living = &self.get_mob_entity().living_entity;
            let mut equipment = living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut first = true;

            for slot in &MobEntity::EQUIPMENT_POPULATION_ORDER {
                let current = equipment.get(slot);
                if !first && rand::random::<f32>() < partial_chance {
                    break;
                }
                first = false;
                if current.is_empty()
                    && let Some(item) = MobEntity::get_equipment_for_slot(slot, armor_type)
                {
                    equipment.put(slot, ItemStack::new(1, item));
                }
            }
        }
    }

    fn populate_default_equipment_enchantments(
        &self,
        difficulty: &crate::entity::mob::equipment::RegionalDifficulty,
    ) {
        self.enchant_spawned_weapon(difficulty);
        for slot in &MobEntity::EQUIPMENT_POPULATION_ORDER {
            self.enchant_spawned_armor(slot, difficulty);
        }
    }

    fn enchant_spawned_weapon(
        &self,
        difficulty: &crate::entity::mob::equipment::RegionalDifficulty,
    ) {
        self.enchant_spawned_equipment(
            &EquipmentSlot::MAIN_HAND,
            MobEntity::MAX_ENCHANTED_WEAPON_CHANCE,
            difficulty,
        );
    }

    fn enchant_spawned_armor(
        &self,
        slot: &EquipmentSlot,
        difficulty: &crate::entity::mob::equipment::RegionalDifficulty,
    ) {
        self.enchant_spawned_equipment(slot, MobEntity::MAX_ENCHANTED_ARMOR_CHANCE, difficulty);
    }

    fn enchant_spawned_equipment(
        &self,
        slot: &EquipmentSlot,
        chance: f32,
        difficulty: &crate::entity::mob::equipment::RegionalDifficulty,
    ) {
        let living = &self.get_mob_entity().living_entity;
        let mut equipment = living
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stack) = equipment.equipment.get_mut(slot)
            && !stack.is_empty()
            && rand::random::<f32>() < chance * difficulty.special_multiplier
        {
            crate::entity::mob::equipment::apply_vanilla_enchantments(
                stack,
                slot,
                difficulty.special_multiplier,
            );
        }
    }

    /// Runs after navigation and before the movement controls, where vanilla ticks a mob's brain.
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {}

    /// Builds this mob's brain from its saved memories; goal mobs keep the brain-dead default.
    fn make_brain(&self, _packed: &PackedMemories) -> Brain {
        Brain::default()
    }

    fn mob_write_nbt(&self, _nbt: &mut NbtCompound) {}

    fn mob_read_nbt(&self, _nbt: &NbtCompound) {}

    /// Drops a target the mob is not allowed to attack, such as a creative player.
    fn as_valid_target(&self, target: Option<Arc<dyn EntityBase>>) -> Option<Arc<dyn EntityBase>> {
        let target = target?;
        if !EntityPredicate::ExceptCreativeOrSpectator.test(target.get_entity()) {
            return None;
        }
        self.can_attack(target.as_ref()).then_some(target)
    }

    /// Set or clear the mob's target through `EntityTargetEvent`.
    /// Returns `false` when a plugin cancelled the change.
    fn set_mob_target(&self, target: Option<Arc<dyn EntityBase>>, reason: TargetReason) -> bool {
        let mob = self.get_mob_entity();
        let mut target = self.as_valid_target(target);
        let target_id = target.as_ref().map(|t| t.get_entity().entity_id);
        // Unchanged: refresh the handle, no event.
        if mob.get_target().map(|t| t.get_entity().entity_id) == target_id {
            self.apply_mob_target(target);
            return true;
        }
        let world = mob.living_entity.entity.world.load_full();
        if let Some(server) = world.server.upgrade() {
            let mut event =
                EntityTargetEvent::new(mob.living_entity.entity.entity_id, target_id, reason);
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return false;
            }
            if event.target_id != target_id {
                target = event
                    .target_id
                    .and_then(|id| world.get_entity_by_id(id))
                    .filter(|entity| entity.get_living_entity().is_some());
            }
        }
        self.apply_mob_target(target);
        true
    }

    /// Stores an approved target. Override for side effects of a target change.
    fn apply_mob_target(&self, target: Option<Arc<dyn EntityBase>>) {
        *self
            .get_mob_entity()
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.get_mob_entity()
            .mob_interact(player, item_stack, self.can_be_leashed())
    }

    fn mob_interact_with_hand(
        &self,
        _player: &Arc<Player>,
        _item_stack: &mut ItemStack,
        _hand: pumpkin_util::Hand,
    ) -> Option<bool> {
        None
    }

    /// Fires `EntityTameEvent` for a successful taming roll. `false`: a plugin refused.
    fn approve_taming(&self, player: &Arc<Player>) -> bool {
        let mob = self.get_mob_entity();
        let Some(server) = mob.living_entity.entity.world.load().server.upgrade() else {
            return true;
        };
        let mut event = crate::plugin::api::events::entity::entity_tame::EntityTameEvent::new(
            mob.living_entity.entity.entity_id,
            player.clone(),
        );
        server.plugin_manager.fire_blocking(&server, &mut event);
        !event.cancelled
    }

    /// Called on a newborn for each online player who fed its parents. Foxes trust them.
    fn on_bred_by(&self, _player: Uuid) {}

    /// Fires `EntityBreedEvent` before the child spawns. `None`: a plugin cancelled,
    /// else the experience to drop.
    fn approve_breeding(&self, father_id: i32, child_id: i32, experience: i32) -> Option<i32> {
        let mob = self.get_mob_entity();
        let Some(server) = mob.living_entity.entity.world.load().server.upgrade() else {
            return Some(experience);
        };
        let mut event = crate::plugin::api::events::entity::entity_breed::EntityBreedEvent::new(
            father_id,
            mob.living_entity.entity.entity_id,
            child_id,
            experience,
        );
        server.plugin_manager.fire_blocking(&server, &mut event);
        (!event.cancelled).then_some(event.experience)
    }

    fn dye(
        &self,
        color: crate::plugin::api::events::entity::entity_dye::DyeColor,
        player: Option<&Arc<Player>>,
    ) {
        let mob = self.get_mob_entity();
        let mut event = crate::plugin::api::events::entity::entity_dye::EntityDyeEvent::new(
            mob.living_entity.entity.entity_id,
            color,
            player.cloned(),
        );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    /// Fires `EntityEnterLoveModeEvent`. `None`: a plugin cancelled, else the love ticks.
    fn approve_love(&self, human_entity_id: Option<i32>, ticks_in_love: i32) -> Option<i32> {
        let mob = self.get_mob_entity();
        let Some(server) = mob.living_entity.entity.world.load().server.upgrade() else {
            return Some(ticks_in_love);
        };
        let mut event = crate::plugin::api::events::entity::entity_enter_love_mode::EntityEnterLoveModeEvent::new(
            mob.living_entity.entity.entity_id,
            human_entity_id,
            ticks_in_love,
        );
        server.plugin_manager.fire_blocking(&server, &mut event);
        (!event.cancelled).then_some(event.ticks_in_love)
    }

    fn transform(&self, new_entity_id: i32, transform_reason: String) {
        let mob = self.get_mob_entity();
        let mut event =
            crate::plugin::api::events::entity::entity_transform::EntityTransformEvent::new(
                mob.living_entity.entity.entity_id,
                new_entity_id,
                transform_reason,
            );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    fn break_door(&self, block_pos: BlockPos) {
        let mob = self.get_mob_entity();
        let mut event =
            crate::plugin::api::events::entity::entity_break_door::EntityBreakDoorEvent::new(
                mob.living_entity.entity.entity_id,
                block_pos,
            );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    fn enter_block(&self, block_pos: BlockPos) {
        let mob = self.get_mob_entity();
        let mut event =
            crate::plugin::api::events::entity::entity_enter_block::EntityEnterBlockEvent::new(
                mob.living_entity.entity.entity_id,
                block_pos,
            );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    fn interact(&self, block_pos: BlockPos) {
        let mob = self.get_mob_entity();
        let mut event =
            crate::plugin::api::events::entity::entity_interact::EntityInteractEvent::new(
                mob.living_entity.entity.entity_id,
                block_pos,
            );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    fn place_block(&self, block_pos: BlockPos, block_name: String) {
        let mob = self.get_mob_entity();
        let mut event = crate::plugin::api::events::entity::entity_place::EntityPlaceEvent::new(
            mob.living_entity.entity.entity_id,
            block_pos,
            block_name,
        );
        if let Some(server) = mob.living_entity.entity.world.load().server.upgrade() {
            server.plugin_manager.fire_blocking(&server, &mut event);
        }
    }

    fn mob_player_collision(&self, _player: &Arc<Player>) {}

    fn mob_entity_collision(&self, _entity: &dyn EntityBase) {}

    fn get_owner_uuid(&self) -> Option<Uuid> {
        self.as_tamable()
            .and_then(crate::entity::passive::tamable::TamableAnimal::get_owner)
    }

    /// The sitting pose, see [`Self::ordered_to_sit`] for the command.
    fn is_sitting(&self) -> bool {
        self.as_tamable()
            .is_some_and(crate::entity::passive::tamable::TamableAnimal::is_in_sitting_pose)
    }

    fn ordered_to_sit(&self) -> bool {
        self.as_tamable()
            .is_some_and(crate::entity::passive::tamable::TamableAnimal::is_ordered_to_sit)
    }

    fn is_tamed(&self) -> bool {
        self.as_tamable()
            .is_some_and(crate::entity::passive::tamable::TamableAnimal::is_tame)
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.get_entity().entity_type.experience_reward
    }

    fn should_drop_loot(&self) -> bool {
        let living = &self.get_mob_entity().living_entity;
        if self.get_entity().entity_type.category == &pumpkin_data::entity::MobCategory::MONSTER {
            living
                .entity
                .world
                .load()
                .level_info
                .load()
                .game_rules
                .mob_drops
        } else {
            living.should_drop_loot()
        }
    }

    fn should_drop_experience(&self) -> bool {
        self.get_entity().entity_type.category == &pumpkin_data::entity::MobCategory::MONSTER
            || self.get_mob_entity().living_entity.should_drop_experience()
    }

    fn mob_get_component(
        &self,
        _component: pumpkin_data::data_component::DataComponent,
    ) -> Option<Box<dyn pumpkin_data::data_component_impl::DataComponentImpl>> {
        None
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(std::sync::atomic::Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(tracked_data::ageable_mob::DATA_BABY_ID, true);
        }
    }

    fn mob_set_variant_name(&self, _name: &str) {}
    fn mob_apply_implicit_component(
        &self,
        _component: pumpkin_data::data_component::DataComponent,
        _value: &str,
    ) {
    }
    fn mob_set_sound_variant_name(&self, _name: &str) {}

    fn mob_on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        self.get_mob_entity()
            .living_entity
            .on_lightning_strike(caller, lightning);
    }
}
impl<T: Mob + Send + 'static> EntityBase for T {
    fn is_invulnerable_to_piercing_weapon(&self) -> bool {
        self.mob_invulnerable_to_piercing_weapon()
    }

    fn can_be_picked_from_inside(&self) -> bool {
        self.mob_can_be_picked_from_inside()
    }

    fn projectile_deflection(
        &self,
        projectile: &dyn EntityBase,
    ) -> crate::entity::projectile_deflection::ProjectileDeflectionType {
        Mob::projectile_deflection(self, projectile)
    }

    fn is_pushed_by_fluids(&self) -> bool {
        self.mob_is_pushed_by_fluids()
    }

    fn can_be_seen_as_enemy(&self) -> bool {
        self.mob_can_be_seen_as_enemy()
            && self.get_living_entity().is_some_and(|living| {
                !living
                    .entity
                    .invulnerable
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
            && self.can_be_seen_by_anyone()
    }

    fn sound_source(&self) -> SoundCategory {
        self.mob_sound_source()
    }

    fn play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        self.mob_play_step_sound(pos, state);
    }

    fn is_flapping(&self) -> bool {
        self.mob_is_flapping()
    }

    fn on_flap(&self) {
        self.mob_on_flap();
    }

    fn is_affected_by_blocks(&self) -> bool {
        self.mob_is_affected_by_blocks()
    }

    fn get_swim_sound(&self) -> Option<Sound> {
        self.mob_get_swim_sound()
    }

    fn play_swim_sound(&self, volume: f32) {
        self.mob_play_swim_sound(volume);
    }

    fn get_mob(&self) -> Option<&dyn Mob> {
        Some(self)
    }

    fn is_pushable(&self) -> bool {
        self.mob_is_pushable() && self.get_mob_entity().living_entity.is_pushable()
    }

    fn on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        self.mob_on_lightning_strike(caller, lightning);
    }

    fn get_item_steerable(&self) -> Option<&dyn crate::entity::item_steerable::ItemSteerable> {
        Mob::get_item_steerable(self)
    }

    fn get_component(
        &self,
        component: pumpkin_data::data_component::DataComponent,
    ) -> Option<Box<dyn pumpkin_data::data_component_impl::DataComponentImpl>> {
        self.mob_get_component(component)
    }

    fn init_data_tracker(&self) {
        self.mob_init_data_tracker();
        let world = self.get_mob_entity().living_entity.entity.world.load();
        crate::entity::mob::equipment::equip_mob_on_spawn(self as &dyn EntityBase, &world);

        let entity_name = self.get_entity().entity_type.resource_name;
        if let Some(def) = crate::entity::mob::equipment::EQUIPMENT_REGISTRY.get(entity_name)
            && def.can_pick_up_loot
        {
            let difficulty = crate::entity::mob::equipment::RegionalDifficulty::at(
                &world,
                self.get_entity().pos.load(),
            );
            let pickup_chance = 0.55 * difficulty.special_multiplier;
            self.get_mob_entity()
                .set_can_pick_up_loot(rand::random::<f32>() < pickup_chance);
        }
    }

    fn set_variant_name(&self, name: &str) {
        self.mob_set_variant_name(name);
    }

    fn apply_implicit_component(
        &self,
        component: pumpkin_data::data_component::DataComponent,
        value: &str,
    ) {
        self.mob_apply_implicit_component(component, value);
    }

    fn set_sound_variant_name(&self, name: &str) {
        self.mob_set_sound_variant_name(name);
    }

    #[allow(clippy::too_many_lines)]
    fn tick(&self, caller: &dyn EntityBase, server: &Server) {
        let mob_entity = self.get_mob_entity();
        // non-finite position would hang raycasts, skip the tick and name the mob
        let pos = mob_entity.living_entity.entity.pos.load();
        if !(pos.x.is_finite() && pos.y.is_finite() && pos.z.is_finite()) {
            tracing::error!(
                "{} has a non-finite position {pos:?}",
                mob_entity.living_entity.entity.entity_type.resource_name
            );
            return;
        }
        mob_entity.process_navigation_updates();
        if mob_entity.living_entity.entity.tick_leash() {
            self.on_elastic_leash_pull();
        }
        sun_burn::tick(self);
        looting::tick_looting(self);
        // path max fall distance follows target + health (Mob.getMaxFallDistance)
        let max_fall = self.max_fall_distance();
        if mob_entity.applied_max_fall_distance.swap(max_fall, Relaxed) != max_fall {
            mob_entity
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_max_fall_distance(max_fall as f32);
        }

        if mob_entity.breeding_cooldown.load(Relaxed) > 0 {
            mob_entity.breeding_cooldown.fetch_sub(1, Relaxed);
        }

        // animal: love countdown, hearts are client-side only (Animal.aiStep)
        if mob_entity.love_ticks.load(Relaxed) > 0 {
            mob_entity.love_ticks.fetch_sub(1, Relaxed);
        }

        mob_entity.check_despawn(self);
        if mob_entity.living_entity.entity.is_removed() {
            return;
        }

        if let Some(neutral) = self.as_neutral()
            && !neutral.updates_anger_in_ai_step()
        {
            neutral.update_persistent_anger();
        }

        self.mob_tick(caller);
        if mob_entity.living_entity.entity.is_removed() {
            return;
        }

        mob_entity.living_entity.tick(caller, server);
        self.post_tick();
        if mob_entity.living_entity.entity.tick_count.load(Relaxed)
            % MobEntity::CONTROL_FLAGS_UPDATE_INTERVAL
            == 0
        {
            mob_entity.update_control_flags(self);
        }
    }

    fn is_collidable(&self, entity: Option<Box<dyn EntityBase>>) -> bool {
        self.mob_is_collidable(entity.as_deref())
    }

    fn can_hit(&self) -> bool {
        true
    }

    fn damage_with_context(
        &self,
        caller: &dyn EntityBase,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        // pre_damage hook: allows mobs to dodge/cancel damage (e.g. enderman projectile dodge)
        if !self.pre_damage_with_context(damage_type, source, cause) {
            return false;
        }
        if let Some(result) = self.damage_override(amount, damage_type, position, source, cause) {
            return result;
        }
        // Mob-specific damage modifier (e.g. shulker armor when closed).
        let amount = self.modify_incoming_damage(amount, damage_type);
        let damaged = self.get_mob_entity().living_entity.damage_with_context(
            caller,
            amount,
            damage_type,
            position,
            source,
            cause,
        );
        if damaged {
            return self.on_damage_with_context(damage_type, source, cause);
        }
        damaged
    }

    fn interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.mob_interact(player, item_stack)
            || self
                .as_ageable()
                .is_some_and(|ageable| ageable.ageable_interact(player, item_stack))
    }

    fn interact_with_hand(
        &self,
        player: &Arc<Player>,
        item_stack: &mut ItemStack,
        hand: pumpkin_util::Hand,
    ) -> Option<bool> {
        self.mob_interact_with_hand(player, item_stack, hand)
    }

    fn on_player_collision(&self, player: &Arc<Player>) {
        self.mob_player_collision(player);
    }

    fn get_entity(&self) -> &Entity {
        &self.get_mob_entity().living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.get_mob_entity().living_entity)
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_in_love(&self) -> bool {
        self.get_mob_entity().is_in_love()
    }

    fn is_breeding_ready(&self) -> bool {
        self.get_mob_entity().is_breeding_ready()
    }

    fn reset_love(&self) {
        self.get_mob_entity().reset_love_ticks();
    }

    fn set_breeding_cooldown(&self, ticks: i32) {
        self.get_mob_entity()
            .breeding_cooldown
            .store(ticks, Relaxed);
    }

    fn is_panicking(&self) -> bool {
        self.get_path_aware_entity()
            .is_some_and(PathAwareEntity::is_panicking)
    }

    fn get_job_site_pos(&self) -> Option<pumpkin_util::math::position::BlockPos> {
        <T as Mob>::get_job_site(self)
    }

    fn get_home_pos(&self) -> Option<pumpkin_util::math::position::BlockPos> {
        <T as Mob>::get_home(self)
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        self.get_mob_entity().write_mob_nbt(self, nbt);
        if let Some(ageable) = self.as_ageable() {
            ageable.write_ageable_nbt(nbt);
        }
        if let Some(animal) = self.as_animal() {
            animal.write_animal_nbt(nbt);
        }
        if let Some(tamable) = self.as_tamable() {
            tamable.write_tamable_nbt(nbt);
        }
        if let Some(neutral) = self.as_neutral() {
            neutral.write_anger_nbt(nbt);
        }
        self.mob_write_nbt(nbt);
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        self.get_mob_entity().read_mob_nbt(nbt);
        if let Some(brain) = nbt.get_compound("Brain") {
            let rebuilt = self.make_brain(&PackedMemories::from_nbt(brain));
            self.get_mob_entity().set_brain(rebuilt);
        }
        if let Some(ageable) = self.as_ageable() {
            ageable.read_ageable_nbt(nbt);
        }
        if let Some(animal) = self.as_animal() {
            animal.read_animal_nbt(nbt);
        }
        if let Some(tamable) = self.as_tamable() {
            tamable.read_tamable_nbt(nbt);
        }
        if let Some(neutral) = self.as_neutral() {
            neutral.read_anger_nbt(nbt);
        }
        self.mob_read_nbt(nbt);
    }

    fn get_gravity(&self) -> f64 {
        self.get_mob_gravity()
    }

    fn get_y_velocity_drag(&self) -> Option<f64> {
        self.get_mob_y_velocity_drag()
    }

    fn is_flutterer(&self) -> bool {
        self.omnidirectional_air_mover()
    }

    fn is_immobile(&self) -> bool {
        self.is_mob_immobile()
    }

    fn get_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        self.mob_controlling_passenger()
    }

    fn get_experience_reward(&self, _killer: Option<&dyn EntityBase>) -> u32 {
        // TODO: apply enchantment processing like in vanilla
        Mob::get_base_experience_reward(self)
    }

    fn should_drop_loot(&self) -> bool {
        Mob::should_drop_loot(self)
    }

    fn should_drop_experience(&self) -> bool {
        Mob::should_drop_experience(self)
    }

    fn get_base_experience_reward(&self) -> u32 {
        Mob::get_base_experience_reward(self)
    }
}

#[expect(dead_code)]
const DEFAULT_PATHFINDING_FAVOR: f32 = 0.0;

pub trait PathAwareEntity: Mob + Send + Sync {
    fn get_pathfinding_favor(&self, _block_pos: BlockPos, _world: Arc<World>) -> f32 {
        0.0
    }

    // TODO: missing SpawnReason attribute
    fn can_spawn(&self, world: Arc<World>) -> bool {
        self.get_pathfinding_favor(
            self.get_mob_entity().living_entity.entity.block_pos.load(),
            world,
        ) >= 0.0
    }

    fn is_navigation(&self) -> bool {
        !self.get_mob_entity().steered().navigator().is_idle()
    }

    // TODO: implement
    fn is_panicking(&self) -> bool {
        false
    }

    fn should_follow_leash(&self) -> bool {
        true
    }

    fn on_short_leash_tick(&self) {
        // TODO: implement
    }

    fn before_leash_tick(&self) {
        // TODO: implement
    }

    fn get_follow_leash_speed(&self) -> f32 {
        1.0
    }
}

pub trait RangedAttackMob: Mob + Send + Sync {
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, power: f32);
}

#[cfg(test)]
mod navigation_update_tests {
    use super::*;
    use crate::entity::ai::{
        goal::Goal,
        pathfinder::{node::Node, path::Path},
        test_support::world,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn route(target: i32) -> Path {
        let mut path = Path::new(
            vec![
                Node::new(BlockPos::new(0, 64, 0)),
                Node::new(BlockPos::new(4, 64, 0)),
                Node::new(BlockPos::new(target, 64, 0)),
            ],
            BlockPos::new(target, 64, 0),
            true,
        );
        path.set_next_node_index(1);
        path
    }

    fn mob(world: Arc<World>) -> MobEntity {
        world
            .level_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .world_age = 21;
        let mob = MobEntity::new(Entity::new(
            world,
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::BAT,
        ));
        let mut navigator = mob
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *navigator = Navigator::flying();
        navigator.move_to_path(Some(route(8)), 1.0, &mob.living_entity);
        drop(navigator);
        mob
    }

    #[tokio::test]
    async fn contended_notifications_survive_and_drain_for_no_ai_mobs() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let mob = mob(world.clone());
        mob.set_no_ai(true);
        let before = {
            let navigator = mob
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let sequence = world
                .navigation_change_sequence
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            mob.on_navigation_block_changed(BlockPos::new(4, 64, 0), world.uuid, sequence);
            navigator.get_path().map(Path::get_next_node_index)
        };
        assert_eq!(before, Some(1));
        mob.process_navigation_updates();
        let index = mob
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_path()
            .map(Path::get_next_node_index);
        world.level.shutdown().await;
        assert_eq!(index, Some(0));
        assert!(mob.is_no_ai());
        Ok(())
    }

    struct Probe(MobEntity);

    impl Mob for Probe {
        fn get_mob_entity(&self) -> &MobEntity {
            &self.0
        }
    }

    struct ChangeRouteBlock;

    impl Goal for ChangeRouteBlock {
        fn can_start(&mut self, _mob: &dyn Mob) -> bool {
            true
        }

        fn tick(&mut self, mob: &dyn Mob) {
            let mob = mob.get_mob_entity();
            let _navigator = mob
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let world = mob.living_entity.entity.world.load();
            let sequence = world
                .navigation_change_sequence
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            mob.on_navigation_block_changed(BlockPos::new(4, 64, 0), world.uuid, sequence);
        }
    }

    struct ReadRoute(Arc<std::sync::Mutex<Option<usize>>>);

    impl Goal for ReadRoute {
        fn can_start(&mut self, _mob: &dyn Mob) -> bool {
            true
        }

        fn tick(&mut self, mob: &dyn Mob) {
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = mob
                .get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_path()
                .map(Path::get_next_node_index);
        }
    }

    #[tokio::test]
    async fn later_goals_see_routes_recomputed_by_an_earlier_goal() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let mob = Probe(mob(world.clone()));
        let seen = Arc::new(std::sync::Mutex::new(None));
        mob.0.add_goal(0, ChangeRouteBlock);
        mob.0.add_goal(1, ReadRoute(seen.clone()));

        mob.0.server_ai_step(&mob, &mob);
        world.level.shutdown().await;

        assert_eq!(
            *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some(0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_routes_supersede_older_notifications_including_equal_routes() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let mob = mob(world.clone());
        for target in [8, 10] {
            let mut navigator = mob
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let sequence = world
                .navigation_change_sequence
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            mob.on_navigation_block_changed(BlockPos::new(4, 64, 0), world.uuid, sequence);
            navigator.move_to_path(Some(route(target)), 1.0, &mob.living_entity);
            drop(navigator);
            mob.process_navigation_updates();
            let navigator = mob
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(navigator.get_path().map(Path::get_next_node_index), Some(1));
            assert_eq!(
                navigator.get_path().map(Path::get_target),
                Some(BlockPos::new(target, 64, 0))
            );
        }
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn notifications_from_an_old_world_do_not_touch_a_new_world_route() -> TestResult {
        let first_folder = tempfile::tempdir()?;
        let second_folder = tempfile::tempdir()?;
        let first = world(&first_folder);
        let second = world(&second_folder);
        let mob = mob(first.clone());
        second
            .level_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .world_age = 21;
        {
            let mut navigator = mob
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            first.navigation_change_sequence.store(1, Ordering::Release);
            mob.on_navigation_block_changed(BlockPos::new(4, 64, 0), first.uuid, 1);
            mob.living_entity.entity.set_world(second.clone());
            navigator.move_to_path(Some(route(8)), 1.0, &mob.living_entity)
        };
        mob.process_navigation_updates();
        let index = mob
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_path()
            .map(Path::get_next_node_index);
        first.level.shutdown().await;
        second.level.shutdown().await;
        assert_eq!(index, Some(1));
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::entity::{
        ai::{goal::Goal, test_support::world},
        mob::slime::SlimeEntity,
        passive::cow::CowEntity,
    };

    struct ControlProbe(Controls, Arc<AtomicBool>);

    #[tokio::test]
    async fn audited_reach_covers_a_route_after_the_mob_grows_up()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::entity::ai::pathfinder::{node::Node, path::Path};

        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let cow = CowEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::COW,
        ));
        let mob = cow.get_mob_entity();
        let standing = mob
            .audited_reach(cow.as_ref())
            .ok_or("missing adult reach")?;
        let destination = BlockPos::new(8, 64, 0);
        assert!(
            mob.navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .move_to_path(
                    Some(Path::new(vec![Node::new(destination)], destination, true)),
                    1.25,
                    &mob.living_entity,
                )
        );
        cow.get_entity().age.store(-1, Relaxed);
        let baby = mob
            .audited_reach(cow.as_ref())
            .ok_or("missing baby reach")?;
        cow.get_entity().age.store(0, Relaxed);
        let grown = mob
            .audited_reach(cow.as_ref())
            .ok_or("missing grown reach")?;
        assert!(baby.motion.0 > standing.motion.0);
        assert_eq!(grown.motion, baby.motion);
        cow.get_entity().age.store(-1, Relaxed);
        mob.navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_speed(1.0);
        let slower_route = mob
            .audited_reach(cow.as_ref())
            .ok_or("missing slower route reach")?;
        assert_eq!(slower_route.motion, baby.motion);
        cow.get_entity().age.store(0, Relaxed);
        mob.navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_path_mut()
            .ok_or("missing retained path")?
            .advance();
        let done = mob
            .audited_reach(cow.as_ref())
            .ok_or("missing done reach")?;
        assert_eq!(done.motion, standing.motion);
        world.level.shutdown().await;
        Ok(())
    }

    impl Goal for ControlProbe {
        fn can_start(&mut self, _mob: &dyn Mob) -> bool {
            true
        }
        fn tick(&mut self, _mob: &dyn Mob) {
            self.1.store(true, Relaxed);
        }
        fn controls(&self) -> Controls {
            self.0
        }
    }

    fn run_controlled_goals(mob: &dyn Mob) -> [bool; 3] {
        let entity = mob.get_mob_entity();
        entity.clear_ai_goals(mob);
        let ran: [Arc<AtomicBool>; 3] = std::array::from_fn(|_| Arc::new(AtomicBool::new(false)));
        {
            let goals = &entity.goals_selector;
            for (control, observed) in [Controls::MOVE, Controls::LOOK, Controls::JUMP]
                .into_iter()
                .zip(&ran)
            {
                goals.add_goal(0, Box::new(ControlProbe(control, observed.clone())));
            }
        }
        entity.update_control_flags(mob);
        GoalSelector::tick(&entity.goals_selector, mob);
        ran.map(|ran| ran.load(Relaxed))
    }

    #[tokio::test]
    async fn control_flags_use_the_controlling_passenger_and_boat_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let cow = CowEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::COW,
        ));
        let rider = CowEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::COW,
        ));
        cow.get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(rider.clone());
        assert_eq!(run_controlled_goals(&*cow), [false, false, false]);
        let slime = SlimeEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::SLIME,
        ));
        {
            let mut passengers = cow
                .get_entity()
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            passengers.insert(0, slime);
        };
        assert_eq!(run_controlled_goals(&*cow), [true, true, true]);
        cow.get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let boat = Arc::new(BoatEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::OAK_BOAT,
        )));
        *cow.get_entity()
            .vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(boat);
        let boat_controls = run_controlled_goals(&*cow);
        let chest_boat = Arc::new(BoatEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::OAK_CHEST_BOAT,
        )));
        *cow.get_entity()
            .vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(chest_boat);
        let chest_boat_controls = run_controlled_goals(&*cow);
        world.level.shutdown().await;
        assert_eq!(boat_controls, [true, true, false]);
        assert_eq!(chest_boat_controls, [true, true, false]);
        Ok(())
    }

    #[tokio::test]
    async fn riders_steer_the_mob_they_control() -> Result<(), Box<dyn std::error::Error>> {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let cow = CowEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::COW,
        ));
        let rider = CowEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::COW,
        ));
        cow.get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(rider.clone());
        *rider
            .get_entity()
            .vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cow.clone());
        rider.get_mob_entity().steered().navigator().set_progress(
            crate::entity::ai::pathfinder::NavigatorGoal::new(
                Vector3::new(0.0, 64.0, 0.0),
                Vector3::new(8.0, 64.0, 0.0),
                1.0,
            ),
        );
        let cow_navigating = !cow
            .get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_idle();
        let rider_navigating = !rider
            .get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_idle();
        cow.get_mob_entity().set_no_ai(true);
        let steers_itself = std::ptr::eq(
            rider.get_mob_entity().steered().mob_entity(),
            rider.get_mob_entity(),
        );
        world.level.shutdown().await;
        assert!(cow_navigating);
        assert!(!rider_navigating);
        assert!(steers_itself);
        Ok(())
    }
}

// bat: Entity.isIgnoringBlockTriggers
#[must_use]
pub fn ignores_block_triggers(entity: &dyn EntityBase) -> bool {
    entity.get_mob().is_some_and(Mob::ignores_block_triggers)
}
