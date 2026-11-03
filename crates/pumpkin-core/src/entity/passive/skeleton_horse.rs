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

use crate::entity::passive::abstract_horse::{HORSE_SOUND_VOLUME, HorseCore, HorseLike};
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
            FLAG_SADDLE, FLAG_TAME, controlling_passenger_horse, generate_jump_strength,
            tick_ridden_horse,
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
    core: HorseCore,
}

impl SkeletonHorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        // skeleton_horse: AbstractHorse.getQuadLeashOffsets
        mob_entity
            .living_entity
            .entity
            .quad_leash_params
            .store(Some([0.04, 0.52, 0.23, 0.87]));
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
            core: HorseCore::default(),
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
        self.flag(flag)
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        self.put_flag(flag, val);
    }

    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.is_horse_tame()
    }

    pub fn set_tame(&self, val: bool) {
        self.put_flag(FLAG_TAME, val);
    }

    // skeleton_horse: SkeletonHorse.randomizeAttributes
    fn randomize_attributes(&self) {
        let jump = generate_jump_strength(&mut self.get_random());
        self.mob_entity
            .living_entity
            .set_attribute_base(&Attributes::JUMP_STRENGTH, jump);
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
    pub fn is_trap(&self) -> bool {
        self.is_trap.load(Ordering::Relaxed)
    }

    pub fn set_trap(&self, trap: bool) {
        self.is_trap.store(trap, Ordering::Relaxed);
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

impl HorseLike for SkeletonHorseEntity {
    fn core(&self) -> &HorseCore {
        &self.core
    }

    fn horse_flags(&self) -> &AtomicU8 {
        &self.flags
    }

    fn horse_temper(&self) -> &AtomicI32 {
        &self.temper
    }

    fn horse_owner(&self) -> &AtomicCell<Option<Uuid>> {
        &self.owner
    }

    fn store_flags(&self, flags: u8) {
        self.flags.store(flags, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::skeleton_horse::DATA_ID_FLAGS,
            flags as i8,
        );
    }

    fn horse_saddled(&self) -> bool {
        self.is_saddled()
    }

    fn set_horse_saddle(&self, saddled: bool) {
        self.set_saddled(saddled);
    }

    fn horse_eat_sound(&self) -> Sound {
        Sound::EntityHorseEat
    }

    // SkeletonHorse has no angry sound
    fn angry_sound(&self) -> Sound {
        Sound::EntitySkeletonHorseAmbient
    }

    // skeleton_horse: SkeletonHorse.playJumpSound
    fn jump_sound(&self) -> Sound {
        if self.get_entity().is_in_water() {
            Sound::EntitySkeletonHorseJumpWater
        } else {
            Sound::EntityHorseJump
        }
    }

    fn can_gallop(&self) -> bool {
        false
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
    fn sound_volume(&self) -> f32 {
        HORSE_SOUND_VOLUME
    }

    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySkeletonHorseDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySkeletonHorseHurt)
    }
}

impl Mob for SkeletonHorseEntity {
    crate::horse_mob_hooks!();

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
            self.horse_step_sound(pos, state);
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
        self.randomize_attributes();
        self.finalize_spawn_super(group_data)
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.horse_tick();
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
        self.write_horse_nbt(nbt);
        nbt.put_bool("SkeletonTrap", self.is_trap());
        nbt.put_int("SkeletonTrapTime", self.trap_time.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.read_horse_nbt(nbt);
        if let Some(trap) = nbt.get_bool("SkeletonTrap") {
            self.set_trap(trap);
        }
        if let Some(time) = nbt.get_int("SkeletonTrapTime") {
            self.trap_time.store(time, Ordering::Relaxed);
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

    // skeleton_horse: SkeletonHorse.mobInteract, an untamed one ignores players
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if !self.is_horse_tame() {
            return false;
        }
        self.abstract_horse_interact(player, item_stack, |_, _, _| false)
    }
}
