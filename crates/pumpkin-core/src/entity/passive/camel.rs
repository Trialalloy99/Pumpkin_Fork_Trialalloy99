use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering},
};

use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityPose;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data::camel::{DASH, DATA_BABY_ID, DATA_ID_FLAGS, LAST_POSE_CHANGE_TICK};
use pumpkin_data::{BlockState, entity::EntityType};
use pumpkin_inventory::Inventory;
use pumpkin_inventory::inventory::SimpleInventory;
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::server::play::SPlayerInput;
use pumpkin_util::math::position::BlockPos;
use uuid::Uuid;

use crate::entity::ai::goal::wander_around::WanderAroundGoal;
use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::r#type::from_type;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal,
    },
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};

const TEMPT_ITEMS: &[&Item] = &[&Item::CACTUS];

pub const FLAG_TAME: u8 = 2;
pub const FLAG_SADDLE: u8 = 4;
pub const FLAG_BRED: u8 = 8;
pub const FLAG_EATING: u8 = 16;
pub const FLAG_STANDING: u8 = 32;

// camel: panic goal stands up instantly and runs at 4x speed (CamelAi.CamelPanic)
pub struct CamelPanicGoal {
    inner: Box<EscapeDangerGoal>,
    camel_weak: Weak<CamelEntity>,
}

impl CamelPanicGoal {
    #[must_use]
    pub const fn new(inner: Box<EscapeDangerGoal>, camel_weak: Weak<CamelEntity>) -> Self {
        Self { inner, camel_weak }
    }
}

impl Goal for CamelPanicGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(camel) = self.camel_weak.upgrade() {
            camel.stand_up_instantly();
            camel.is_panicking.store(true, Ordering::Relaxed);
        }
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(camel) = self.camel_weak.upgrade() {
            camel.is_panicking.store(false, Ordering::Relaxed);
        }
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// camel: wander goal only executes when camel does not refuse to move (CamelAi.initIdleActivity)
pub struct CamelWanderGoal {
    inner: WanderAroundGoal,
}

impl CamelWanderGoal {
    #[must_use]
    pub const fn new(inner: WanderAroundGoal) -> Self {
        Self { inner }
    }
}

impl Goal for CamelWanderGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if let Some(camel) = mob.cast_any().downcast_ref::<CamelEntity>()
            && camel.refuse_to_move()
        {
            return false;
        }
        self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        if let Some(camel) = mob.cast_any().downcast_ref::<CamelEntity>()
            && camel.refuse_to_move()
        {
            return false;
        }
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// camel: random sitting behavior (CamelAi.RandomSitting)
pub struct CamelRandomSittingGoal {
    minimal_pose_ticks: i64,
}

impl CamelRandomSittingGoal {
    #[must_use]
    pub const fn new(minimal_pose_secs: i64) -> Self {
        Self {
            minimal_pose_ticks: minimal_pose_secs * 20,
        }
    }
}

impl Goal for CamelRandomSittingGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(camel) = mob.cast_any().downcast_ref::<CamelEntity>() else {
            return false;
        };
        let entity = camel.get_entity();
        if entity.touching_water.load(Ordering::Relaxed)
            || !entity.on_ground.load(Ordering::Relaxed)
            || camel.is_panicking.load(Ordering::Relaxed)
            || entity.has_passengers()
            || camel.is_in_pose_transition()
            || camel.get_pose_time() < self.minimal_pose_ticks
        {
            return false;
        }

        let leashed = entity
            .leashed_to
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .is_some();
        !leashed
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(camel) = mob.cast_any().downcast_ref::<CamelEntity>() {
            if camel.is_camel_sitting() {
                camel.stand_up();
            } else {
                camel.sit_down();
            }
        }
    }
}

pub struct CamelEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub flags: AtomicU8,
    pub dashing: AtomicBool,
    pub dash_cooldown: AtomicI32,
    pub last_pose_change_tick: AtomicI64,
    pub is_panicking: AtomicBool,
    pub saddle_inventory: Arc<SimpleInventory>,
}

impl CamelEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let camel = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            flags: AtomicU8::new(FLAG_TAME),
            dashing: AtomicBool::new(false),
            dash_cooldown: AtomicI32::new(0),
            last_pose_change_tick: AtomicI64::new(0),
            is_panicking: AtomicBool::new(false),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
        };
        let mob_arc = Arc::new(camel);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let camel_weak = Arc::downgrade(&mob_arc);

        let goal_selector = &mob_arc.mob_entity.goals_selector;
        goal_selector.add_goal(0, Box::new(SwimGoal::default()));
        goal_selector.add_goal(
            1,
            Box::new(CamelPanicGoal::new(EscapeDangerGoal::new(4.0), camel_weak)),
        );
        goal_selector.add_goal(2, BreedGoal::new(1.0));
        goal_selector.add_goal(3, Box::new(TemptGoal::new(2.5, TEMPT_ITEMS, false)));
        goal_selector.add_goal(4, Box::new(FollowParentGoal::new(2.5)));
        goal_selector.add_goal(5, Box::new(CamelRandomSittingGoal::new(20)));
        goal_selector.add_goal(
            6,
            Box::new(CamelWanderGoal::new(WanderAroundGoal::new(2.0))),
        );
        goal_selector.add_goal(
            7,
            LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
        );
        goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));

        mob_arc
    }

    pub fn new_camel_husk(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let camel = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            flags: AtomicU8::new(FLAG_TAME),
            dashing: AtomicBool::new(false),
            dash_cooldown: AtomicI32::new(0),
            last_pose_change_tick: AtomicI64::new(0),
            is_panicking: AtomicBool::new(false),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
        };
        let mob_arc = Arc::new(camel);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let camel_weak = Arc::downgrade(&mob_arc);

        let goal_selector = &mob_arc.mob_entity.goals_selector;
        goal_selector.add_goal(0, Box::new(SwimGoal::default()));
        goal_selector.add_goal(
            1,
            Box::new(CamelPanicGoal::new(EscapeDangerGoal::new(4.0), camel_weak)),
        );
        goal_selector.add_goal(3, Box::new(TemptGoal::new(2.5, TEMPT_ITEMS, false)));
        goal_selector.add_goal(5, Box::new(CamelRandomSittingGoal::new(20)));
        goal_selector.add_goal(
            6,
            Box::new(CamelWanderGoal::new(WanderAroundGoal::new(2.0))),
        );
        goal_selector.add_goal(
            7,
            LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
        );
        goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));

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
        entity.set_synced_data(DATA_ID_FLAGS, new_flags as i8);
    }

    #[must_use]
    pub fn is_saddled(&self) -> bool {
        self.has_flag(FLAG_SADDLE)
    }

    pub fn set_saddled(&self, val: bool) {
        self.set_flag(FLAG_SADDLE, val);
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

    #[must_use]
    pub fn is_dashing(&self) -> bool {
        self.dashing.load(Ordering::Relaxed)
    }

    pub fn set_dashing(&self, dashing: bool) {
        self.dashing.store(dashing, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(DASH, dashing);
    }

    #[must_use]
    pub fn last_pose_change_tick(&self) -> i64 {
        self.last_pose_change_tick.load(Ordering::Relaxed)
    }

    pub fn set_last_pose_change_tick(&self, val: i64) {
        self.last_pose_change_tick.store(val, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(LAST_POSE_CHANGE_TICK, val);
        self.get_entity().set_pose(if val < 0 {
            EntityPose::Sitting
        } else {
            EntityPose::Standing
        });
    }

    #[must_use]
    pub fn is_camel_sitting(&self) -> bool {
        self.last_pose_change_tick() < 0
    }

    #[must_use]
    pub fn get_pose_time(&self) -> i64 {
        let game_time = self
            .get_entity()
            .world
            .load()
            .level_time
            .lock()
            .map_or(0, |t| t.world_age);
        game_time - self.last_pose_change_tick().abs()
    }

    #[must_use]
    pub fn is_in_pose_transition(&self) -> bool {
        let pose_time = self.get_pose_time();
        let duration = if self.is_camel_sitting() { 40 } else { 52 };
        pose_time < duration
    }

    #[must_use]
    pub fn refuse_to_move(&self) -> bool {
        self.is_camel_sitting() || self.is_in_pose_transition()
    }

    // camel: sit down (Camel.sitDown)
    pub fn sit_down(&self) {
        if !self.is_camel_sitting() {
            self.play_entity_sound(self.get_sit_sound(), 1.0, 1.0);
            self.get_entity().set_pose(EntityPose::Sitting);
            let game_time = self
                .get_entity()
                .world
                .load()
                .level_time
                .lock()
                .map_or(0, |t| t.world_age);
            self.set_last_pose_change_tick(-game_time);
        }
    }

    // camel: stand up (Camel.standUp)
    pub fn stand_up(&self) {
        if self.is_camel_sitting() {
            self.play_entity_sound(self.get_stand_sound(), 1.0, 1.0);
            self.get_entity().set_pose(EntityPose::Standing);
            let game_time = self
                .get_entity()
                .world
                .load()
                .level_time
                .lock()
                .map_or(0, |t| t.world_age);
            self.set_last_pose_change_tick(game_time);
        }
    }

    // camel: stand up instantly (Camel.standUpInstantly)
    pub fn stand_up_instantly(&self) {
        if self.is_camel_sitting() {
            self.get_entity().set_pose(EntityPose::Standing);
            let game_time = self
                .get_entity()
                .world
                .load()
                .level_time
                .lock()
                .map_or(0, |t| t.world_age);
            self.set_last_pose_change_tick((game_time - 53).max(0));
        }
    }

    // camel: dash horizontal and vertical momentum (Camel.executeRidersJump)
    pub fn execute_riders_jump(&self, amount: f64) {
        let entity = self.get_entity();
        let speed = self
            .mob_entity
            .living_entity
            .get_attribute_value(&Attributes::MOVEMENT_SPEED);
        let yaw_rad = f64::from(entity.yaw.load()).to_radians();
        let look_x = -yaw_rad.sin();
        let look_z = yaw_rad.cos();
        let len = look_x.hypot(look_z).max(0.0001);
        let norm_x = look_x / len;
        let norm_z = look_z / len;

        let horizontal_scale = 22.2222 * amount * speed;
        let vertical_scale = 1.4285 * amount * 0.42;

        let mut vel = entity.velocity.load();
        vel.x += norm_x * horizontal_scale;
        vel.y += vertical_scale;
        vel.z += norm_z * horizontal_scale;
        entity.velocity.store(vel);

        self.dash_cooldown.store(55, Ordering::Relaxed);
        self.set_dashing(true);
        self.play_entity_sound(self.get_dash_sound(), 1.0, 1.0);
    }

    pub fn open_mount_inventory(&self, player: &Arc<Player>) {
        if player.can_interact_with_entity(self.get_entity(), 4.0) {
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                0,
                &player.inventory,
                Arc::new(SimpleInventory::new(0)),
                self.saddle_inventory.clone(),
                Arc::new(SimpleInventory::new(0)),
                0,
            )));
            player.open_mount_screen(handler, 0, self.get_entity().entity_id);
        }
    }

    // camel: feeding handling (Camel.handleEating)
    pub fn handle_eating(&self, player: &Player, item_stack: &mut ItemStack) -> bool {
        let living = &self.mob_entity.living_entity;
        let health = living.health.load();
        let max_health = living.get_max_health();

        let could_heal = health < max_health;
        if could_heal {
            living.heal(2.0);
        }

        let is_adult = self.get_entity().age.load(Ordering::Relaxed) == 0;
        let could_set_in_love = is_adult
            && self.get_entity().entity_type != &EntityType::CAMEL_HUSK
            && !self.mob_entity.is_in_love();
        if could_set_in_love {
            self.mob_entity
                .set_love_ticks(600, Some(player.gameprofile.id));
        }

        let could_age_up = self.is_baby();
        if could_age_up {
            self.age_up(10, true);
        }

        if !could_heal && !could_set_in_love && !could_age_up {
            return false;
        }

        let entity = self.get_entity();
        let world = entity.world.load();
        world.play_sound(
            self.get_eat_sound(),
            SoundCategory::Neutral,
            &entity.pos.load(),
        );

        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        true
    }

    #[must_use]
    pub fn get_ambient_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskAmbient
        } else {
            Sound::EntityCamelAmbient
        }
    }

    #[must_use]
    pub fn get_hurt_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskHurt
        } else {
            Sound::EntityCamelHurt
        }
    }

    #[must_use]
    pub fn get_death_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskDeath
        } else {
            Sound::EntityCamelDeath
        }
    }

    #[must_use]
    pub fn get_step_sound(&self, on_sand: bool) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            if on_sand {
                Sound::EntityCamelHuskStepSand
            } else {
                Sound::EntityCamelHuskStep
            }
        } else if on_sand {
            Sound::EntityCamelStepSand
        } else {
            Sound::EntityCamelStep
        }
    }

    #[must_use]
    pub fn get_dash_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskDash
        } else {
            Sound::EntityCamelDash
        }
    }

    #[must_use]
    pub fn get_dash_ready_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskDashReady
        } else {
            Sound::EntityCamelDashReady
        }
    }

    #[must_use]
    pub fn get_sit_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskSit
        } else {
            Sound::EntityCamelSit
        }
    }

    #[must_use]
    pub fn get_stand_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskStand
        } else {
            Sound::EntityCamelStand
        }
    }

    #[must_use]
    pub fn get_eat_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskEat
        } else {
            Sound::EntityCamelEat
        }
    }

    #[must_use]
    pub fn get_saddle_sound(&self) -> Sound {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            Sound::EntityCamelHuskSaddle
        } else {
            Sound::EntityCamelSaddle
        }
    }
}

impl AgeableMob for CamelEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for CamelEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            item_stack
                .item
                .has_tag(&tag::Item::MINECRAFT_CAMEL_HUSK_FOOD)
                || item_stack.item == &Item::CACTUS
        } else {
            item_stack.item.has_tag(&tag::Item::MINECRAFT_CAMEL_FOOD)
                || item_stack.item == &Item::CACTUS
        }
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby = from_type(
            &EntityType::CAMEL,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        baby.get_entity().age.store(-24000, Ordering::Relaxed);
        baby
    }
}

impl Mob for CamelEntity {
    // camel_husk: despawn rules (CamelHusk.removeWhenFarAway)
    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        self.get_entity().entity_type == &EntityType::CAMEL_HUSK
    }

    // camel_husk: leashing forbidden when mob controlled (CamelHusk.canBeLeashed)
    fn can_be_leashed(&self) -> bool {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            let passengers = self
                .get_entity()
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if passengers.first().is_some_and(|p| p.get_mob().is_some()) {
                return false;
            }
        }
        true
    }

    // camel husk: spear charge mount factor (CamelHusk.chargeSpeedModifier)
    fn charge_speed_modifier(&self) -> f32 {
        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            4.0
        } else {
            1.0
        }
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, state: &BlockState) {
        let on_sand = state
            .id
            .to_block()
            .has_tag(&tag::Block::MINECRAFT_CAMEL_SAND_STEP_SOUND_BLOCKS);
        self.play_entity_sound(self.get_step_sound(on_sand), 0.4, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("Saddle", self.is_saddled());
        nbt.put_long("LastPoseTick", self.last_pose_change_tick());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(saddle) = nbt.get_bool("Saddle") {
            self.set_saddled(saddle);
        }
        if let Some(pose_tick) = nbt.get_long("LastPoseTick") {
            self.set_last_pose_change_tick(pose_tick);
            if pose_tick < 0 {
                self.get_entity().set_pose(EntityPose::Sitting);
            }
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(DATA_BABY_ID, true);
        }
        entity.set_synced_data(DATA_ID_FLAGS, self.flags.load(Ordering::Relaxed) as i8);
        entity.set_synced_data(DASH, self.is_dashing());
        entity.set_synced_data(LAST_POSE_CHANGE_TICK, self.last_pose_change_tick());
        entity.set_pose(if self.is_camel_sitting() {
            EntityPose::Sitting
        } else {
            EntityPose::Standing
        });
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();

        // Water sitting standup
        if self.is_camel_sitting() && entity.touching_water.load(Ordering::Relaxed) {
            self.stand_up_instantly();
        }

        // Dashing cleanup
        let cd = self.dash_cooldown.load(Ordering::Relaxed);
        if self.is_dashing()
            && cd < 50
            && (entity.on_ground.load(Ordering::Relaxed)
                || entity.touching_water.load(Ordering::Relaxed)
                || entity.touching_lava.load(Ordering::Relaxed))
        {
            self.set_dashing(false);
        }

        if cd > 0 {
            let next_cd = cd - 1;
            self.dash_cooldown.store(next_cd, Ordering::Relaxed);
            if next_cd == 0 {
                self.play_entity_sound(self.get_dash_ready_sound(), 1.0, 1.0);
            }
        }
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        super::abstract_horse::tick_ridden_horse(self, controller);
        if let Some(player) = controller.get_player() {
            let input = player.last_input.load(Ordering::Relaxed);
            if (input & SPlayerInput::FORWARD) != 0
                && self.is_camel_sitting()
                && !self.is_in_pose_transition()
            {
                self.stand_up();
            }
        }
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        if self.refuse_to_move() || !self.is_saddled() {
            return None;
        }
        let passengers = self
            .get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        passengers.first().cloned()
    }

    fn on_damage_with_context(
        &self,
        _damage_type: DamageType,
        _source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.stand_up_instantly();
        true
    }

    fn on_player_start_jump(&self, jump_scale: i32) {
        if self.is_saddled()
            && self.dash_cooldown.load(Ordering::Relaxed) <= 0
            && self.get_entity().on_ground.load(Ordering::Relaxed)
        {
            let amount = (f64::from(jump_scale) / 100.0).clamp(0.0, 1.0);
            self.execute_riders_jump(amount);
        }
    }

    fn jump_power_multiplier(&self) -> f64 {
        0.42
    }

    fn is_tamed(&self) -> bool {
        true
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();

        if self.get_entity().entity_type == &EntityType::CAMEL_HUSK {
            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }

        // Sneak + right click opens mount inventory (if adult)
        if player.living_entity.entity.sneaking.load(Ordering::Relaxed) && !self.is_baby() {
            self.open_mount_inventory(player);
            return true;
        }

        // Feeding with Cactus / Camel food
        if self.is_food(item_stack) {
            return self.handle_eating(player, item_stack);
        }

        // Saddling
        if item == &Item::SADDLE && !self.is_saddled() && !self.is_baby() {
            self.set_saddled(true);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                self.get_saddle_sound(),
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            return true;
        }

        // Mounting (up to 2 passengers)
        let passenger_count = self.get_entity().passengers.lock().map_or(0, |p| p.len());

        if !self.is_baby() && passenger_count < 2 {
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
