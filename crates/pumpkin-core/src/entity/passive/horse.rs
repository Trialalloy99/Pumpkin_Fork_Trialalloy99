use std::sync::{
    Arc, Weak,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        run_around_like_crazy::RunAroundLikeCrazyGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::{
        abstract_horse::{
            FLAG_BRED, FLAG_STANDING, FLAG_TAME, calculate_offspring_attributes,
            controlling_passenger_horse, feed_horse, generate_jump_strength, generate_max_health,
            generate_speed, tame_horse, throw_rider_off, tick_ridden_horse,
        },
        animal::Animal,
        mule::MuleEntity,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

pub use crate::entity::passive::abstract_horse::play_horse_step_sound;

const TEMPT_ITEMS: &[&Item] = &[
    &Item::GOLDEN_APPLE,
    &Item::ENCHANTED_GOLDEN_APPLE,
    &Item::GOLDEN_CARROT,
];

pub struct HorseEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub flags: AtomicU8,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
}

impl HorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mut rng = rand::rng();
        let color = rng.random_range(0..7);
        let style = rng.random_range(0..5);
        let variant = color | (style << 8);

        let max_health = generate_max_health(&mut rng);
        let jump_strength = generate_jump_strength(&mut rng);
        let speed = generate_speed(&mut rng);

        mob_entity.living_entity.set_max_health(max_health);
        mob_entity.living_entity.health.store(max_health);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, speed);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::JUMP_STRENGTH, jump_strength);

        let horse = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(variant),
            flags: AtomicU8::new(0),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
            armor_inventory: Arc::new(SimpleInventory::new(1)),
        };

        let mob_arc = Arc::new(horse);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            // horse: vanilla goal priorities (AbstractHorse.registerGoals)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(1, Box::new(RunAroundLikeCrazyGoal::new(1.2)));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::new(0.7)));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn get_variant(&self) -> i32 {
        self.variant.load(Ordering::Relaxed)
    }

    pub fn set_variant(&self, variant: i32) {
        self.variant.store(variant, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_TYPE_VARIANT,
            VarInt(variant),
        );
    }

    #[must_use]
    pub fn has_flag(&self, flag: u8) -> bool {
        (self.flags.load(Ordering::Relaxed) & flag) != 0
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        let current = self.flags.load(Ordering::Relaxed);
        let new_flags = if val { current | flag } else { current & !flag };
        self.flags.store(new_flags, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_FLAGS,
            new_flags as i8,
        );
    }

    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.has_flag(FLAG_TAME)
    }

    pub fn set_tame(&self, val: bool) {
        self.set_flag(FLAG_TAME, val);
    }

    // horse: saddle lives in SADDLE slot (AbstractHorse.isSaddled)
    #[must_use]
    pub fn is_saddled(&self) -> bool {
        let equipment = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        equipment
            .equipment
            .get(&EquipmentSlot::SADDLE)
            .is_some_and(|stack| !stack.is_empty())
    }

    pub fn set_saddled(&self, val: bool) {
        if val {
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::SADDLE,
                ItemStack::new(1, &Item::SADDLE),
            );
            self.saddle_inventory
                .set_stack(0, ItemStack::new(1, &Item::SADDLE));
        } else {
            self.mob_entity
                .set_item_slot(&EquipmentSlot::SADDLE, ItemStack::EMPTY.clone());
            self.saddle_inventory.set_stack(0, ItemStack::EMPTY.clone());
        }
    }

    // horse: body armor in BODY slot (Horse.isWearingArmor)
    #[must_use]
    pub fn is_wearing_armor(&self) -> bool {
        let equipment = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        equipment
            .equipment
            .get(&EquipmentSlot::BODY)
            .is_some_and(|stack| !stack.is_empty())
    }

    pub fn equip_armor(&self, item_stack: &ItemStack) {
        self.mob_entity
            .set_item_slot_and_drop_when_killed(&EquipmentSlot::BODY, item_stack.clone());
        self.armor_inventory.set_stack(0, item_stack.clone());
        let entity = self.get_entity();
        let world = entity.world.load();
        world.play_sound(
            Sound::EntityHorseArmor,
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

#[must_use]
pub fn is_horse_armor(item: &Item) -> bool {
    item == &Item::LEATHER_HORSE_ARMOR
        || item == &Item::COPPER_HORSE_ARMOR
        || item == &Item::IRON_HORSE_ARMOR
        || item == &Item::GOLDEN_HORSE_ARMOR
        || item == &Item::DIAMOND_HORSE_ARMOR
        || item == &Item::NETHERITE_HORSE_ARMOR
}

impl AgeableMob for HorseEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for HorseEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_HORSE_FOOD)
            || item_stack.item == &Item::WHEAT
            || item_stack.item == &Item::SUGAR
            || item_stack.item == &Item::HAY_BLOCK
            || item_stack.item == &Item::APPLE
            || item_stack.item == &Item::CARROT
            || item_stack.item == &Item::GOLDEN_CARROT
            || item_stack.item == &Item::GOLDEN_APPLE
            || item_stack.item == &Item::ENCHANTED_GOLDEN_APPLE
    }

    // horse: breeding genetics and cross breeding with Donkey (Horse.getBreedOffspring)
    #[allow(clippy::too_many_lines)]
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let mut rng = rand::rng();

        // Horse + Donkey = Mule!
        if partner.get_entity().entity_type == &EntityType::DONKEY {
            let mule_base = from_type(
                &EntityType::MULE,
                self.get_entity().pos.load(),
                &world,
                Uuid::new_v4(),
            );
            if let Some(mule) = mule_base.cast_any().downcast_ref::<MuleEntity>() {
                let partner_health = partner
                    .get_mob()
                    .map_or(20.0, |m| m.get_mob_entity().living_entity.get_max_health());
                let partner_speed = partner.get_mob().map_or(0.175, |m| {
                    m.get_mob_entity()
                        .living_entity
                        .get_attribute_value(&Attributes::MOVEMENT_SPEED)
                });

                let (health, jump, speed) = calculate_offspring_attributes(
                    self.mob_entity.living_entity.get_max_health(),
                    partner_health,
                    0.7,
                    0.5,
                    self.mob_entity
                        .living_entity
                        .get_attribute_value(&Attributes::MOVEMENT_SPEED),
                    partner_speed,
                    &mut rng,
                );
                mule.mob_entity.living_entity.set_max_health(health);
                mule.mob_entity.living_entity.health.store(health);
                mule.mob_entity
                    .living_entity
                    .set_attribute_base(&Attributes::MOVEMENT_SPEED, speed);
                mule.mob_entity
                    .living_entity
                    .set_attribute_base(&Attributes::JUMP_STRENGTH, jump);
                mule.set_baby(true);
            }
            return mule_base;
        }

        let baby_base = from_type(
            &EntityType::HORSE,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );

        if let Some(baby) = baby_base.cast_any().downcast_ref::<Self>() {
            // Inheritance: 4/9 parent 1, 4/9 parent 2, 1/9 random
            let select_skin = rng.random_range(0..9);
            let p1_color = self.get_variant() & 0xFF;
            let p2_color = partner
                .cast_any()
                .downcast_ref::<Self>()
                .map_or(0, |h| h.get_variant() & 0xFF);
            let color = if select_skin < 4 {
                p1_color
            } else if select_skin < 8 {
                p2_color
            } else {
                rng.random_range(0..7)
            };

            let select_marking = rng.random_range(0..5);
            let p1_style = (self.get_variant() >> 8) & 0xFF;
            let p2_style = partner
                .cast_any()
                .downcast_ref::<Self>()
                .map_or(0, |h| (h.get_variant() >> 8) & 0xFF);
            let style = if select_marking < 2 {
                p1_style
            } else if select_marking < 4 {
                p2_style
            } else {
                rng.random_range(0..5)
            };

            baby.set_variant(color | (style << 8));
            let partner_health = partner
                .get_mob()
                .map_or(20.0, |m| m.get_mob_entity().living_entity.get_max_health());
            let partner_speed = partner.get_mob().map_or(0.225, |m| {
                m.get_mob_entity()
                    .living_entity
                    .get_attribute_value(&Attributes::MOVEMENT_SPEED)
            });

            let (health, jump, speed) = calculate_offspring_attributes(
                self.mob_entity.living_entity.get_max_health(),
                partner_health,
                0.7,
                0.7,
                self.mob_entity
                    .living_entity
                    .get_attribute_value(&Attributes::MOVEMENT_SPEED),
                partner_speed,
                &mut rng,
            );
            baby.mob_entity.living_entity.set_max_health(health);
            baby.mob_entity.living_entity.health.store(health);
            baby.mob_entity
                .living_entity
                .set_attribute_base(&Attributes::MOVEMENT_SPEED, speed);
            baby.mob_entity
                .living_entity
                .set_attribute_base(&Attributes::JUMP_STRENGTH, jump);
            baby.set_baby(true);
        }

        baby_base
    }
}

impl CustomSound for HorseEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityBabyHorseDeath
        } else {
            Sound::EntityHorseDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityBabyHorseHurt
        } else {
            Sound::EntityHorseHurt
        })
    }
}

impl Mob for HorseEntity {
    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        play_horse_step_sound(self, pos, state, true);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityBabyHorseAmbient
        } else {
            Sound::EntityHorseAmbient
        })
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.is_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
    }

    // horse: jump rearing animation and sound (AbstractHorse.handleStartJump)
    fn on_player_start_jump(&self, _jump_scale: i32) {
        if self.is_saddled() {
            self.set_flag(FLAG_STANDING, true);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityHorseJump,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
        }
    }

    fn on_player_stop_jump(&self) {
        self.set_flag(FLAG_STANDING, false);
    }

    // horse: open mount screen with 0 chest columns (AbstractHorse.openCustomInventoryScreen)
    fn open_custom_inventory(&self, player: &Arc<Player>) {
        if !self.is_baby() && self.is_tamed() {
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                0,
                &player.inventory,
                Arc::new(SimpleInventory::new(0)),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                0,
            )));
            player.open_mount_screen(handler, 0, self.get_entity().entity_id);
        }
    }

    fn get_temper(&self) -> i32 {
        self.temper.load(Ordering::Relaxed)
    }

    fn max_temper(&self) -> i32 {
        100
    }

    fn modify_temper(&self, delta: i32) {
        let cur = self.temper.load(Ordering::Relaxed);
        self.temper
            .store((cur + delta).clamp(0, 100), Ordering::Relaxed);
    }

    fn tame_with_name(&self, player: &Player) -> bool {
        tame_horse(self, player, &self.flags, &self.owner)
    }

    fn make_mad(&self) {
        let sound = if self.is_baby() {
            Sound::EntityBabyHorseAngry
        } else {
            Sound::EntityHorseAngry
        };
        throw_rider_off(self, &self.flags, sound);
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut rng = rand::rng();
        let color = rng.random_range(0..7);
        let style = rng.random_range(0..5);
        self.set_variant(color | (style << 8));
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_int("Variant", self.get_variant());
        nbt.put_bool("Tame", self.is_tame());
        nbt.put_int("Temper", self.temper.load(Ordering::Relaxed));
        nbt.put_bool("Bred", self.has_flag(FLAG_BRED));
        if let Some(owner) = self.owner.load() {
            nbt.put_uuid("Owner", owner);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(variant) = nbt.get_int("Variant") {
            self.set_variant(variant);
        }
        if let Some(tame) = nbt.get_bool("Tame") {
            self.set_tame(tame);
        }
        if let Some(temper) = nbt.get_int("Temper") {
            self.temper.store(temper, Ordering::Relaxed);
        }
        if let Some(bred) = nbt.get_bool("Bred") {
            self.set_flag(FLAG_BRED, bred);
        }
        if let Some(owner) = nbt.get_uuid("Owner") {
            self.owner.store(Some(owner));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::horse::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_TYPE_VARIANT,
            VarInt(self.get_variant()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();

        // Shift-right-click to open inventory
        if !self.is_baby() && self.is_tame() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }

        // Feeding
        if self.is_food(item_stack) {
            let eating_sound = if self.is_baby() {
                Sound::EntityBabyHorseEat
            } else {
                Sound::EntityHorseEat
            };
            if feed_horse(
                self,
                player,
                item_stack,
                &self.flags,
                &self.temper,
                eating_sound,
                true,
            ) {
                return true;
            }
        }

        // Saddling
        if self.is_tame() && item == &Item::SADDLE && !self.is_saddled() && !self.is_baby() {
            self.set_saddled(true);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityHorseSaddle,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            return true;
        }

        // Horse armor
        if self.is_tame() && is_horse_armor(item) && !self.is_wearing_armor() && !self.is_baby() {
            self.equip_armor(item_stack);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            return true;
        }

        // Riding
        if !self.is_baby() {
            let world = player.world();
            let ent = &self.mob_entity.living_entity.entity;
            if let Some(vehicle) = world.get_entity_by_id(ent.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                ent.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
                return true;
            }
        }

        false
    }
}
