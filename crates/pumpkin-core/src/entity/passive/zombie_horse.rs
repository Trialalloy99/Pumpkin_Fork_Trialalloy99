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
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{
        Mob, MobEntity,
        spawn::{SpawnGroupData, SpawnReason},
    },
    passive::{
        abstract_horse::{
            FLAG_SADDLE, FLAG_STANDING, FLAG_TAME, controlling_passenger_horse, feed_horse,
            play_horse_step_sound, tame_horse, throw_rider_off, tick_ridden_horse,
        },
        animal::Animal,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::RED_MUSHROOM, &Item::BAKED_POTATO];

fn generate_zombie_horse_jump_strength(rng: &mut impl rand::Rng) -> f64 {
    0.5 + rng.random::<f64>() * 0.06666666666666667
        + rng.random::<f64>() * 0.06666666666666667
        + rng.random::<f64>() * 0.06666666666666667
}

fn generate_zombie_horse_speed(rng: &mut impl rand::Rng) -> f64 {
    (9.0 + rng.random::<f64>() + rng.random::<f64>() + rng.random::<f64>()) / 42.16
}

pub struct ZombieHorseEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub flags: AtomicU8,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
}

impl ZombieHorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mut rng = rand::rng();

        // zombie_horse: vanilla attributes: 25.0 max health, jump strength, speed (ZombieHorse.createAttributes)
        mob_entity.living_entity.set_max_health(25.0);
        mob_entity.living_entity.health.store(25.0);
        let speed = generate_zombie_horse_speed(&mut rng);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, speed);
        let jump = generate_zombie_horse_jump_strength(&mut rng);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::JUMP_STRENGTH, jump);

        let horse = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
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
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
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
    pub fn has_flag(&self, flag: u8) -> bool {
        (self.flags.load(Ordering::Relaxed) & flag) != 0
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        let current = self.flags.load(Ordering::Relaxed);
        let new_flags = if val { current | flag } else { current & !flag };
        self.flags.store(new_flags, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::zombie_horse::DATA_ID_FLAGS,
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

    #[must_use]
    pub fn is_saddled(&self) -> bool {
        self.has_flag(FLAG_SADDLE)
    }

    pub fn set_saddled(&self, val: bool) {
        self.set_flag(FLAG_SADDLE, val);
        if val {
            self.saddle_inventory
                .set_stack(0, ItemStack::new(1, &Item::SADDLE));
        } else {
            self.saddle_inventory.set_stack(0, ItemStack::EMPTY.clone());
        }
    }

    #[must_use]
    pub fn is_standing(&self) -> bool {
        self.has_flag(FLAG_STANDING)
    }

    pub fn set_standing(&self, val: bool) {
        self.set_flag(FLAG_STANDING, val);
    }

    pub fn play_jump_sound(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        world.play_sound(
            Sound::EntityHorseJump,
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

impl AgeableMob for ZombieHorseEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for ZombieHorseEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack
            .item
            .has_tag(&tag::Item::MINECRAFT_ZOMBIE_HORSE_FOOD)
            || item_stack.get_item() == &Item::BAKED_POTATO
    }
}

impl CustomSound for ZombieHorseEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseHurt)
    }
}

impl Mob for ZombieHorseEntity {
    // zombie horse: spear charge mount factor (ZombieHorse.chargeSpeedModifier)
    fn charge_speed_modifier(&self) -> f32 {
        1.4
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        true
    }

    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseAmbient)
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        play_horse_step_sound(self, pos, state, false);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn sun_protection_slot(&self) -> EquipmentSlot {
        EquipmentSlot::BODY
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.is_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
    }

    fn on_player_start_jump(&self, jump_power: i32) {
        let entity = self.get_entity();
        let power_pct = (jump_power as f32 / 100.0).clamp(0.0, 1.0);
        let max_jump = self
            .mob_entity
            .living_entity
            .get_attribute_value(&Attributes::JUMP_STRENGTH) as f32;
        let actual_jump_y = (max_jump * power_pct).max(0.4);

        let mut vel = entity.velocity.load();
        vel.y = actual_jump_y as f64;
        entity.velocity.store(vel);

        self.play_jump_sound();
        self.set_standing(true);
    }

    fn on_player_stop_jump(&self) {
        self.set_standing(false);
    }

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
        throw_rider_off(self, &self.flags, Sound::EntityZombieHorseAngry);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let result = self.finalize_spawn_super(group_data);
        // zombie_horse: natural jockey spawn holding iron spear (ZombieHorse.finalizeSpawn)
        let reason = self.get_mob_entity().spawn_reason.load(Ordering::Relaxed);
        if reason == SpawnReason::Natural as u8 {
            let pos = self.get_entity().pos.load();
            let zombie = from_type(&EntityType::ZOMBIE, pos, world, Uuid::new_v4());
            if let Some(living) = zombie.get_living_entity() {
                if let Ok(mut eq) = living.entity_equipment.lock() {
                    eq.put(
                        &EquipmentSlot::MAIN_HAND,
                        ItemStack::new(1, &Item::IRON_SPEAR),
                    );
                }
                living.send_equipment_changes(&[(
                    EquipmentSlot::MAIN_HAND,
                    ItemStack::new(1, &Item::IRON_SPEAR),
                )]);
            }
            if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
                self.get_entity().add_passenger(this_base, zombie.clone());
            }
            world.spawn_entity(zombie);
        }
        result
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("Tame", self.is_tame());
        nbt.put_int("Temper", self.temper.load(Ordering::Relaxed));
        if let Some(owner) = self.owner.load() {
            nbt.put_uuid("Owner", owner);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(tame) = nbt.get_bool("Tame") {
            self.set_tame(tame);
        }
        if let Some(temper) = nbt.get_int("Temper") {
            self.temper.store(temper, Ordering::Relaxed);
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
            entity.set_synced_data(pumpkin_data::tracked_data::zombie_horse::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::zombie_horse::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        // Shift-right-click to open mount screen
        if !self.is_baby() && self.is_tame() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }

        let item = item_stack.get_item();

        // Feeding
        if self.is_food(item_stack)
            && feed_horse(
                self,
                player,
                item_stack,
                &self.flags,
                &self.temper,
                Sound::EntityZombieHorseEat,
                false,
            )
        {
            return true;
        }

        // Holding an item on untamed zombie horse makes it mad
        if !item_stack.is_empty() && !self.is_tame() {
            self.make_mad();
            return true;
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

        // Horse armor equipping
        if self.is_tame() && !self.is_baby() && super::horse::is_horse_armor(item) {
            self.armor_inventory.set_stack(0, ItemStack::new(1, item));
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityHorseArmor,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
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
