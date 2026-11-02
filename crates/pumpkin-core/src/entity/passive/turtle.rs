use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::attributes::Attributes;
use pumpkin_data::block_properties::TurtleEggLikeProperties;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::world::WorldEvent;
use pumpkin_data::{Block, BlockState};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::control::{
        Control, MoveControlTrait,
        move_control::{MoveControl, Operation},
    },
    ai::goal::{
        Controls, Goal, ParentHandle,
        breed::BreedGoal,
        escape_danger::EscapeDangerGoal,
        look_at_entity::LookAtEntityGoal,
        move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal},
        tempt::TemptGoal,
        to_goal_ticks,
        wander_around::WanderAroundGoal,
    },
    ai::pathfinder::{Navigator, node::PathType},
    ai::util::default_random_pos,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

// turtle: constants (Turtle)
const BABY_SCALE: f32 = 0.3;
const HOME_ARRIVE_DISTANCE: f64 = 7.0;
const LAY_EGG_HOME_DISTANCE: f64 = 9.0;
const TRAVEL_RANGE: i32 = 512;
const TRAVEL_CHUNK_CHECK: i32 = 34;
const LAY_EGG_TICKS: i32 = 200;
const GO_HOME_GIVE_UP_TICKS: i32 = 600;
const GO_TO_WATER_GIVE_UP_TICKS: i32 = 1200;
const GO_TO_WATER_RESET_PATH_INTERVAL: i32 = 160;
const WATER_SEARCH_RANGE: i32 = 24;
const EGG_SEARCH_RANGE: i32 = 16;

const TEMPT_ITEMS: &[&Item] = &[&Item::SEAGRASS];

/// Turtle is prey for wolves and cats.
pub fn is_baby_on_land(mob: &dyn Mob) -> bool {
    mob.as_ageable().is_some_and(AgeableMob::is_baby) && !mob.get_entity().is_in_water()
}

pub struct TurtleEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub has_egg: AtomicBool,
    pub laying_egg: AtomicBool,
    pub lay_egg_counter: AtomicI32,
    pub home_pos: Mutex<BlockPos>,
    pub travel_pos: Mutex<Option<BlockPos>>,
    pub going_home: AtomicBool,
}

impl TurtleEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Box::new(TurtleMoveControl::default());
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        *navigator = Navigator::amphibious(false);
        // turtle: pathfinding malus (Turtle constructor)
        navigator.set_pathfinding_malus(PathType::Water, 0.0);
        navigator.set_pathfinding_malus(PathType::DoorIronClosed, -1.0);
        navigator.set_pathfinding_malus(PathType::DoorWoodClosed, -1.0);
        navigator.set_pathfinding_malus(PathType::DoorOpen, -1.0);
        let turtle = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            has_egg: AtomicBool::new(false),
            laying_egg: AtomicBool::new(false),
            lay_egg_counter: AtomicI32::new(0),
            home_pos: Mutex::new(BlockPos::new(0, 0, 0)),
            travel_pos: Mutex::new(None),
            going_home: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(turtle);
        let mob_weak: std::sync::Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, EscapeDangerGoal::new(1.2).seeking_water(7));
            goal_selector.add_goal(1, BreedGoal::new(1.0));
            goal_selector.add_goal(1, Box::new(LayEggGoal::new()));
            goal_selector.add_goal(2, Box::new(TemptGoal::new(1.1, TEMPT_ITEMS, false)));
            goal_selector.add_goal(3, Box::new(GoToWaterGoal::new()));
            goal_selector.add_goal(4, Box::new(GoHomeGoal::new()));
            goal_selector.add_goal(7, Box::new(TravelGoal::new()));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(9, Box::new(TurtleRandomStrollGoal::new()));
        };

        mob_arc
    }

    fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        cell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn home_pos(&self) -> BlockPos {
        *Self::lock(&self.home_pos)
    }

    pub fn set_home_pos(&self, pos: BlockPos) {
        *Self::lock(&self.home_pos) = pos;
    }

    #[must_use]
    pub fn is_going_home(&self) -> bool {
        self.going_home.load(Ordering::Relaxed)
    }

    // turtle: BlockPos.closerToCenterThan(position, distance) around the home
    fn home_closer_than(&self, distance: f64) -> bool {
        self.home_pos()
            .to_centered_f64()
            .squared_distance_to_vec(&self.get_entity().pos.load())
            < distance * distance
    }

    #[must_use]
    pub fn has_egg(&self) -> bool {
        self.has_egg.load(Ordering::Relaxed)
    }

    pub fn set_has_egg(&self, has_egg: bool) {
        self.has_egg.store(has_egg, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::turtle::HAS_EGG, has_egg);
    }

    #[must_use]
    pub fn is_laying_egg(&self) -> bool {
        self.laying_egg.load(Ordering::Relaxed)
    }

    pub fn set_laying_egg(&self, laying_egg: bool) {
        self.lay_egg_counter
            .store(i32::from(laying_egg), Ordering::Relaxed);
        self.laying_egg.store(laying_egg, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::turtle::LAYING_EGG, laying_egg);
    }
}

impl AgeableMob for TurtleEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    // turtle: scute when growing up (Turtle.ageBoundaryReached, TURTLE_GROW)
    fn age_boundary_reached(&self) {
        if self.is_baby() {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if !world.level_info.load().game_rules.mob_drops {
            return;
        }
        for stack in crate::world::loot::generate_loot(
            &pumpkin_data::loot_table::GAMEPLAY_TURTLE_GROW,
            rand::random::<i64>(),
        ) {
            let item = crate::entity::item::ItemEntity::new(
                Entity::new(world.clone(), entity.pos.load(), &EntityType::ITEM),
                stack,
            );
            world.spawn_entity(Arc::new(item));
        }
    }
}

impl Animal for TurtleEntity {
    // turtle: Turtle.getWalkTargetValue
    fn animal_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let world = self.get_entity().world.load();
        if !self.is_going_home() && world.get_fluid(pos).has_tag(&tag::Fluid::MINECRAFT_WATER) {
            return 10.0;
        }
        if world
            .get_block(&pos.down())
            .has_tag(&tag::Block::MINECRAFT_SAND)
        {
            10.0
        } else {
            world.get_light_level_dependent_magic_value(pos) - 0.5
        }
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_TURTLE_FOOD)
            || item_stack.item == &Item::SEAGRASS
    }
}

impl Mob for TurtleEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        let sound = if self.get_entity().age.load(Ordering::Relaxed) < 0 {
            Sound::EntityTurtleShambleBaby
        } else {
            Sound::EntityTurtleShamble
        };
        self.play_entity_sound(sound, 0.15, 1.0);
    }

    fn mob_play_swim_sound(&self, volume: f32) {
        self.play_default_swim_sound(volume * 1.5);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        let home = self.home_pos();
        nbt.put(
            "home_pos",
            NbtTag::IntArray(vec![home.0.x, home.0.y, home.0.z]),
        );
        nbt.put_bool("has_egg", self.has_egg());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        let here = self.get_entity().block_pos.load();
        self.set_home_pos(
            nbt.get_int_array("home_pos")
                .and_then(|arr| (arr.len() == 3).then(|| BlockPos::new(arr[0], arr[1], arr[2])))
                .unwrap_or(here),
        );
        self.read_ageable_nbt(nbt);
        if let Some(has_egg) = nbt.get_bool("has_egg") {
            self.set_has_egg(has_egg);
        }
    }

    // turtle: spawn position becomes home (Turtle.finalizeSpawn)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<crate::entity::mob::spawn::SpawnGroupData>,
    ) -> Option<crate::entity::mob::spawn::SpawnGroupData> {
        self.set_home_pos(self.get_entity().block_pos.load());
        self.finalize_spawn_super(group_data)
    }

    // turtle: lays an egg instead of a baby (TurtleBreedGoal.breed)
    fn custom_breed(&self, mate: &dyn EntityBase) -> bool {
        let Some(mate_mob) = mate.get_mob() else {
            return false;
        };
        BreedGoal::award_breeding(self, mate, None);
        self.set_has_egg(true);
        self.mob_entity
            .breeding_cooldown
            .store(6000, Ordering::Relaxed);
        mate.set_breeding_cooldown(6000);
        self.mob_entity.reset_love_ticks();
        mate_mob.get_mob_entity().reset_love_ticks();
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if world.level_info.load().game_rules.mob_drops {
            crate::entity::experience_orb::ExperienceOrbEntity::spawn(
                &world,
                entity.pos.load(),
                rand::random_range(1..=7),
            );
        }
        true
    }

    // turtle: Turtle.canFallInLove
    fn can_fall_in_love(&self) -> bool {
        !self.has_egg()
    }

    fn mob_is_pushed_by_fluids(&self) -> bool {
        false
    }

    fn can_be_leashed(&self) -> bool {
        false
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        200
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        let entity = self.get_entity();
        (!entity.is_in_water() && entity.on_ground.load(Ordering::Relaxed) && !self.is_baby())
            .then_some(Sound::EntityTurtleAmbientLand)
    }

    // turtle: BABY_DIMENSIONS scaled to 0.3
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        let adult = self.get_entity().entity_type.dimension;
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: adult[0] * BABY_SCALE,
            height: adult[1] * BABY_SCALE,
            eye_height: self.get_entity().entity_type.eye_height * BABY_SCALE,
        })
    }

    // turtle: sinks slowly unless it has a target or is heading home (Turtle.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(living.movement_input.load(), 0.1);
        living.make_move(caller);
        let mut velocity = entity.velocity.load() * 0.9;
        if self.mob_entity.get_target().is_none()
            && (!self.is_going_home() || !self.home_closer_than(20.0))
        {
            velocity.y -= 0.005;
        }
        entity.velocity.store(velocity);
    }

    // turtle: egg laying dust while digging (Turtle.aiStep)
    fn mob_ai_step(&self) {
        let entity = self.get_entity();
        let counter = self.lay_egg_counter.load(Ordering::Relaxed);
        if entity.is_alive() && self.is_laying_egg() && counter >= 1 && counter % 5 == 0 {
            let pos = entity.block_pos.load();
            let world = entity.world.load();
            if is_sand(&world, &pos.down()) {
                world.sync_world_event(
                    WorldEvent::ParticlesDestroyBlock,
                    pos,
                    i32::from(world.get_block_state_id(&pos.down()).as_u16()),
                );
                world.emit_game_event(GameEvent::EntityAction.name(), entity.pos.load());
            }
        }
    }

    // turtle: lightning kills outright (Turtle.thunderHit)
    fn mob_on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        _lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        caller.damage_with_context(
            caller,
            f32::MAX,
            pumpkin_data::damage::DamageType::LIGHTNING_BOLT,
            None,
            None,
            None,
        );
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::turtle::DATA_BABY_ID, true);
        }
        entity.set_synced_data(pumpkin_data::tracked_data::turtle::HAS_EGG, self.has_egg());
        entity.set_synced_data(
            pumpkin_data::tracked_data::turtle::LAYING_EGG,
            self.is_laying_egg(),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityTurtleAmbientLand)
    }
}

fn turtle(mob: &dyn Mob) -> Option<&TurtleEntity> {
    mob.cast_any().downcast_ref::<TurtleEntity>()
}

// turtle: TurtleEggBlock.isSand
fn is_sand(world: &World, pos: &BlockPos) -> bool {
    world.get_block(pos).has_tag(&tag::Block::MINECRAFT_SAND)
}

fn nav_of(mob: &dyn Mob) -> std::sync::MutexGuard<'_, Navigator> {
    mob.get_mob_entity()
        .navigator
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn move_to(mob: &dyn Mob, pos: Vector3<f64>, speed: f64) {
    nav_of(mob).move_to_coords(
        pos.x,
        pos.y,
        pos.z,
        speed,
        &mob.get_mob_entity().living_entity,
    );
}

// turtle: Turtle.TurtleMoveControl
#[derive(Default)]
struct TurtleMoveControl {
    inner: MoveControl,
}

impl Control for TurtleMoveControl {}

impl MoveControlTrait for TurtleMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        let living = &mob_entity.living_entity;
        let entity = &living.entity;
        // turtle: updateSpeed
        if let Some(turtle) = turtle(mob) {
            if entity.is_in_water() {
                entity
                    .velocity
                    .store(entity.velocity.load().add_raw(0.0, 0.005, 0.0));
                if !turtle.home_closer_than(16.0) {
                    mob_entity.set_speed((living.get_speed() / 2.0).max(0.08));
                }
                if turtle.is_baby() {
                    mob_entity.set_speed((living.get_speed() / 3.0).max(0.06));
                }
            } else if entity.on_ground.load(Ordering::Relaxed) {
                mob_entity.set_speed((living.get_speed() / 2.0).max(0.06));
            }
        }
        let navigating = self.inner.operation == Operation::MoveTo
            && !mob_entity.steered().navigator().is_done();
        if !navigating {
            mob_entity.set_speed(0.0);
            return;
        }
        let pos = entity.pos.load();
        let xd = self.inner.wanted_x - pos.x;
        let yd = self.inner.wanted_y - pos.y;
        let zd = self.inner.wanted_z - pos.z;
        let distance = (xd * xd + yd * yd + zd * zd).sqrt();
        if distance < 1.0E-5 {
            mob_entity.set_speed(0.0);
            return;
        }
        let yd = yd / distance;
        let yaw = MoveControl::rotlerp(
            entity.yaw.load(),
            zd.atan2(xd).to_degrees() as f32 - 90.0,
            MoveControl::MAX_TURN,
        );
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);
        let target_speed = (self.inner.speed_modifier
            * living.get_attribute_value(&Attributes::MOVEMENT_SPEED))
            as f32;
        let speed = living.get_speed();
        mob_entity.set_speed(speed + 0.125 * (target_speed - speed));
        entity.velocity.store(entity.velocity.load().add_raw(
            0.0,
            f64::from(living.get_speed()) * yd * 0.1,
            0.0,
        ));
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.inner.set_wanted_position(x, y, z, speed_modifier);
    }

    fn strafe(&mut self, forwards: f32, right: f32) {
        self.inner.strafe(forwards, right);
    }

    fn has_wanted(&self) -> bool {
        self.inner.has_wanted()
    }

    fn speed_modifier(&self) -> f64 {
        self.inner.speed_modifier
    }
}

// turtle: TurtleGoToWaterGoal.isValidTarget
struct WaterTarget;
static WATER_TARGET: WaterTarget = WaterTarget;

impl MoveToTargetPos for WaterTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        world.get_block(&block_pos) == &Block::WATER
    }
}

// turtle: TurtleLayEggGoal.isValidTarget
struct EggTarget;
static EGG_TARGET: EggTarget = EggTarget;

impl MoveToTargetPos for EggTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        world.get_block_state(&block_pos.up()).is_air() && is_sand(&world, &block_pos)
    }
}

// turtle: Turtle.TurtleGoToWaterGoal
struct GoToWaterGoal {
    inner: MoveToTargetPosGoal<WaterTarget>,
}

impl GoToWaterGoal {
    fn new() -> Self {
        let mut inner = MoveToTargetPosGoal::with_default(
            // SAFETY: WATER_TARGET is a static.
            unsafe { ParentHandle::new(&WATER_TARGET) },
            1.0,
            WATER_SEARCH_RANGE,
        );
        inner.lowest_y = -1;
        inner.reset_path_interval = GO_TO_WATER_RESET_PATH_INTERVAL;
        Self { inner }
    }
}

impl Goal for GoToWaterGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(turtle) = turtle(mob) else {
            return false;
        };
        let in_water = mob.get_entity().is_in_water();
        let allowed = if turtle.is_baby() && !in_water {
            true
        } else {
            !turtle.is_going_home() && !in_water && !turtle.has_egg()
        };
        allowed && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let world = mob.get_entity().world.load_full();
        !mob.get_entity().is_in_water()
            && self.inner.trying_time <= GO_TO_WATER_GIVE_UP_TICKS
            && WATER_TARGET.is_target_pos(world, self.inner.target_pos)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// turtle: Turtle.TurtleLayEggGoal
struct LayEggGoal {
    inner: MoveToTargetPosGoal<EggTarget>,
}

impl LayEggGoal {
    fn new() -> Self {
        Self {
            inner: MoveToTargetPosGoal::with_default(
                // SAFETY: EGG_TARGET is a static.
                unsafe { ParentHandle::new(&EGG_TARGET) },
                1.0,
                EGG_SEARCH_RANGE,
            ),
        }
    }
}

impl Goal for LayEggGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        turtle(mob).is_some_and(|turtle| {
            turtle.has_egg()
                && turtle.home_closer_than(LAY_EGG_HOME_DISTANCE)
                && self.inner.can_start(mob)
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
            && turtle(mob).is_some_and(|turtle| {
                turtle.has_egg() && turtle.home_closer_than(LAY_EGG_HOME_DISTANCE)
            })
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
        let Some(turtle) = turtle(mob) else { return };
        let entity = mob.get_entity();
        if entity.is_in_water() || !self.inner.is_reached_target() {
            return;
        }
        let counter = turtle.lay_egg_counter.load(Ordering::Relaxed);
        if counter < 1 {
            turtle.set_laying_egg(true);
        } else if counter > to_goal_ticks(LAY_EGG_TICKS) {
            let world = entity.world.load_full();
            let pos = entity.block_pos.load();
            world.play_sound_fine(
                Sound::EntityTurtleLayEgg,
                SoundCategory::Blocks,
                &pos.to_centered_f64(),
                0.3,
                0.9 + rand::random::<f32>() * 0.2,
            );
            // turtle: egg is placed above the sand block (TurtleLayEggGoal.tick, blockPos.above())
            let egg_pos = self.inner.get_target_pos().up();
            let mut props = TurtleEggLikeProperties::default(&Block::TURTLE_EGG);
            props.eggs = rand::random_range(1..=4);
            world.set_block_state(
                &egg_pos,
                props.to_state_id(&Block::TURTLE_EGG),
                BlockFlags::NOTIFY_ALL,
            );
            world.emit_game_event(GameEvent::BlockPlace.name(), egg_pos.to_centered_f64());
            turtle.set_has_egg(false);
            turtle.set_laying_egg(false);
            turtle.mob_entity.set_love_ticks(600, None);
        }
        if turtle.is_laying_egg() {
            turtle.lay_egg_counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// turtle: Turtle.TurtleGoHomeGoal
struct GoHomeGoal {
    stuck: bool,
    close_to_home_try_ticks: i32,
}

impl GoHomeGoal {
    const fn new() -> Self {
        Self {
            stuck: false,
            close_to_home_try_ticks: 0,
        }
    }
}

impl Goal for GoHomeGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(turtle) = turtle(mob) else {
            return false;
        };
        if turtle.is_baby() {
            return false;
        }
        if turtle.has_egg() {
            return true;
        }
        mob.get_random().random_range(0..to_goal_ticks(700)) == 0 && !turtle.home_closer_than(64.0)
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(turtle) = turtle(mob) {
            turtle.going_home.store(true, Ordering::Relaxed);
        }
        self.stuck = false;
        self.close_to_home_try_ticks = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(turtle) = turtle(mob) {
            turtle.going_home.store(false, Ordering::Relaxed);
        }
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        turtle(mob).is_some_and(|turtle| {
            !turtle.home_closer_than(HOME_ARRIVE_DISTANCE)
                && !self.stuck
                && self.close_to_home_try_ticks <= to_goal_ticks(GO_HOME_GIVE_UP_TICKS)
        })
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(turtle) = turtle(mob) else { return };
        let close_to_home = turtle.home_closer_than(16.0);
        if close_to_home {
            self.close_to_home_try_ticks += 1;
        }
        if !nav_of(mob).is_done() {
            return;
        }
        let home = turtle.home_pos();
        let home_vec = Vector3::new(
            f64::from(home.0.x) + 0.5,
            f64::from(home.0.y),
            f64::from(home.0.z) + 0.5,
        );
        let mut next =
            default_random_pos::get_pos_towards(mob, 16, 3, home_vec, std::f64::consts::PI / 10.0)
                .or_else(|| {
                    default_random_pos::get_pos_towards(
                        mob,
                        8,
                        7,
                        home_vec,
                        std::f64::consts::FRAC_PI_2,
                    )
                });
        if let Some(pos) = next
            && !close_to_home
        {
            let world = mob.get_entity().world.load();
            let at = BlockPos::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );
            if world.get_block(&at) != &Block::WATER {
                next = default_random_pos::get_pos_towards(
                    mob,
                    16,
                    5,
                    home_vec,
                    std::f64::consts::FRAC_PI_2,
                );
            }
        }
        match next {
            Some(pos) => move_to(mob, pos, 1.0),
            None => self.stuck = true,
        }
    }
}

// turtle: Turtle.TurtleTravelGoal
struct TravelGoal {
    stuck: bool,
}

impl TravelGoal {
    const fn new() -> Self {
        Self { stuck: false }
    }
}

impl Goal for TravelGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        turtle(mob).is_some_and(|turtle| {
            !turtle.is_going_home() && !turtle.has_egg() && mob.get_entity().is_in_water()
        })
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(turtle) = turtle(mob) else { return };
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        let (xt, mut yt, zt) = {
            let mut rng = mob.get_random();
            (
                rng.random_range(0..=2 * TRAVEL_RANGE) - TRAVEL_RANGE,
                rng.random_range(0..9) - 4,
                rng.random_range(0..=2 * TRAVEL_RANGE) - TRAVEL_RANGE,
            )
        };
        if f64::from(yt) + pos.y > f64::from(entity.world.load().sea_level - 1) {
            yt = 0;
        }
        *TurtleEntity::lock(&turtle.travel_pos) = Some(BlockPos::new(
            (f64::from(xt) + pos.x).floor() as i32,
            (f64::from(yt) + pos.y).floor() as i32,
            (f64::from(zt) + pos.z).floor() as i32,
        ));
        self.stuck = false;
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(turtle) = turtle(mob) else { return };
        let Some(travel) = *TurtleEntity::lock(&turtle.travel_pos) else {
            self.stuck = true;
            return;
        };
        if !nav_of(mob).is_done() {
            return;
        }
        let target = Vector3::new(
            f64::from(travel.0.x) + 0.5,
            f64::from(travel.0.y),
            f64::from(travel.0.z) + 0.5,
        );
        let mut next =
            default_random_pos::get_pos_towards(mob, 16, 3, target, std::f64::consts::PI / 10.0)
                .or_else(|| {
                    default_random_pos::get_pos_towards(
                        mob,
                        8,
                        7,
                        target,
                        std::f64::consts::FRAC_PI_2,
                    )
                });
        if let Some(pos) = next {
            let (xc, zc) = (pos.x.floor() as i32, pos.z.floor() as i32);
            let world = mob.get_entity().world.load();
            let loaded = [(-1, -1), (1, -1), (-1, 1), (1, 1)].iter().all(|(dx, dz)| {
                world.is_loaded(&BlockPos::new(
                    xc + dx * TRAVEL_CHUNK_CHECK,
                    pos.y.floor() as i32,
                    zc + dz * TRAVEL_CHUNK_CHECK,
                ))
            });
            if !loaded {
                next = None;
            }
        }
        match next {
            Some(pos) => move_to(mob, pos, 1.0),
            None => self.stuck = true,
        }
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        turtle(mob).is_some_and(|turtle| {
            !nav_of(mob).is_done()
                && !self.stuck
                && !turtle.is_going_home()
                && !turtle.mob_entity.is_in_love()
                && !turtle.has_egg()
        })
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(turtle) = turtle(mob) {
            *TurtleEntity::lock(&turtle.travel_pos) = None;
        }
    }
}

// turtle: Turtle.TurtleRandomStrollGoal, interval 100
struct TurtleRandomStrollGoal {
    inner: WanderAroundGoal,
}

impl TurtleRandomStrollGoal {
    const fn new() -> Self {
        Self {
            inner: WanderAroundGoal::with_interval(1.0, 100),
        }
    }
}

impl Goal for TurtleRandomStrollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        turtle(mob).is_some_and(|turtle| {
            !mob.get_entity().is_in_water() && !turtle.is_going_home() && !turtle.has_egg()
        }) && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
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
