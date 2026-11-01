use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::{
        abstract_horse::{
            FLAG_SADDLE, FLAG_STANDING, FLAG_TAME, controlling_passenger_horse,
            generate_jump_strength, play_horse_step_sound, tick_ridden_horse,
        },
        animal::Animal,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TRAP_MAX_LIFE: i32 = 18000;

pub struct SkeletonHorseEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub flags: AtomicU8,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub is_trap: AtomicBool,
    pub trap_time: AtomicI32,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
}

impl SkeletonHorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mut rng = rand::rng();

        // skeleton_horse: vanilla attributes: 15.0 max health, 0.2 speed, random jump strength (SkeletonHorse.createAttributes)
        mob_entity.living_entity.set_max_health(15.0);
        mob_entity.living_entity.health.store(15.0);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.2);
        let jump = generate_jump_strength(&mut rng);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::JUMP_STRENGTH, jump);

        let horse = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            flags: AtomicU8::new(0),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            is_trap: AtomicBool::new(false),
            trap_time: AtomicI32::new(0),
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
            pumpkin_data::tracked_data::skeleton_horse::DATA_ID_FLAGS,
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

    #[must_use]
    pub fn is_trap(&self) -> bool {
        self.is_trap.load(Ordering::Relaxed)
    }

    pub fn set_trap(&self, trap: bool) {
        self.is_trap.store(trap, Ordering::Relaxed);
    }

    pub fn play_jump_sound(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        if entity.is_in_water() {
            world.play_sound(
                Sound::EntitySkeletonHorseJumpWater,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
        } else {
            world.play_sound(
                Sound::EntityHorseJump,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
        }
    }

    // skeleton_horse: trap trigger logic (SkeletonTrapGoal.tick)
    fn trigger_skeleton_trap(&self, world: &Arc<World>, pos: Vector3<f64>) {
        self.set_trap(false);
        self.set_tame(true);
        self.get_entity().age.store(0, Ordering::Relaxed);
        self.mob_entity
            .persistence_required
            .store(true, Ordering::Relaxed);

        // Lightning strike
        let bolt = from_type(&EntityType::LIGHTNING_BOLT, pos, world, Uuid::new_v4());
        world.spawn_entity(bolt);

        // Skeleton rider on this horse
        let rider = from_type(&EntityType::SKELETON, pos, world, Uuid::new_v4());
        if let Some(living) = rider.get_living_entity() {
            if let Ok(mut eq) = living.entity_equipment.lock() {
                eq.put(&EquipmentSlot::HEAD, ItemStack::new(1, &Item::IRON_HELMET));
                eq.put(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, &Item::BOW));
            }
            living.send_equipment_changes(&[
                (EquipmentSlot::HEAD, ItemStack::new(1, &Item::IRON_HELMET)),
                (EquipmentSlot::MAIN_HAND, ItemStack::new(1, &Item::BOW)),
            ]);
        }
        if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
            self.get_entity().add_passenger(this_base, rider.clone());
        }
        world.spawn_entity(rider);

        // 3 additional skeleton horses with skeleton riders
        for _ in 0..3 {
            let mut rng = rand::rng();
            let offset_x = (rng.random::<f64>() - 0.5) * 2.0;
            let offset_z = (rng.random::<f64>() - 0.5) * 2.0;
            let p = Vector3::new(pos.x + offset_x, pos.y, pos.z + offset_z);

            let horse = from_type(&EntityType::SKELETON_HORSE, p, world, Uuid::new_v4());
            if let Some(mob) = horse.get_mob() {
                mob.get_mob_entity()
                    .persistence_required
                    .store(true, Ordering::Relaxed);
            }

            let skel = from_type(&EntityType::SKELETON, p, world, Uuid::new_v4());
            if let Some(living) = skel.get_living_entity() {
                if let Ok(mut eq) = living.entity_equipment.lock() {
                    eq.put(&EquipmentSlot::HEAD, ItemStack::new(1, &Item::IRON_HELMET));
                    eq.put(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, &Item::BOW));
                }
                living.send_equipment_changes(&[
                    (EquipmentSlot::HEAD, ItemStack::new(1, &Item::IRON_HELMET)),
                    (EquipmentSlot::MAIN_HAND, ItemStack::new(1, &Item::BOW)),
                ]);
            }
            horse
                .get_entity()
                .add_passenger(horse.clone(), skel.clone());
            world.spawn_entity(horse);
            world.spawn_entity(skel);
        }
    }
}

impl AgeableMob for SkeletonHorseEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for SkeletonHorseEntity {
    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }
}

impl CustomSound for SkeletonHorseEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySkeletonHorseDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySkeletonHorseHurt)
    }
}

impl Mob for SkeletonHorseEntity {
    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        let entity = self.get_entity();
        // skeleton_horse: water ambient sound when in water (SkeletonHorse.getAmbientSound)
        if entity.is_in_water() {
            Some(Sound::EntitySkeletonHorseAmbientWater)
        } else {
            Some(Sound::EntitySkeletonHorseAmbient)
        }
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        if self.get_entity().is_in_water() {
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntitySkeletonHorseStepWater,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
        } else {
            play_horse_step_sound(self, pos, state, false);
        }
    }

    fn mob_play_swim_sound(&self, volume: f32) {
        let entity = self.get_entity();
        let world = entity.world.load();
        if entity.on_ground.load(Ordering::Relaxed) {
            world.play_sound(
                Sound::EntitySkeletonHorseStepWater,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
        } else {
            let vol = (volume * 25.0).min(0.1);
            if vol > 0.0 {
                world.play_sound(
                    Sound::EntitySkeletonHorseSwim,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
            }
        }
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
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

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_super(group_data)
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        // skeleton_horse: trap despawn and trigger check (SkeletonHorse.aiStep, SkeletonTrapGoal)
        if self.is_trap() {
            if !self.mob_entity.persistence_required.load(Ordering::Relaxed) {
                let time = self.trap_time.fetch_add(1, Ordering::Relaxed) + 1;
                if time >= TRAP_MAX_LIFE {
                    self.get_entity().remove();
                    return;
                }
            }

            let pos = self.get_entity().pos.load();
            let world = self.get_entity().world.load();
            if world.get_closest_player(pos, 10.0).is_some() {
                self.trigger_skeleton_trap(&world, pos);
            }
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("Tame", self.is_tame());
        nbt.put_int("Temper", self.temper.load(Ordering::Relaxed));
        nbt.put_bool("SkeletonTrap", self.is_trap());
        nbt.put_int("SkeletonTrapTime", self.trap_time.load(Ordering::Relaxed));
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
        if let Some(trap) = nbt.get_bool("SkeletonTrap") {
            self.set_trap(trap);
        }
        if let Some(time) = nbt.get_int("SkeletonTrapTime") {
            self.trap_time.store(time, Ordering::Relaxed);
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
            entity.set_synced_data(
                pumpkin_data::tracked_data::skeleton_horse::DATA_BABY_ID,
                true,
            );
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::skeleton_horse::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        // skeleton_horse: cannot interact if untamed (SkeletonHorse.mobInteract)
        if !self.is_tame() {
            return false;
        }

        let item = item_stack.get_item();

        // Shift-right-click to open mount screen
        if !self.is_baby() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }

        // Saddling
        if item == &Item::SADDLE && !self.is_saddled() && !self.is_baby() {
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
