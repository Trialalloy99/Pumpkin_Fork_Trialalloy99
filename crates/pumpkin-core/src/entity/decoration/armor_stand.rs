use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU8, Ordering};

use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::world::game_event::GameEventContext;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::data_component_impl::{CustomNameImpl, EquippableImpl};
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{
    Block,
    damage::DamageType,
    data_component_impl::{EquipmentSlot, EquipmentType},
    entity::{EntityStatus, EntityType},
    item::Item,
    particle::Particle,
    sound::{Sound, SoundCategory},
    tag::{self, Taggable},
    tracked_data::armor_stand as tracked,
};
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::{
    Hand,
    math::{boundingbox::EntityDimensions, euler_angle::EulerAngle, vector3::Vector3},
};

// armor_stand: ArmorStand.WOBBLE_TIME and the slot disable offsets
const WOBBLE_TIME: i64 = 5;
const DISABLE_TAKING_OFFSET: i32 = 8;
const DISABLE_PUTTING_OFFSET: i32 = 16;
// armor_stand: ArmorStand.BABY_DIMENSIONS eye height, getClickedSlot offsets
const BABY_EYE_HEIGHT: f32 = 0.9875;
const FEET_OFFSET: f64 = 0.1;
const CHEST_OFFSET: f64 = 0.9;
const LEGS_OFFSET: f64 = 0.4;
const HEAD_OFFSET: f64 = 1.6;
// armor_stand: causeDamage health floor, fire damage numbers
const BREAK_HEALTH: f32 = 0.5;
const IGNITE_SECONDS: f32 = 5.0;
const ON_FIRE_DAMAGE: f32 = 0.15;
const BURN_DAMAGE: f32 = 4.0;
const MAX_PUSH_DISTANCE_SQ: f64 = 0.2;
const BREAK_PARTICLES: i32 = 10;

const ALL_SLOTS: [EquipmentSlot; 8] = [
    EquipmentSlot::MAIN_HAND,
    EquipmentSlot::OFF_HAND,
    EquipmentSlot::FEET,
    EquipmentSlot::LEGS,
    EquipmentSlot::CHEST,
    EquipmentSlot::HEAD,
    EquipmentSlot::BODY,
    EquipmentSlot::SADDLE,
];

#[derive(Debug, Clone, Copy)]
pub struct PackedRotation {
    pub head: EulerAngle,
    pub body: EulerAngle,
    pub left_arm: EulerAngle,
    pub right_arm: EulerAngle,
    pub left_leg: EulerAngle,
    pub right_leg: EulerAngle,
}

impl Default for PackedRotation {
    fn default() -> Self {
        Self {
            head: EulerAngle::new(0.0, 0.0, 0.0),
            body: EulerAngle::new(0.0, 0.0, 0.0),
            left_arm: EulerAngle::new(-10.0, 0.0, -10.0),
            right_arm: EulerAngle::new(-15.0, 0.0, 10.0),
            left_leg: EulerAngle::new(-1.0, 0.0, -1.0),
            right_leg: EulerAngle::new(1.0, 0.0, 1.0),
        }
    }
}

impl From<PackedRotation> for NbtTag {
    fn from(val: PackedRotation) -> Self {
        let mut compound = NbtCompound::new();
        compound.put("Head", val.head);
        compound.put("Body", val.body);
        compound.put("LeftArm", val.left_arm);
        compound.put("RightArm", val.right_arm);
        compound.put("LeftLeg", val.left_leg);
        compound.put("RightLeg", val.right_leg);
        Self::Compound(compound)
    }
}

impl From<NbtTag> for PackedRotation {
    #[expect(clippy::unnecessary_fallible_conversions)]
    fn from(tag: NbtTag) -> Self {
        if let NbtTag::Compound(compound) = tag {
            fn get_rotation(
                compound: &NbtCompound,
                key: &'static str,
                default: EulerAngle,
            ) -> EulerAngle {
                compound
                    .get(key)
                    .and_then(|tag| tag.clone().try_into().ok())
                    .unwrap_or(default)
            }

            let default = Self::default();

            Self {
                head: get_rotation(&compound, "Head", default.head),
                body: get_rotation(&compound, "Body", default.body),
                left_arm: get_rotation(&compound, "LeftArm", default.left_arm),
                right_arm: get_rotation(&compound, "RightArm", default.right_arm),
                left_leg: get_rotation(&compound, "LeftLeg", default.left_leg),
                right_leg: get_rotation(&compound, "RightLeg", default.right_leg),
            }
        } else {
            Self::default()
        }
    }
}

pub struct ArmorStandEntity {
    living_entity: LivingEntity,

    armor_stand_flags: AtomicU8,
    last_hit_time: AtomicI64,
    disabled_slots: AtomicI32,

    rotation: AtomicCell<PackedRotation>,
}

impl ArmorStandEntity {
    pub fn new(entity: Entity) -> Self {
        let living_entity = LivingEntity::new(entity);
        let packed_rotation = PackedRotation::default();

        Self {
            living_entity,
            armor_stand_flags: AtomicU8::new(0),
            last_hit_time: AtomicI64::new(0),
            disabled_slots: AtomicI32::new(0),
            rotation: AtomicCell::new(packed_rotation),
        }
    }

    fn flag(&self, flag: ArmorStandFlags) -> bool {
        (self.armor_stand_flags.load(Ordering::Relaxed) & flag as u8) != 0
    }

    pub fn set_small(&self, small: bool) {
        self.set_bit_field(ArmorStandFlags::Small, small);
    }

    pub fn is_small(&self) -> bool {
        self.flag(ArmorStandFlags::Small)
    }

    pub fn set_show_arms(&self, show_arms: bool) {
        self.set_bit_field(ArmorStandFlags::ShowArms, show_arms);
    }

    pub fn should_show_arms(&self) -> bool {
        self.flag(ArmorStandFlags::ShowArms)
    }

    pub fn set_hide_base_plate(&self, hide_base_plate: bool) {
        self.set_bit_field(ArmorStandFlags::HideBasePlate, hide_base_plate);
    }

    pub fn should_show_base_plate(&self) -> bool {
        !self.flag(ArmorStandFlags::HideBasePlate)
    }

    pub fn set_marker(&self, marker: bool) {
        self.set_bit_field(ArmorStandFlags::Marker, marker);
    }

    pub fn is_marker(&self) -> bool {
        self.flag(ArmorStandFlags::Marker)
    }

    // armor_stand: setBit + onSyncedDataUpdated (refreshDimensions on flag change)
    fn set_bit_field(&self, bit_field: ArmorStandFlags, set: bool) {
        let current = self.armor_stand_flags.load(Ordering::Relaxed);
        let new_value = if set {
            current | bit_field as u8
        } else {
            current & !(bit_field as u8)
        };
        self.armor_stand_flags.store(new_value, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked::DATA_CLIENT_FLAGS, new_value as i8);
        self.refresh_dimensions();
    }

    // armor_stand: getDefaultDimensions, marker, baby and normal size
    fn refresh_dimensions(&self) {
        let entity = self.get_entity();
        let normal = Entity::type_dimensions(entity.entity_type);
        entity.set_default_dimensions(if self.is_marker() {
            EntityDimensions::new(0.0, 0.0, 0.0)
        } else if self.is_small() {
            EntityDimensions::new(normal.width * 0.5, normal.height * 0.5, BABY_EYE_HEIGHT)
        } else {
            normal
        });
    }

    // armor_stand: hasPhysics
    fn has_physics(&self) -> bool {
        !self.is_marker() && !self.get_entity().has_no_gravity()
    }

    pub fn can_use_slot(&self, slot: &EquipmentSlot) -> bool {
        !matches!(slot, EquipmentSlot::Body(_) | EquipmentSlot::Saddle(_))
            && !self.is_slot_disabled(slot)
    }

    // armor_stand: isDisabled, filter bit of the slot
    pub fn is_slot_disabled(&self, slot: &EquipmentSlot) -> bool {
        let disabled_slots = self.disabled_slots.load(Ordering::Relaxed);
        let slot_bit = 1 << slot.get_slot_index();

        (disabled_slots & slot_bit) != 0
            || (slot.slot_type() == EquipmentType::Hand && !self.should_show_arms())
    }

    pub fn set_slot_disabled(&self, slot: &EquipmentSlot, disabled: bool) {
        let slot_bit = 1 << slot.get_slot_index();
        let current = self.disabled_slots.load(Ordering::Relaxed);

        let new_val = if disabled {
            current | slot_bit
        } else {
            current & !slot_bit
        };

        self.disabled_slots.store(new_val, Ordering::Relaxed);
    }

    pub fn is_invisible(&self) -> bool {
        self.get_entity().invisible.load(Ordering::Relaxed)
    }

    pub fn pack_rotation(&self) -> PackedRotation {
        self.rotation.load()
    }

    pub fn unpack_rotation(&self, packed: &PackedRotation) {
        self.rotation.store(packed.to_owned());
        self.sync_pose();
    }

    // armor_stand: DATA_*_POSE
    fn sync_pose(&self) {
        let pose = self.rotation.load();
        let entity = self.get_entity();
        entity.set_synced_data(tracked::DATA_HEAD_POSE, pose.head);
        entity.set_synced_data(tracked::DATA_BODY_POSE, pose.body);
        entity.set_synced_data(tracked::DATA_LEFT_ARM_POSE, pose.left_arm);
        entity.set_synced_data(tracked::DATA_RIGHT_ARM_POSE, pose.right_arm);
        entity.set_synced_data(tracked::DATA_LEFT_LEG_POSE, pose.left_leg);
        entity.set_synced_data(tracked::DATA_RIGHT_LEG_POSE, pose.right_leg);
    }

    fn get_item(&self, slot: &EquipmentSlot) -> ItemStack {
        self.living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot)
    }

    fn has_item_in_slot(&self, slot: &EquipmentSlot) -> bool {
        !self.get_item(slot).is_empty()
    }

    // armor_stand: LivingEntity.setItemSlot + onEquipItem (equip sound and game event)
    fn set_item_slot(&self, slot: &EquipmentSlot, stack: &ItemStack) {
        let previous = self
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(slot, stack.clone());
        self.living_entity
            .send_equipment_changes(&[(slot.clone(), stack.clone())]);

        let entity = self.get_entity();
        if (previous.is_empty() && stack.is_empty())
            || previous.are_items_and_components_equal(stack)
            || entity.tick_count.load(Ordering::Relaxed) == 0
        {
            return;
        }
        let world = entity.world.load();
        let pos = entity.pos.load();
        let equippable = stack.get_data_component::<EquippableImpl>();
        if !entity.is_silent()
            && let Some(equippable) = equippable
            && equippable.slot == slot
        {
            world.play_sound_event(&equippable.equip_sound, SoundCategory::Neutral, &pos);
        }
        let event = if equippable.is_some() {
            GameEvent::Equip
        } else {
            GameEvent::EntityAction
        };
        world.emit_game_event_with_context(
            event.name(),
            pos,
            &GameEventContext {
                source_entity: Some(self),
            },
        );
    }

    // armor_stand: LivingEntity.getEquipmentSlotForItem
    fn equipment_slot_for_item(&self, stack: &ItemStack) -> EquipmentSlot {
        stack
            .get_data_component::<EquippableImpl>()
            .map(|equippable| equippable.slot.clone())
            .filter(|slot| self.can_use_slot(slot))
            .unwrap_or(EquipmentSlot::MAIN_HAND)
    }

    // armor_stand: getClickedSlot
    fn clicked_slot(&self, location: Vector3<f64>) -> EquipmentSlot {
        let small = self.is_small();
        let age_scale = if small { 0.5 } else { 1.0 };
        let click_y = location.y / age_scale;
        if click_y >= FEET_OFFSET
            && click_y < FEET_OFFSET + if small { 0.8 } else { 0.45 }
            && self.has_item_in_slot(&EquipmentSlot::FEET)
        {
            EquipmentSlot::FEET
        } else if click_y >= CHEST_OFFSET + if small { 0.3 } else { 0.0 }
            && click_y < CHEST_OFFSET + if small { 1.0 } else { 0.7 }
            && self.has_item_in_slot(&EquipmentSlot::CHEST)
        {
            EquipmentSlot::CHEST
        } else if click_y >= LEGS_OFFSET
            && click_y < LEGS_OFFSET + if small { 1.0 } else { 0.8 }
            && self.has_item_in_slot(&EquipmentSlot::LEGS)
        {
            EquipmentSlot::LEGS
        } else if click_y >= HEAD_OFFSET && self.has_item_in_slot(&EquipmentSlot::HEAD) {
            EquipmentSlot::HEAD
        } else if !self.has_item_in_slot(&EquipmentSlot::MAIN_HAND)
            && self.has_item_in_slot(&EquipmentSlot::OFF_HAND)
        {
            EquipmentSlot::OFF_HAND
        } else {
            EquipmentSlot::MAIN_HAND
        }
    }

    // armor_stand: swapItem
    fn swap_item(
        &self,
        player: &Player,
        slot: &EquipmentSlot,
        player_stack: &mut ItemStack,
    ) -> bool {
        let current = self.get_item(slot);
        let disabled = self.disabled_slots.load(Ordering::Relaxed);
        let index = slot.get_slot_index();
        if !current.is_empty() && disabled & (1 << (index + DISABLE_TAKING_OFFSET)) != 0 {
            return false;
        }
        if current.is_empty() && disabled & (1 << (index + DISABLE_PUTTING_OFFSET)) != 0 {
            return false;
        }
        if player.is_creative() && current.is_empty() && !player_stack.is_empty() {
            self.set_item_slot(slot, &player_stack.copy_with_count(1));
            return true;
        }
        if player_stack.is_empty() || player_stack.item_count <= 1 {
            self.set_item_slot(slot, player_stack);
            *player_stack = current;
            return true;
        }
        if !current.is_empty() {
            return false;
        }
        self.set_item_slot(slot, &player_stack.split(1));
        true
    }

    // armor_stand: ArmorStand.interact(player, hand, location)
    fn interact_slots(
        &self,
        player: &Arc<Player>,
        player_stack: &mut ItemStack,
        location: Vector3<f64>,
    ) -> Option<bool> {
        if self.is_marker() || player_stack.item.id == Item::NAME_TAG.id {
            return None;
        }
        if player.is_spectator() {
            return Some(true);
        }
        let item_slot = self.equipment_slot_for_item(player_stack);
        if player_stack.is_empty() {
            let clicked = self.clicked_slot(location);
            let target = if self.is_slot_disabled(&clicked) {
                item_slot
            } else {
                clicked
            };
            if self.has_item_in_slot(&target) && self.swap_item(player, &target, player_stack) {
                return Some(true);
            }
        } else {
            if self.is_slot_disabled(&item_slot) {
                return Some(false);
            }
            if item_slot.slot_type() == EquipmentType::Hand && !self.should_show_arms() {
                return Some(false);
            }
            if self.swap_item(player, &item_slot, player_stack) {
                return Some(true);
            }
        }
        None
    }

    // armor_stand: kill, ENTITY_DIE
    fn kill_stand(&self, attributed_to: Option<&dyn EntityBase>) {
        let entity = self.get_entity();
        entity.remove();
        entity.world.load().emit_game_event_with_context(
            GameEvent::EntityDie.name(),
            entity.pos.load(),
            &GameEventContext {
                source_entity: Some(attributed_to.unwrap_or(self)),
            },
        );
    }

    // armor_stand: causeDamage
    fn cause_damage(&self, cause: Option<&dyn EntityBase>, damage: f32) {
        let health = self.living_entity.health.load() - damage;
        if health <= BREAK_HEALTH {
            self.broken_by_anything();
            self.kill_stand(cause);
        } else {
            self.living_entity.set_health(health);
            let entity = self.get_entity();
            entity.world.load().emit_game_event_with_context(
                GameEvent::EntityDamage.name(),
                entity.pos.load(),
                &GameEventContext {
                    source_entity: cause,
                },
            );
        }
    }

    // armor_stand: brokenByPlayer, the item keeps the custom name
    fn broken_by_player(&self) {
        let entity = self.get_entity();
        let mut item = ItemStack::new(1, &Item::ARMOR_STAND);
        if let Some(name) = (**entity.custom_name.load()).clone() {
            item.set_data_component(CustomNameImpl { name });
        }
        entity
            .world
            .load()
            .drop_stack(&entity.block_pos.load(), item);
        self.broken_by_anything();
    }

    // armor_stand: brokenByAnything, the equipment pops out above the stand
    fn broken_by_anything(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        world.play_sound(
            Sound::EntityArmorStandBreak,
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
        let drop_pos = entity.block_pos.load().up();
        for slot in &ALL_SLOTS {
            let item = self
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .put(slot, ItemStack::EMPTY.clone());
            if !item.is_empty() && !crate::entity::living::prevents_equipment_drop(&item) {
                world.drop_stack(&drop_pos, item);
            }
        }
    }

    // armor_stand: showBreakingParticles, oak planks
    fn show_breaking_particles(&self) {
        let entity = self.get_entity();
        let width = entity.width();
        let height = entity.height();
        let pos = entity.pos.load();
        let mut data = Vec::with_capacity(5);
        let mut state = u32::from(Block::OAK_PLANKS.default_state.id.as_u16());
        loop {
            let byte = (state & 0x7F) as u8;
            state >>= 7;
            if state == 0 {
                data.push(byte);
                break;
            }
            data.push(byte | 0x80);
        }
        entity.world.load().spawn_particles_with_data(
            Particle::Block,
            Vector3::new(
                pos.x,
                pos.y + f64::from(height) * 0.666_666_666_666_666_6,
                pos.z,
            ),
            BREAK_PARTICLES as u32,
            Vector3::new(width / 4.0, height / 4.0, width / 4.0),
            0.05,
            &data,
        );
    }
}

impl EntityBase for ArmorStandEntity {
    fn can_be_seen_by_anyone(&self) -> bool {
        !self.is_invisible() && !self.is_marker()
    }

    fn init_data_tracker(&self) {
        let entity = self.get_entity();
        entity.set_synced_data(
            tracked::DATA_CLIENT_FLAGS,
            self.armor_stand_flags.load(Ordering::Relaxed) as i8,
        );
        self.sync_pose();
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let disabled_slots = self.disabled_slots.load(Ordering::Relaxed);

        nbt.put_bool("Invisible", self.is_invisible());
        nbt.put_bool("Small", self.is_small());
        nbt.put_bool("ShowArms", self.should_show_arms());
        nbt.put_int("DisabledSlots", disabled_slots);
        nbt.put_bool("NoBasePlate", !self.should_show_base_plate());
        if self.is_marker() {
            nbt.put_bool("Marker", true);
        }

        nbt.put("Pose", self.pack_rotation());
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        self.get_entity()
            .set_invisible(nbt.get_bool("Invisible").unwrap_or(false));
        self.armor_stand_flags.store(0, Ordering::Relaxed);
        self.set_small(nbt.get_bool("Small").unwrap_or(false));
        self.set_show_arms(nbt.get_bool("ShowArms").unwrap_or(false));
        self.disabled_slots
            .store(nbt.get_int("DisabledSlots").unwrap_or(0), Ordering::Relaxed);
        self.set_hide_base_plate(nbt.get_bool("NoBasePlate").unwrap_or(false));
        self.set_marker(nbt.get_bool("Marker").unwrap_or(false));
        self.get_entity()
            .no_physics
            .store(!self.has_physics(), Ordering::Relaxed);

        if let Some(pose_tag) = nbt.get("Pose") {
            let packed: PackedRotation = pose_tag.clone().into();
            self.unpack_rotation(&packed);
        }
    }

    fn get_entity(&self) -> &Entity {
        &self.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.living_entity)
    }

    fn kill(&self, _caller: &dyn EntityBase) {
        self.kill_stand(None);
    }

    fn interact_at(
        &self,
        player: &Arc<Player>,
        item_stack: &mut ItemStack,
        _hand: Hand,
        location: Vector3<f64>,
    ) -> Option<bool> {
        self.interact_slots(player, item_stack, location)
    }

    // armor_stand: ArmorStand.hurtServer
    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        _amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let entity = self.get_entity();
        if entity.is_removed() {
            return false;
        }

        let world = entity.world.load();

        let mob_griefing_gamerule = {
            let game_rules = &world.level_info.load().game_rules;
            game_rules.mob_griefing
        };

        // Vanilla: only mob attackers are stopped by `mobGriefing`.
        if !mob_griefing_gamerule && cause.is_some_and(|cause| cause.get_mob().is_some()) {
            return false;
        }

        if damage_type.has_tag(&tag::DamageType::MINECRAFT_BYPASSES_INVULNERABILITY) {
            self.kill_stand(cause);
            return false;
        }

        if entity.is_invulnerable_to(&damage_type, cause) || self.is_invisible() || self.is_marker()
        {
            return false;
        }

        if damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_EXPLOSION) {
            self.broken_by_anything();
            self.kill_stand(cause);
            return false;
        }

        if damage_type.has_tag(&tag::DamageType::MINECRAFT_IGNITES_ARMOR_STANDS) {
            if entity.is_on_fire() {
                self.cause_damage(cause, ON_FIRE_DAMAGE);
            } else {
                EntityBase::set_on_fire_for(self, IGNITE_SECONDS);
            }
            return false;
        }
        if damage_type.has_tag(&tag::DamageType::MINECRAFT_BURNS_ARMOR_STANDS)
            && self.living_entity.health.load() > BREAK_HEALTH
        {
            self.cause_damage(cause, BURN_DAMAGE);
            return false;
        }

        let can_break = damage_type.has_tag(&tag::DamageType::MINECRAFT_CAN_BREAK_ARMOR_STAND);
        let always_kills =
            damage_type.has_tag(&tag::DamageType::MINECRAFT_ALWAYS_KILLS_ARMOR_STANDS);

        if !can_break && !always_kills {
            return false;
        }

        let attacker = cause.or(source);
        if let Some(attacker) = attacker
            && let Some(player) = attacker.get_player()
        {
            // armor_stand: skipAttackInteraction, spawn protection
            if world.is_in_spawn_protection(player, &entity.block_pos.load()) {
                return false;
            }
            if !player
                .abilities
                .try_lock()
                .is_ok_and(|a| a.allow_modify_world)
            {
                return false;
            } else if player.is_creative() {
                world.play_sound(
                    Sound::EntityArmorStandBreak,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
                self.show_breaking_particles();
                self.kill_stand(cause);
                return true;
            }
        }

        let time = world
            .level_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .query_gametime();

        if time - self.last_hit_time.load(Ordering::Relaxed) > WOBBLE_TIME && !always_kills {
            world.send_entity_status(entity, EntityStatus::ArmorstandWobble, ActorEventID::None);
            world.emit_game_event_with_context(
                GameEvent::EntityDamage.name(),
                entity.pos.load(),
                &GameEventContext {
                    source_entity: cause,
                },
            );
            self.last_hit_time.store(time, Ordering::Relaxed);
        } else {
            self.broken_by_player();
            self.show_breaking_particles();
            self.kill_stand(cause);
        }

        true
    }

    fn get_gravity(&self) -> f64 {
        if self.has_physics() { 0.08 } else { 0.0 }
    }

    // armor_stand: ArmorStand.ignoreExplosion
    fn ignore_explosion(&self, explosion: &crate::world::explosion::Explosion) -> bool {
        let mob_griefing = self
            .get_entity()
            .world
            .load()
            .level_info
            .load()
            .game_rules
            .mob_griefing;
        !explosion.should_affect_blocklike_entities(mob_griefing) || self.is_invisible()
    }

    // armor_stand: isPickable
    fn is_pickable(&self) -> bool {
        !self.is_marker()
    }

    // armor_stand: thunderHit does nothing
    fn on_lightning_strike(
        &self,
        _caller: &dyn EntityBase,
        _lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
    }

    // armor_stand: getFallSounds
    fn get_fall_sounds(&self) -> (Sound, Sound) {
        (Sound::EntityArmorStandFall, Sound::EntityArmorStandFall)
    }

    // armor_stand: pushEntities, only rideable minecarts get pushed
    fn custom_push_entities(&self, caller: &dyn EntityBase) -> bool {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        for other in world.get_entities_at_box(&entity.bounding_box.load()) {
            let other_entity = other.get_entity();
            if other_entity.entity_type == &EntityType::MINECART
                && other_entity.pos.load().squared_distance_to_vec(&pos) <= MAX_PUSH_DISTANCE_SQ
            {
                other.push(caller);
            }
        }
        true
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub enum ArmorStandFlags {
    /// Small armor stand Flag
    Small = 1,
    /// Show arms Flag
    ShowArms = 4,
    /// Hide base plate fLag
    HideBasePlate = 8,
    /// Marker Flag
    Marker = 16,
}
