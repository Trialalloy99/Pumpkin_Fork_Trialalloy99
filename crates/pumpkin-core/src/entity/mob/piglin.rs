use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::Block;
use pumpkin_data::BlockState;
use pumpkin_data::Enchantment;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::brain::{Brain, memory::PackedMemories, memory::types};
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::item::ItemEntity;
use crate::entity::mob::spawn::{SpawnGroupData, SpawnReason};
use crate::entity::mob::{MobMetadata, equipment as mob_equipment};
use crate::entity::player::Player;
use crate::entity::{
    Entity, EntityBase,
    mob::{
        Mob, MobEntity, crossbow_attack_mob::CrossbowAttackMob, equipment::RegionalDifficulty,
        piglin_ai::PiglinAi,
    },
};
use crate::world::World;

const SPEED_MODIFIER_BABY_ID: &str = "minecraft:baby";
const SPEED_MODIFIER_BABY_AMOUNT: f64 = 0.2;
const PROBABILITY_OF_SPAWNING_AS_BABY: f32 = 0.2;
const CHANCE_OF_WEARING_EACH_ARMOUR_ITEM: f32 = 0.1;
const PROBABILITY_OF_SPAWNING_WITH_CROSSBOW_INSTEAD_OF_SWORD: f32 = 0.5;
const FIRE_IN_NEIGHBOR_MALUS: f32 = 16.0;

pub struct PiglinEntity {
    pub mob_entity: MobEntity,
    pub immune_to_zombification: AtomicBool,
    pub time_in_overworld: AtomicI32,
    pub is_baby: AtomicBool,
    pub cannot_hunt: AtomicBool,
    pub is_charging_crossbow: AtomicBool,
    pub is_dancing: AtomicBool,
    pub inventory: Mutex<Vec<ItemStack>>,
}

impl PiglinEntity {
    pub const CONVERSION_TIME: i32 = 300;
    pub const INVENTORY_SIZE: usize = 8;
    pub const XP_REWARD: u32 = 5;

    pub const ADULT_DIMENSIONS: EntityDimensions = EntityDimensions {
        width: 0.6,
        height: 1.95,
        eye_height: 1.79,
    };
    pub const BABY_DIMENSIONS: EntityDimensions = EntityDimensions {
        width: 0.49,
        height: 0.98,
        eye_height: 0.78,
    };

    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        {
            // piglin: AbstractPiglin pathfinding maluses and door opening
            let navigator = mob_entity
                .navigator
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            navigator.set_pathfinding_malus(
                crate::entity::ai::pathfinder::node::PathType::DangerFire,
                FIRE_IN_NEIGHBOR_MALUS,
            );
            navigator.set_pathfinding_malus(
                crate::entity::ai::pathfinder::node::PathType::DamageFire,
                -1.0,
            );
            navigator.set_can_open_doors(true);
        }
        let piglin = Self {
            mob_entity,
            immune_to_zombification: AtomicBool::new(false),
            time_in_overworld: AtomicI32::new(0),
            is_baby: AtomicBool::new(false),
            cannot_hunt: AtomicBool::new(false),
            is_charging_crossbow: AtomicBool::new(false),
            is_dancing: AtomicBool::new(false),
            inventory: Mutex::new(Vec::new()),
        };
        let mob_arc = Arc::new(piglin);
        mob_arc.mob_entity.set_can_pick_up_loot(true);
        mob_arc
    }

    #[must_use]
    pub fn is_immune_to_zombification(&self) -> bool {
        self.immune_to_zombification.load(Ordering::Relaxed)
    }

    pub fn set_immune_to_zombification(&self, immune: bool) {
        self.immune_to_zombification
            .store(immune, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(tracked_data::piglin::DATA_IMMUNE_TO_ZOMBIFICATION, immune);
    }

    // piglin: AbstractPiglin.isConverting
    #[must_use]
    pub fn is_converting(&self, world: &World) -> bool {
        !self.is_immune_to_zombification()
            && !self.mob_entity.is_no_ai()
            && world.dimension.piglins_zombify
    }

    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.is_baby.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_adult(&self) -> bool {
        !self.is_baby()
    }

    // piglin: Piglin.setBaby, speed bonus and hitbox
    pub fn set_baby(&self, baby: bool) {
        self.mob_entity
            .set_baby_flag(&self.is_baby, tracked_data::piglin::DATA_BABY_ID, baby);
        let entity = &self.mob_entity.living_entity.entity;
        entity.set_default_dimensions(if baby {
            Self::BABY_DIMENSIONS
        } else {
            Self::ADULT_DIMENSIONS
        });
        let mut attributes = self
            .mob_entity
            .living_entity
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
            speed.remove_modifier(SPEED_MODIFIER_BABY_ID);
            if baby {
                speed.add_or_replace_modifier(Modifier {
                    id: SPEED_MODIFIER_BABY_ID.to_string(),
                    amount: SPEED_MODIFIER_BABY_AMOUNT,
                    operation: ModifierOperation::MultiplyBase,
                });
            }
        }
    }

    #[must_use]
    pub fn is_charging_crossbow(&self) -> bool {
        self.is_charging_crossbow.load(Ordering::Relaxed)
    }

    pub fn set_charging_crossbow(&self, is_charging: bool) {
        self.is_charging_crossbow
            .store(is_charging, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(tracked_data::piglin::DATA_IS_CHARGING_CROSSBOW, is_charging);
    }

    #[must_use]
    pub fn is_dancing(&self) -> bool {
        self.is_dancing.load(Ordering::Relaxed)
    }

    pub fn set_dancing(&self, is_dancing: bool) {
        if self.is_dancing.swap(is_dancing, Ordering::Relaxed) != is_dancing {
            self.mob_entity
                .living_entity
                .entity
                .set_synced_data(tracked_data::piglin::DATA_IS_DANCING, is_dancing);
        }
    }

    // piglin: Piglin.canHunt
    #[must_use]
    pub fn can_hunt(&self) -> bool {
        !self.cannot_hunt.load(Ordering::Relaxed)
    }

    pub fn set_cannot_hunt(&self, cannot_hunt: bool) {
        self.cannot_hunt.store(cannot_hunt, Ordering::Relaxed);
    }

    #[must_use]
    pub fn off_hand_item(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::OFF_HAND)
    }

    #[must_use]
    pub fn main_hand_item(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::MAIN_HAND)
    }

    // piglin: SimpleContainer.canAddItem
    #[must_use]
    pub fn can_add_to_inventory(&self, item: &ItemStack) -> bool {
        let inv = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inv.len() < Self::INVENTORY_SIZE
            || inv.iter().any(|slot| {
                slot.are_items_and_components_equal(item)
                    && slot.item_count < slot.get_max_stack_size()
            })
    }

    // piglin: SimpleContainer.addItem
    pub fn add_to_inventory(&self, item: ItemStack) -> Option<ItemStack> {
        if item.is_empty() {
            return None;
        }
        let mut inv = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut remaining = item;
        for slot in inv.iter_mut() {
            if !slot.are_items_and_components_equal(&remaining) {
                continue;
            }
            let space = slot.get_max_stack_size().saturating_sub(slot.item_count);
            let moved = remaining.item_count.min(space);
            slot.increment(moved);
            remaining.decrement(moved);
            if remaining.is_empty() {
                return None;
            }
        }
        if inv.len() < Self::INVENTORY_SIZE {
            inv.push(remaining);
            return None;
        }
        Some(remaining)
    }

    // piglin: Piglin.canReplaceCurrentItem(ItemStack)
    #[must_use]
    pub fn can_replace_current_item_for(&self, new_item: &ItemStack) -> bool {
        let slot = mob_equipment::get_equipment_slot_for_item(new_item);
        let current = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&slot);
        Mob::can_replace_current_item(self, new_item, &current, &slot)
    }

    // piglin: Piglin.holdInOffHand
    pub(crate) fn hold_in_off_hand(&self, item: ItemStack) {
        let keep_loaded = !PiglinAi::is_barter_currency(&item);
        self.mob_entity
            .set_item_slot_and_drop_when_killed(&EquipmentSlot::OFF_HAND, item);
        if keep_loaded {
            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
    }

    // piglin: Piglin.holdInMainHand
    pub(crate) fn hold_in_main_hand(&self, item: ItemStack) {
        self.mob_entity
            .set_item_slot_and_drop_when_killed(&EquipmentSlot::MAIN_HAND, item);
        self.mob_entity
            .persistence_required
            .store(true, Ordering::Relaxed);
    }

    // piglin: thrown loved item picked up by a piglin advancement trigger
    pub(crate) fn trigger_thrower_advancement(&self, item: &ItemEntity, taken: &ItemStack) {
        let Some(thrower_uuid) = item.get_thrower() else {
            return;
        };
        let Some(thrower_player) = self
            .get_entity()
            .world
            .load()
            .get_player_by_uuid(thrower_uuid)
        else {
            return;
        };
        thrower_player.trigger_advancement(
            crate::entity::player::advancement::trigger::AdvancementTrigger::ThrownItemPickedUpByEntity {
                entity_type: "minecraft:piglin".to_string(),
                item_id: taken.item.registry_key.to_string(),
                is_baby: self.is_baby(),
                player_wearing_safe_armor: PiglinAi::is_wearing_safe_armor(
                    &thrower_player.living_entity,
                ),
            },
        );
    }

    pub fn drop_inventory(&self) {
        let items = self
            .inventory
            .try_lock()
            .map_or_else(|_| Vec::new(), |mut inv| std::mem::take(&mut *inv));
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load();
        let pos = entity.pos.load();
        for item in items {
            if !item.is_empty() {
                let item_entity =
                    ItemEntity::new(Entity::new(world.clone(), pos, &EntityType::ITEM), item);
                world.spawn_entity(Arc::new(item_entity));
            }
        }
    }

    #[must_use]
    pub fn check_piglin_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        let below = BlockPos::new(pos.0.x, pos.0.y - 1, pos.0.z);
        let state = world.get_block_state(&below);
        state.id != Block::NETHER_WART_BLOCK.default_state.id
    }

    // piglin: Piglin.createSpawnWeapon
    fn create_spawn_weapon() -> ItemStack {
        let weapon =
            if rand::random::<f32>() < PROBABILITY_OF_SPAWNING_WITH_CROSSBOW_INSTEAD_OF_SWORD {
                &Item::CROSSBOW
            } else if rand::random_range(0..10) == 0 {
                &Item::GOLDEN_SPEAR
            } else {
                &Item::GOLDEN_SWORD
            };
        ItemStack::new(1, weapon)
    }

    // piglin: Piglin.finishConversion
    fn convert_to_zombified(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load();
        let pos = entity.pos.load();

        if world.level_info.load().difficulty != pumpkin_util::Difficulty::Peaceful {
            world.play_sound(
                Sound::EntityPiglinConvertedToZombified,
                pumpkin_data::sound::SoundCategory::Hostile,
                &pos,
            );
        }

        self.mob_entity.with_brain(self, |brain| {
            PiglinAi::cancel_admiring(self, brain);
        });
        self.drop_inventory();

        let zombified = crate::entity::r#type::from_type(
            &EntityType::ZOMBIFIED_PIGLIN,
            pos,
            &world,
            uuid::Uuid::new_v4(),
        );

        let zombified_base = zombified.get_entity();
        zombified_base.set_rotation(entity.yaw.load(), entity.pitch.load());
        zombified_base.head_yaw.store(entity.head_yaw.load());
        zombified_base.velocity.store(entity.velocity.load());

        if let Some(living) = zombified.get_living_entity() {
            living.set_health(self.mob_entity.living_entity.health.load());
            living.add_effect(pumpkin_data::potion::Effect {
                effect_type: &pumpkin_data::effect::StatusEffect::NAUSEA,
                duration: 200,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
        }

        if let Some(custom_name) = &**entity.custom_name.load() {
            zombified_base.set_custom_name(custom_name.clone());
        }

        {
            let src_equip = self
                .mob_entity
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(living) = zombified.get_living_entity() {
                let mut dst_equip = living
                    .entity_equipment
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (slot, item) in &src_equip.equipment {
                    dst_equip.put(slot, item.clone());
                }
            }
        }

        world.spawn_entity(zombified);
        entity.remove();
    }
}

impl Mob for PiglinEntity {
    fn is_baby_mob(&self) -> bool {
        self.is_baby()
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityPiglinStep, 0.15, 1.0);
    }

    fn mob_metadata(&self) -> Option<MobMetadata> {
        self.is_baby().then(|| MobMetadata::new().baby(true))
    }

    fn spawn_as_baby(&self) -> bool {
        self.set_baby(true);
        true
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        PiglinAi::make_brain(self, packed)
    }

    // piglin: Piglin.finalizeSpawn
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if self.mob_entity.spawn_reason() != SpawnReason::Structure {
            if rand::random::<f32>() < PROBABILITY_OF_SPAWNING_AS_BABY {
                self.set_baby(true);
            } else if self.is_adult() {
                self.mob_entity
                    .set_item_slot(&EquipmentSlot::MAIN_HAND, Self::create_spawn_weapon());
            }
        }
        self.mob_entity.with_brain(self, |brain| {
            PiglinAi::init_memories(brain, self);
        });
        let difficulty = RegionalDifficulty::at(world, self.get_entity().pos.load());
        self.populate_default_equipment_slots(world, &difficulty);
        self.populate_default_equipment_enchantments(&difficulty);
        self.finalize_spawn_super(group_data)
    }

    // piglin: Piglin.populateDefaultEquipmentSlots, golden armour
    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        if self.is_baby() {
            return;
        }
        for (slot, item) in [
            (EquipmentSlot::HEAD, &Item::GOLDEN_HELMET),
            (EquipmentSlot::CHEST, &Item::GOLDEN_CHESTPLATE),
            (EquipmentSlot::LEGS, &Item::GOLDEN_LEGGINGS),
            (EquipmentSlot::FEET, &Item::GOLDEN_BOOTS),
        ] {
            if rand::random::<f32>() < CHANCE_OF_WEARING_EACH_ARMOUR_ITEM {
                self.mob_entity
                    .set_item_slot(&slot, ItemStack::new(1, item));
            }
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        if self.is_immune_to_zombification() {
            entity.set_synced_data(tracked_data::piglin::DATA_IMMUNE_TO_ZOMBIFICATION, true);
        }
        if self.is_baby() {
            entity.set_synced_data(tracked_data::piglin::DATA_BABY_ID, true);
        }
        if self.is_charging_crossbow() {
            entity.set_synced_data(tracked_data::piglin::DATA_IS_CHARGING_CROSSBOW, true);
        }
        if self.is_dancing() {
            entity.set_synced_data(tracked_data::piglin::DATA_IS_DANCING, true);
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("IsImmuneToZombification", self.is_immune_to_zombification());
        nbt.put_int(
            "TimeInOverworld",
            self.time_in_overworld.load(Ordering::Relaxed),
        );
        nbt.put_bool("IsBaby", self.is_baby());
        nbt.put_bool("CannotHunt", !self.can_hunt());

        let inv = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let items_tag: Vec<NbtTag> = inv
            .iter()
            .filter(|item| !item.is_empty())
            .map(|item| {
                let mut item_nbt = NbtCompound::new();
                item.write_item_stack(&mut item_nbt);
                NbtTag::Compound(item_nbt)
            })
            .collect();
        if !items_tag.is_empty() {
            nbt.put_list("Inventory", items_tag);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.mob_entity
            .set_can_pick_up_loot(nbt.get_bool("CanPickUpLoot").unwrap_or(true));
        self.set_immune_to_zombification(nbt.get_bool("IsImmuneToZombification").unwrap_or(false));
        self.time_in_overworld.store(
            nbt.get_int("TimeInOverworld").unwrap_or(0),
            Ordering::Relaxed,
        );
        self.set_baby(nbt.get_bool("IsBaby").unwrap_or(false));
        self.set_cannot_hunt(nbt.get_bool("CannotHunt").unwrap_or(false));
        if let Some(inv_list) = nbt.get_list("Inventory") {
            let mut inv = self
                .inventory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inv.clear();
            for tag in inv_list {
                if let Some(compound) = tag.extract_compound()
                    && let Some(stack) = ItemStack::read_item_stack(compound)
                {
                    inv.push(stack);
                }
            }
        }
    }

    // piglin: Piglin.mobInteract, AbstractPiglin first then PiglinAi.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if self
            .mob_entity
            .mob_interact(player, item_stack, self.can_be_leashed())
        {
            return true;
        }
        let can_admire = self
            .mob_entity
            .try_with_brain(self, |brain| PiglinAi::can_admire(self, brain, item_stack))
            .unwrap_or(false);
        if !can_admire {
            return false;
        }
        let taken = item_stack.split_unless_creative(player.gamemode.load(), 1);
        self.mob_entity.with_brain(self, |brain| {
            PiglinAi::admire_gold_item(brain);
            brain.erase(types::WALK_TARGET.id());
        });
        PiglinAi::hold_in_off_hand(self, taken);
        self.mob_entity.steered().navigator().stop();
        true
    }

    // piglin: Piglin.customServerAiStep + AbstractPiglin.customServerAiStep
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        let target = self.mob_entity.with_brain(self, |brain| {
            PiglinAi::update_activity(self, brain);
            brain.get(types::ATTACK_TARGET).cloned()
        });
        self.apply_mob_target(target);

        let world = self.get_entity().world.load();
        if self.is_converting(&world) {
            let time = self.time_in_overworld.fetch_add(1, Ordering::Relaxed) + 1;
            if time > Self::CONVERSION_TIME {
                self.convert_to_zombified();
            }
        } else {
            self.time_in_overworld.store(0, Ordering::Relaxed);
        }
    }

    // piglin: Piglin.getAmbientSound
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(
            self.mob_entity
                .try_with_brain(self, |brain| {
                    PiglinAi::get_sound_for_current_activity(self, brain)
                })
                .flatten()
                .unwrap_or(Sound::EntityPiglinAmbient),
        )
    }

    // piglin: AbstractPiglin.playAmbientSound, only while idle
    fn play_ambient_sound(&self) {
        let idle = self
            .mob_entity
            .try_with_brain(self, |brain| brain.is_active(Activity::Idle))
            .unwrap_or(false);
        if idle && let Some(sound) = self.get_ambient_sound() {
            self.make_sound(sound);
        }
    }

    // piglin: Piglin.wantsToPickUp for the sensor, with the brain at hand
    fn wants_to_pick_up_in_brain(
        &self,
        stack: &ItemStack,
        brain: &crate::entity::ai::brain::BrainState,
    ) -> bool {
        self.get_entity()
            .world
            .load()
            .level_info
            .load()
            .game_rules
            .mob_griefing
            && self.mob_entity.can_pick_up_loot()
            && PiglinAi::wants_to_pickup(self, brain, stack)
    }

    fn wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        self.mob_entity
            .try_with_brain(self, |brain| self.wants_to_pick_up_in_brain(stack, brain))
            .unwrap_or(false)
    }

    // piglin: Mob.aiStep looting uses Piglin.wantsToPickUp
    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        Mob::wants_to_pick_up(self, stack)
    }

    // piglin: Piglin.pickUpItem
    fn mob_pick_up_item(&self, item: &ItemEntity, _stack: &ItemStack) -> bool {
        PiglinAi::pick_up_item(self, item);
        true
    }

    // piglin: Piglin.hurtServer and dropCustomDeathLoot
    fn on_damage(
        &self,
        _damage_type: pumpkin_data::damage::DamageType,
        source: Option<&dyn EntityBase>,
    ) {
        if self.mob_entity.living_entity.dead.load(Ordering::Relaxed) {
            self.drop_inventory();
            return;
        }
        let Some(source) = source else {
            return;
        };
        if source.get_living_entity().is_none() {
            return;
        }
        let world = self.get_entity().world.load_full();
        let Some(attacker) = world.get_entity_by_id(source.get_entity().entity_id) else {
            return;
        };
        self.mob_entity.try_with_brain(self, |brain| {
            PiglinAi::was_hurt_by(self, brain, &world, &attacker);
        });
    }

    fn get_preferred_weapon_type(&self) -> Option<&'static tag::Tag> {
        (!self.is_baby()).then_some(&tag::Item::MINECRAFT_PIGLIN_PREFERRED_WEAPONS)
    }

    // piglin: Piglin.canReplaceCurrentItem
    fn can_replace_current_item(
        &self,
        new_item: &ItemStack,
        current_item: &ItemStack,
        slot: &EquipmentSlot,
    ) -> bool {
        if current_item.get_enchantment_level(&Enchantment::BINDING_CURSE) > 0 {
            return false;
        }
        let preferred = self.get_preferred_weapon_type();
        let new_wanted = PiglinAi::is_loved_item(new_item)
            || preferred.is_some_and(|weapons| new_item.item.has_tag(weapons));
        let current_wanted = PiglinAi::is_loved_item(current_item)
            || preferred.is_some_and(|weapons| current_item.item.has_tag(weapons));
        new_wanted && !current_wanted
            || (new_wanted || !current_wanted)
                && mob_equipment::can_replace_current_item(
                    &self.mob_entity,
                    preferred,
                    new_item,
                    current_item,
                    slot,
                )
    }

    fn as_crossbow_attack_mob(&self) -> Option<&dyn CrossbowAttackMob> {
        Some(self)
    }

    fn get_base_experience_reward(&self) -> u32 {
        Self::XP_REWARD
    }
}

impl CrossbowAttackMob for PiglinEntity {
    fn set_charging_crossbow(&self, is_charging: bool) {
        self.set_charging_crossbow(is_charging);
    }

    fn is_charging_crossbow(&self) -> bool {
        self.is_charging_crossbow()
    }
}
