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
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::mob::equipment::{RegionalDifficulty, apply_vanilla_enchantments};
use crate::entity::passive::abstract_horse::{
    ABSTRACT_HORSES, HORSE_SOUND_VOLUME, HorseCore, HorseLike, RandomStandGoal,
};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, follow_parent::FollowParentGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, run_around_like_crazy::RunAroundLikeCrazyGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{
        Mob, MobEntity,
        spawn::{SpawnGroupData, SpawnReason},
    },
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
use pumpkin_util::math::boundingbox::EntityDimensions;

const BABY_SCALE: f32 = 0.7;
const TRAP_PLAYER_RANGE_SQ: f64 = 100.0;
const TRAP_INVULNERABLE_TICKS: i32 = 60;
const TRAP_PUSH_DEVIATION: f64 = 1.1485;

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
            // skeleton_horse: AbstractHorse.registerGoals, SkeletonHorse.addBehaviourGoals is empty
            goal_selector.add_goal(1, Box::new(RunAroundLikeCrazyGoal::new(1.2)));
            goal_selector.add_goal(2, BreedGoal::with_partner_types(1.0, ABSTRACT_HORSES));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::water_avoiding(0.7)));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(9, Box::new(RandomStandGoal::default()));
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

    // skeleton_horse: SkeletonTrapGoal.canUse, hasNearbyAlivePlayer within 10 blocks
    fn has_nearby_alive_player(world: &Arc<World>, pos: Vector3<f64>) -> bool {
        world.players.load().iter().any(|player| {
            !player.is_spectator()
                && player.living_entity.is_alive()
                && player
                    .living_entity
                    .entity
                    .pos
                    .load()
                    .squared_distance_to_vec(&pos)
                    < TRAP_PLAYER_RANGE_SQ
        })
    }

    // skeleton_horse: SkeletonTrapGoal.enchant, MOB_SPAWN_EQUIPMENT provider
    fn enchant_slot(mob: &MobEntity, slot: &EquipmentSlot, difficulty: &RegionalDifficulty) {
        let mut stack = mob
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot);
        if stack.is_empty() {
            return;
        }
        apply_vanilla_enchantments(&mut stack, slot, difficulty.special_multiplier);
        mob.set_item_slot(slot, stack);
    }

    // skeleton_horse: SkeletonTrapGoal.createSkeleton
    fn create_trap_skeleton(
        world: &Arc<World>,
        pos: Vector3<f64>,
        difficulty: &RegionalDifficulty,
    ) -> Arc<dyn EntityBase> {
        let skeleton = from_type(&EntityType::SKELETON, pos, world, Uuid::new_v4());
        crate::entity::mob::spawn::finalize_spawn(&skeleton, world, None, SpawnReason::Triggered);
        if let Some(living) = skeleton.get_living_entity() {
            living
                .hurt_cooldown
                .store(TRAP_INVULNERABLE_TICKS, Ordering::Relaxed);
        }
        if let Some(mob) = skeleton.get_mob() {
            let mob_entity = mob.get_mob_entity();
            mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
            let head_empty = mob_entity
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&EquipmentSlot::HEAD)
                .is_empty();
            if head_empty {
                mob_entity
                    .set_item_slot(&EquipmentSlot::HEAD, ItemStack::new(1, &Item::IRON_HELMET));
            }
            Self::enchant_slot(mob_entity, &EquipmentSlot::MAIN_HAND, difficulty);
            Self::enchant_slot(mob_entity, &EquipmentSlot::HEAD, difficulty);
        }
        skeleton
    }

    // skeleton_horse: SkeletonTrapGoal.createHorse
    fn create_trap_horse(world: &Arc<World>, pos: Vector3<f64>) -> Arc<dyn EntityBase> {
        let horse = from_type(&EntityType::SKELETON_HORSE, pos, world, Uuid::new_v4());
        crate::entity::mob::spawn::finalize_spawn(&horse, world, None, SpawnReason::Triggered);
        if let Some(living) = horse.get_living_entity() {
            living
                .hurt_cooldown
                .store(TRAP_INVULNERABLE_TICKS, Ordering::Relaxed);
        }
        if let Some(skeleton_horse) = horse.cast_any().downcast_ref::<Self>() {
            skeleton_horse
                .mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
            skeleton_horse.set_tame(true);
            AgeableMob::set_age(skeleton_horse, 0);
        }
        horse
    }

    // skeleton_horse: SkeletonTrapGoal.tick
    fn trigger_skeleton_trap(&self, world: &Arc<World>, pos: Vector3<f64>) {
        let difficulty = RegionalDifficulty::at(world, pos);
        self.set_trap(false);
        self.set_tame(true);
        AgeableMob::set_age(self, 0);

        let bolt = from_type(&EntityType::LIGHTNING_BOLT, pos, world, Uuid::new_v4());
        if let Some(bolt) = bolt
            .cast_any()
            .downcast_ref::<crate::entity::lightning::LightningBoltEntity>()
        {
            bolt.set_visual_only(true);
        }
        world.spawn_entity(bolt);

        let rider = Self::create_trap_skeleton(world, pos, &difficulty);
        if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
            self.get_entity().add_passenger(this_base, rider.clone());
        }
        world.spawn_entity(rider);

        for _ in 0..3 {
            let horse = Self::create_trap_horse(world, pos);
            let skeleton = Self::create_trap_skeleton(world, pos, &difficulty);
            horse
                .get_entity()
                .add_passenger(horse.clone(), skeleton.clone());
            let mut rng = rand::rng();
            let push = Vector3::new(
                TRAP_PUSH_DEVIATION * f64::from(rng.random::<f32>() - rng.random::<f32>()),
                0.0,
                TRAP_PUSH_DEVIATION * f64::from(rng.random::<f32>() - rng.random::<f32>()),
            );
            horse.get_entity().add_velocity(push);
            world.spawn_entity(horse);
            world.spawn_entity(skeleton);
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

    // skeleton_horse: SkeletonHorse.canAgeUp
    fn can_age_up(&self) -> bool {
        false
    }
}

impl Animal for SkeletonHorseEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_HORSE_FOOD)
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
        // skeleton_horse: SkeletonHorse.getAmbientSound, eye in water
        if self.get_entity().is_submerged_in_water() {
            Some(Sound::EntitySkeletonHorseAmbientWater)
        } else {
            Some(Sound::EntitySkeletonHorseAmbient)
        }
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        self.horse_step_sound(pos, state);
    }

    // skeleton_horse: SkeletonHorse.getSwimSound with the gallop counter
    fn mob_get_swim_sound(&self) -> Option<Sound> {
        let entity = self.get_entity();
        if entity.on_ground.load(Ordering::Relaxed) {
            if !entity.has_passengers() {
                return Some(Sound::EntitySkeletonHorseStepWater);
            }
            let counter = self
                .core()
                .gallop_sound_counter
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if counter > 5 && counter % 3 == 0 {
                return Some(Sound::EntitySkeletonHorseGallopWater);
            }
            if counter <= 5 {
                return Some(Sound::EntitySkeletonHorseStepWater);
            }
        }
        Some(Sound::EntitySkeletonHorseSwim)
    }

    // skeleton_horse: SkeletonHorse.playSwimSound
    fn mob_play_swim_sound(&self, volume: f32) {
        if self.get_entity().on_ground.load(Ordering::Relaxed) {
            self.play_default_swim_sound(0.3);
        } else {
            self.play_default_swim_sound((volume * 25.0).min(0.1));
        }
    }

    // skeleton_horse: AbstractHorse.canMate is false
    fn can_mate(&self, _partner: &dyn EntityBase) -> bool {
        false
    }

    // skeleton_horse: SkeletonHorse.BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<EntityDimensions> {
        let adult = Entity::type_dimensions(self.get_entity().entity_type);
        Some(EntityDimensions::new(
            adult.width * BABY_SCALE,
            adult.height * BABY_SCALE,
            adult.eye_height * BABY_SCALE,
        ))
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
            if !self.mob_entity.persistence_required.load(Ordering::Relaxed)
                && self.trap_time.fetch_add(1, Ordering::Relaxed) >= TRAP_MAX_LIFE
            {
                self.get_entity().remove();
                return;
            }

            let pos = self.get_entity().pos.load();
            let world = self.get_entity().world.load();
            if Self::has_nearby_alive_player(&world, pos) {
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
