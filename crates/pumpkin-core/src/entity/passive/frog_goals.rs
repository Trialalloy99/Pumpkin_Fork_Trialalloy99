use std::sync::atomic::Ordering;

use pumpkin_data::{
    Block,
    attributes::Attributes,
    entity::EntityPose,
    game_event::GameEvent,
    sound::{Sound, SoundCategory},
    tag::{self, Taggable},
};
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use rand::RngExt;

use super::frog::FrogEntity;
use crate::entity::{
    EntityBase,
    ai::goal::{Controls, Goal, to_goal_ticks, wander_around::WanderAroundGoal},
    mob::Mob,
};
use crate::world::World;

// frog: constants (FrogAi, ShootTongue, LongJumpToRandomPos)
const TONGUE_EATING_DISTANCE: f64 = 1.75;
const TONGUE_PULL_FACTOR: f64 = 0.75;
const TONGUE_CATCH_TICKS: i32 = 6;
const TONGUE_TOTAL_TICKS: i32 = 10;
const TONGUE_PATH_RECALCULATE_TICKS: i32 = 10;
const TONGUE_TIME_OUT: i32 = 100;
const TONGUE_WALK_SPEED: f64 = 2.0;
const CROAK_TICKS: i32 = 60;
const TIME_BETWEEN_LONG_JUMPS_MIN: i32 = 100;
const TIME_BETWEEN_LONG_JUMPS_MAX: i32 = 140;
const MAX_LONG_JUMP_HEIGHT: i32 = 2;
const MAX_LONG_JUMP_WIDTH: i32 = 4;
const MAX_JUMP_VELOCITY_MULTIPLIER: f64 = 3.571_428_8;
const PREFERRED_BLOCKS_CHANCE: f32 = 0.5;
const FIND_JUMP_TRIES: i32 = 20;
const PREPARE_JUMP_TICKS: i64 = 40;
const ALLOWED_ANGLES: [i32; 4] = [65, 70, 75, 80];
const MIN_PATHFIND_DISTANCE_TO_VALID_JUMP: i32 = 8;
const SPEED_MULTIPLIER_ON_LAND: f64 = 1.0;
const SPEED_MULTIPLIER_IN_WATER: f64 = 0.75;

fn frog(mob: &dyn Mob) -> Option<&FrogEntity> {
    mob.cast_any().downcast_ref::<FrogEntity>()
}

fn world_age(mob: &dyn Mob) -> i64 {
    mob.get_entity().world.load().get_world_age()
}

pub fn time_between_long_jumps() -> i32 {
    rand::random_range(TIME_BETWEEN_LONG_JUMPS_MIN..=TIME_BETWEEN_LONG_JUMPS_MAX)
}

// frog: FrogAi.updateActivity, TONGUE > LAY_SPAWN > LONG_JUMP > SWIM > IDLE
pub fn tongue_activity(mob: &dyn Mob) -> bool {
    mob.get_mob_entity().get_target().is_some()
}

fn lay_spawn_activity(mob: &dyn Mob) -> bool {
    frog(mob).is_some_and(|frog| frog.is_pregnant() && !frog.is_mid_jump()) && !tongue_activity(mob)
}

pub fn long_jump_activity(mob: &dyn Mob) -> bool {
    let Some(frog) = frog(mob) else { return false };
    !tongue_activity(mob)
        && !lay_spawn_activity(mob)
        && frog.long_jump_cooldown.load(Ordering::Relaxed) <= 0
        && !mob.get_mob_entity().is_in_love()
        && !frog.is_tempted()
        && !mob.get_entity().is_in_water()
}

fn swim_activity(mob: &dyn Mob) -> bool {
    frog(mob).is_some_and(|frog| !frog.is_mid_jump())
        && mob.get_entity().is_in_water()
        && !tongue_activity(mob)
        && !lay_spawn_activity(mob)
        && !long_jump_activity(mob)
}

fn idle_activity(mob: &dyn Mob) -> bool {
    frog(mob).is_some_and(|frog| !frog.is_mid_jump())
        && !mob.get_entity().is_in_water()
        && !tongue_activity(mob)
        && !lay_spawn_activity(mob)
        && !long_jump_activity(mob)
}

pub fn idle_or_swim(mob: &dyn Mob) -> bool {
    idle_activity(mob) || swim_activity(mob)
}

pub fn idle_only(mob: &dyn Mob) -> bool {
    idle_activity(mob)
}

pub fn swim_only(mob: &dyn Mob) -> bool {
    swim_activity(mob)
}

pub fn lay_spawn_only(mob: &dyn Mob) -> bool {
    lay_spawn_activity(mob)
}

pub fn land_speed(mob: &dyn Mob) -> f64 {
    if mob.get_entity().is_in_water() {
        SPEED_MULTIPLIER_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

fn nav_move_to(mob: &dyn Mob, pos: Vector3<f64>, speed: f64) {
    mob.get_mob_entity()
        .navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .move_to_coords(
            pos.x,
            pos.y,
            pos.z,
            speed,
            &mob.get_mob_entity().living_entity,
        );
}

// frog: ShootTongue, state machine MOVE_TO_TARGET -> CATCH_ANIMATION -> EAT_ANIMATION -> DONE
#[derive(PartialEq, Eq, Clone, Copy)]
enum TongueState {
    MoveToTarget,
    CatchAnimation,
    EatAnimation,
    Done,
}

pub struct ShootTongueGoal {
    state: TongueState,
    eat_animation_timer: i32,
    calculate_path_counter: i32,
    running_ticks: i32,
}

impl ShootTongueGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: TongueState::Done,
            eat_animation_timer: 0,
            calculate_path_counter: 0,
            running_ticks: 0,
        }
    }

    // frog: canPathfindToTarget, reachable within 1.75 blocks
    fn can_pathfind_to_target(mob: &dyn Mob, target: &dyn EntityBase) -> bool {
        let mut navigator = mob
            .get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pos = target.get_entity().pos.load();
        navigator
            .create_path(&mob.get_mob_entity().living_entity, pos, 0)
            .is_some_and(|path| {
                let end = path.get_target().to_centered_f64();
                end.squared_distance_to_vec(&pos).sqrt() < TONGUE_EATING_DISTANCE
            })
    }

    fn eat_entity(frog: &FrogEntity) {
        let entity = frog.get_entity();
        let world = entity.world.load();
        world.play_sound_fine(
            Sound::EntityFrogEat,
            SoundCategory::Neutral,
            &entity.pos.load(),
            2.0,
            1.0,
        );
        if let Some(target) = frog.tongue_target()
            && target.get_entity().is_alive()
        {
            frog.get_mob_entity().try_attack(frog, target.as_ref());
            if !target.get_entity().is_alive() {
                target.get_entity().remove();
            }
        }
    }
}

impl Default for ShootTongueGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for ShootTongueGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(frog) = frog(mob) else { return false };
        let Some(target) = mob.get_mob_entity().get_target() else {
            return false;
        };
        if mob.get_mob_entity().is_in_love() {
            return false;
        }
        let reachable = Self::can_pathfind_to_target(mob, target.as_ref());
        if !reachable {
            mob.get_mob_entity().set_target(None);
            frog.add_unreachable_tongue_target(target.get_entity().entity_uuid, world_age(mob));
        }
        reachable
            && mob.get_entity().pose.load() != EntityPose::Croaking
            && FrogEntity::can_eat(target.as_ref())
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        mob.get_mob_entity().get_target().is_some()
            && self.state != TongueState::Done
            && self.running_ticks < TONGUE_TIME_OUT
            && !frog(mob).is_some_and(FrogEntity::is_panicking)
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(frog) = frog(mob) else { return };
        let Some(target) = mob.get_mob_entity().get_target() else {
            return;
        };
        frog.set_tongue_target(Some(target.as_ref()));
        nav_move_to(mob, target.get_entity().pos.load(), TONGUE_WALK_SPEED);
        self.calculate_path_counter = TONGUE_PATH_RECALCULATE_TICKS;
        self.state = TongueState::MoveToTarget;
        self.running_ticks = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().set_target(None);
        if let Some(frog) = frog(mob) {
            frog.set_tongue_target(None);
        }
        mob.get_entity().set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(frog) = frog(mob) else { return };
        let Some(target) = mob.get_mob_entity().get_target() else {
            return;
        };
        self.running_ticks += 1;
        frog.set_tongue_target(Some(target.as_ref()));
        let entity = mob.get_entity();
        match self.state {
            TongueState::MoveToTarget => {
                let frog_pos = entity.pos.load();
                let target_pos = target.get_entity().pos.load();
                if target_pos.squared_distance_to_vec(&frog_pos).sqrt() < TONGUE_EATING_DISTANCE {
                    entity.world.load().play_sound_fine(
                        Sound::EntityFrogTongue,
                        SoundCategory::Neutral,
                        &frog_pos,
                        2.0,
                        1.0,
                    );
                    entity.set_pose(EntityPose::UsingTongue);
                    let pull = (frog_pos - target_pos).normalize() * TONGUE_PULL_FACTOR;
                    target.get_entity().velocity.store(pull);
                    self.eat_animation_timer = 0;
                    self.state = TongueState::CatchAnimation;
                } else if self.calculate_path_counter <= 0 {
                    nav_move_to(mob, target_pos, TONGUE_WALK_SPEED);
                    self.calculate_path_counter = TONGUE_PATH_RECALCULATE_TICKS;
                } else {
                    self.calculate_path_counter -= 1;
                }
            }
            TongueState::CatchAnimation => {
                let timer = self.eat_animation_timer;
                self.eat_animation_timer += 1;
                if timer >= TONGUE_CATCH_TICKS {
                    self.state = TongueState::EatAnimation;
                    Self::eat_entity(frog);
                }
            }
            TongueState::EatAnimation => {
                if self.eat_animation_timer >= TONGUE_TOTAL_TICKS {
                    self.state = TongueState::Done;
                } else {
                    self.eat_animation_timer += 1;
                }
            }
            TongueState::Done => {}
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// frog: Croak, 60 ticks of croaking while standing on land
pub struct CroakGoal {
    croak_counter: i32,
    cooldown: i32,
}

impl CroakGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            croak_counter: 0,
            cooldown: 0,
        }
    }
}

impl Default for CroakGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for CroakGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        // frog: IDLE and LAY_SPAWN RunOne weights, croak is picked in 3 of 9 rolls
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        self.cooldown = to_goal_ticks(rand::random_range(40..100));
        mob.get_entity().pose.load() == EntityPose::Standing
            && mob.is_navigator_idle()
            && rand::random_range(0..3) == 0
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.croak_counter < CROAK_TICKS
    }

    fn start(&mut self, mob: &dyn Mob) {
        if !mob.get_entity().is_in_water() {
            mob.get_entity().set_pose(EntityPose::Croaking);
            self.croak_counter = 0;
        } else {
            self.croak_counter = CROAK_TICKS;
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        mob.get_entity().set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        self.croak_counter += 1;
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// frog: TryFindLand(range, speed), leave the water for dry land
pub struct TryFindLandGoal {
    range: i32,
    speed: f64,
    next_ok_start_time: i64,
}

impl TryFindLandGoal {
    #[must_use]
    pub const fn new(range: i32, speed: f64) -> Self {
        Self {
            range,
            speed,
            next_ok_start_time: 0,
        }
    }
}

impl Goal for TryFindLandGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if mob.get_mob_entity().get_target().is_some() || !mob.is_navigator_idle() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load();
        let body = entity.block_pos.load();
        if !world.get_fluid(&body).has_tag(&tag::Fluid::MINECRAFT_WATER) {
            return false;
        }
        let now = world.get_world_age();
        if now < self.next_ok_start_time {
            self.next_ok_start_time = now + 60;
            return false;
        }
        self.next_ok_start_time = now + 60;
        let Some(target) = find_by_manhattan(&body, self.range, |pos| {
            (pos.0.x != body.0.x || pos.0.z != body.0.z)
                && world.get_fluid(pos).has_tag(&tag::Fluid::MINECRAFT_WATER) == false
                && world
                    .get_block_state(pos)
                    .get_block_collision_shapes_at(pos)
                    .next()
                    .is_none()
                && world.get_block_state(&pos.down()).is_solid()
        }) else {
            return false;
        };
        nav_move_to(mob, target.to_centered_f64(), self.speed);
        true
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// frog: TryFindLandNearLiquid(8, 1.0, FROG_TRIES_TO_FIND_LAND_NEAR) while pregnant
pub struct TryFindLandNearLiquidGoal {
    range: i32,
    speed: f64,
    next_ok_start_time: i64,
}

impl TryFindLandNearLiquidGoal {
    #[must_use]
    pub const fn new(range: i32, speed: f64) -> Self {
        Self {
            range,
            speed,
            next_ok_start_time: 0,
        }
    }
}

impl Goal for TryFindLandNearLiquidGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if mob.get_mob_entity().get_target().is_some() || !mob.is_navigator_idle() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load();
        let body = entity.block_pos.load();
        if world
            .get_fluid(&body)
            .has_tag(&tag::Fluid::MINECRAFT_FROG_TRIES_TO_FIND_LAND_NEAR)
        {
            return false;
        }
        let now = world.get_world_age();
        if now < self.next_ok_start_time {
            self.next_ok_start_time = now + 40;
            return false;
        }
        self.next_ok_start_time = now + 40;
        let Some(target) = find_by_manhattan(&body, self.range, |pos| {
            if pos.0.x == body.0.x && pos.0.z == body.0.z {
                return false;
            }
            if world
                .get_block_state(pos)
                .get_block_collision_shapes_at(pos)
                .next()
                .is_some()
            {
                return false;
            }
            let below = pos.down();
            if world
                .get_block_state(&below)
                .get_block_collision_shapes_at(&below)
                .next()
                .is_none()
            {
                return false;
            }
            [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dz)| {
                let side = BlockPos::new(pos.0.x + dx, pos.0.y, pos.0.z + dz);
                world.get_block_state(&side).is_air()
                    && world
                        .get_fluid(&side.down())
                        .has_tag(&tag::Fluid::MINECRAFT_FROG_TRIES_TO_FIND_LAND_NEAR)
            })
        }) else {
            return false;
        };
        nav_move_to(mob, target.to_centered_f64(), self.speed);
        true
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// frog: BlockGetter.findBlocksInBoxByManhattanDistance, nearest first
fn find_by_manhattan(
    origin: &BlockPos,
    range: i32,
    predicate: impl Fn(&BlockPos) -> bool,
) -> Option<BlockPos> {
    for distance in 0..=range {
        for x in -distance..=distance {
            for y in -(distance - x.abs())..=(distance - x.abs()) {
                let rest = distance - x.abs() - y.abs();
                let zs: &[i32] = if rest == 0 { &[0] } else { &[rest, -rest] };
                for &z in zs {
                    let pos = BlockPos::new(origin.0.x + x, origin.0.y + y, origin.0.z + z);
                    if predicate(&pos) {
                        return Some(pos);
                    }
                }
            }
        }
    }
    None
}

// frog: TryLaySpawnOnFluidNearLand(FROGSPAWN)
pub struct TryLaySpawnGoal;

impl Goal for TryLaySpawnGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(frog) = frog(mob) else { return false };
        let entity = mob.get_entity();
        if mob.get_mob_entity().get_target().is_some()
            || !frog.is_pregnant()
            || entity.is_in_water()
            || !entity.on_ground.load(Ordering::Relaxed)
        {
            return false;
        }
        let world = entity.world.load_full();
        let below = entity.block_pos.load().down();
        for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
            let relative = BlockPos::new(below.0.x + dx, below.0.y, below.0.z + dz);
            let no_top_face = world
                .get_block_state(&relative)
                .get_block_collision_shapes_at(&relative)
                .next()
                .is_none();
            if no_top_face
                && (world
                    .get_fluid(&relative)
                    .has_tag(&tag::Fluid::MINECRAFT_SUPPORTS_FROGSPAWN)
                    || world
                        .get_block(&relative)
                        .has_tag(&tag::Block::MINECRAFT_SUPPORTS_FROGSPAWN))
            {
                let spawn_pos = relative.up();
                if world.get_block_state(&spawn_pos).is_air() {
                    world.set_block_state(
                        &spawn_pos,
                        Block::FROGSPAWN.default_state.id,
                        pumpkin_world::world::BlockFlags::NOTIFY_ALL,
                    );
                    world
                        .emit_game_event(GameEvent::BlockPlace.name(), spawn_pos.to_centered_f64());
                    world.play_sound_fine(
                        Sound::EntityFrogLaySpawn,
                        SoundCategory::Blocks,
                        &entity.pos.load(),
                        1.0,
                        1.0,
                    );
                    frog.set_pregnant(false);
                    return false;
                }
            }
        }
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }
}

// frog: stroll with the swim and land speeds of the activity
pub struct FrogStrollGoal {
    inner: WanderAroundGoal,
}

impl FrogStrollGoal {
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self {
            inner: WanderAroundGoal::new(speed),
        }
    }
}

impl Goal for FrogStrollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        mob.is_navigator_idle() && self.inner.can_start(mob)
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

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// frog: LongJumpMidJump, long jumping pose until landing
pub struct LongJumpMidJumpGoal;

impl Goal for LongJumpMidJumpGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        frog(mob).is_some_and(FrogEntity::is_mid_jump)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        !mob.get_entity().on_ground.load(Ordering::Relaxed)
    }

    fn start(&mut self, mob: &dyn Mob) {
        mob.get_entity().set_pose(EntityPose::LongJumping);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        if entity.on_ground.load(Ordering::Relaxed) {
            let velocity = entity.velocity.load();
            entity
                .velocity
                .store(Vector3::new(velocity.x * 0.1, velocity.y, velocity.z * 0.1));
            entity.world.load().play_sound_fine(
                Sound::EntityFrogStep,
                SoundCategory::Neutral,
                &entity.pos.load(),
                2.0,
                1.0,
            );
        }
        entity.set_pose(EntityPose::Standing);
        if let Some(frog) = frog(mob) {
            frog.set_mid_jump(false);
            frog.long_jump_cooldown
                .store(time_between_long_jumps(), Ordering::Relaxed);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}

// frog: LongJumpToPreferredBlock(2, 4, 3.5714288, FROG_PREFER_JUMP_TO, 0.5)
pub struct LongJumpGoal {
    candidates: Vec<(BlockPos, i32)>,
    not_preferred: Vec<(BlockPos, i32)>,
    initial_position: Option<Vector3<f64>>,
    chosen_jump: Option<Vector3<f64>>,
    find_jump_tries: i32,
    prepare_jump_start: i64,
    wanting_preferred: bool,
}

impl LongJumpGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            candidates: Vec::new(),
            not_preferred: Vec::new(),
            initial_position: None,
            chosen_jump: None,
            find_jump_tries: 0,
            prepare_jump_start: 0,
            wanting_preferred: false,
        }
    }

    // frog: WeightedRandom.getRandomItem over distance weights, removes the pick
    fn take_candidate(&mut self) -> Option<(BlockPos, i32)> {
        let total: i32 = self.candidates.iter().map(|(_, weight)| *weight).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = rand::random_range(0..total);
        let index = self.candidates.iter().position(|(_, weight)| {
            roll -= *weight;
            roll < 0
        })?;
        Some(self.candidates.remove(index))
    }

    // frog: LongJumpToPreferredBlock.getJumpCandidate
    fn next_candidate(&mut self, world: &World) -> Option<(BlockPos, i32)> {
        if !self.wanting_preferred {
            return self.take_candidate();
        }
        while !self.candidates.is_empty() {
            if let Some(candidate) = self.take_candidate() {
                if world
                    .get_block(&candidate.0.down())
                    .has_tag(&tag::Block::MINECRAFT_FROG_PREFER_JUMP_TO)
                {
                    return Some(candidate);
                }
                self.not_preferred.push(candidate);
            }
        }
        (!self.not_preferred.is_empty()).then(|| self.not_preferred.remove(0))
    }

    // frog: FrogAi.isAcceptableLandingSpot + LongJumpToRandomPos.defaultAcceptableLandingSpot
    fn acceptable_landing_spot(world: &World, pos: &BlockPos) -> bool {
        let below = pos.down();
        if world.get_fluid(pos).id != pumpkin_data::fluid::Fluid::EMPTY.id
            || world.get_fluid(&below).id != pumpkin_data::fluid::Fluid::EMPTY.id
            || world.get_fluid(&pos.up()).id != pumpkin_data::fluid::Fluid::EMPTY.id
        {
            return false;
        }
        if world
            .get_block(pos)
            .has_tag(&tag::Block::MINECRAFT_FROG_PREFER_JUMP_TO)
            || world
                .get_block(&below)
                .has_tag(&tag::Block::MINECRAFT_FROG_PREFER_JUMP_TO)
        {
            return true;
        }
        world.get_block_state(&below).is_solid_render()
    }

    // frog: LongJumpUtil.calculateJumpVectorForAngle
    fn jump_vector_for_angle(
        mob: &dyn Mob,
        target: Vector3<f64>,
        max_velocity: f64,
        angle: i32,
    ) -> Option<Vector3<f64>> {
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        let plane = Vector3::new(target.x - pos.x, 0.0, target.z - pos.z).normalize() * 0.5;
        let aim = target - plane;
        let direction = aim - pos;
        let angle_rad = f64::from(angle as f32 * std::f32::consts::PI / 180.0);
        let xz_angle = direction.z.atan2(direction.x);
        let r2 = direction.x * direction.x + direction.z * direction.z;
        let r = r2.sqrt();
        let y = direction.y;
        let gravity = mob.get_gravity();
        let sin_2_angle = (2.0 * angle_rad).sin();
        let cos_angle_sq = angle_rad.cos().powi(2);
        let v0_sq = r2 * gravity / (r * sin_2_angle - 2.0 * y * cos_angle_sq);
        if v0_sq < 0.0 {
            return None;
        }
        let v0 = v0_sq.sqrt();
        if v0 > max_velocity {
            return None;
        }
        let v0_r = v0 * angle_rad.cos();
        let v0_y = v0 * angle_rad.sin();
        let world = entity.world.load();
        let dims = pumpkin_util::math::boundingbox::EntityDimensions::new(
            entity.entity_type.dimension[0],
            entity.entity_type.dimension[1],
            entity.entity_type.eye_height,
        );
        let samples = (r / v0_r).ceil() as i32 * 2;
        let mut ri = 0.0;
        let mut previous: Option<Vector3<f64>> = None;
        for _ in 0..(samples - 1) {
            ri += r / f64::from(samples);
            let yi = angle_rad.sin() / angle_rad.cos() * ri
                - ri.powi(2) * gravity / (2.0 * v0_sq * angle_rad.cos().powi(2));
            let sample = Vector3::new(
                pos.x + ri * xz_angle.cos(),
                pos.y + yi,
                pos.z + ri * xz_angle.sin(),
            );
            if let Some(prev) = previous
                && !Self::clear_transition(&world, &dims, prev, sample)
            {
                return None;
            }
            previous = Some(sample);
        }
        Some(Vector3::new(v0_r * xz_angle.cos(), v0_y, v0_r * xz_angle.sin()) * 0.95)
    }

    fn clear_transition(
        world: &World,
        dims: &pumpkin_util::math::boundingbox::EntityDimensions,
        from: Vector3<f64>,
        to: Vector3<f64>,
    ) -> bool {
        let direction = to - from;
        let min_dimension = f64::from(dims.width.min(dims.height));
        let checks = (direction.length() / min_dimension).ceil() as i32;
        let normalized = direction.normalize();
        let mut next = from;
        for i in 0..checks {
            next = if i == checks - 1 {
                to
            } else {
                next + normalized * (min_dimension * 0.9)
            };
            let half = f64::from(dims.width) / 2.0;
            let bounds = BoundingBox::new(
                Vector3::new(next.x - half, next.y, next.z - half),
                Vector3::new(
                    next.x + half,
                    next.y + f64::from(dims.height),
                    next.z + half,
                ),
            );
            if !world.is_space_empty(bounds) {
                return false;
            }
        }
        true
    }
}

impl Default for LongJumpGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for LongJumpGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(frog) = frog(mob) else { return false };
        if !long_jump_activity(mob) || frog.is_mid_jump() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load();
        let body = entity.block_pos.load();
        let can_start = entity.on_ground.load(Ordering::Relaxed)
            && !entity.is_in_water()
            && !entity.touching_lava.load(Ordering::Relaxed)
            && world.get_block(&body) != &Block::HONEY_BLOCK;
        if !can_start {
            frog.long_jump_cooldown
                .store(time_between_long_jumps() / 2, Ordering::Relaxed);
        }
        can_start
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(frog) = frog(mob) else { return false };
        let valid = self
            .initial_position
            .is_some_and(|initial| initial == mob.get_entity().pos.load())
            && self.find_jump_tries > 0
            && !mob.get_entity().is_in_water()
            && (self.chosen_jump.is_some() || !self.candidates.is_empty());
        if !valid && !frog.is_mid_jump() {
            frog.long_jump_cooldown
                .store(time_between_long_jumps() / 2, Ordering::Relaxed);
        }
        valid
    }

    fn start(&mut self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        let body = entity.block_pos.load();
        self.chosen_jump = None;
        self.find_jump_tries = FIND_JUMP_TRIES;
        self.initial_position = Some(entity.pos.load());
        self.not_preferred.clear();
        self.wanting_preferred = mob.get_random().random::<f32>() < PREFERRED_BLOCKS_CHANCE;
        self.candidates.clear();
        for x in -MAX_LONG_JUMP_WIDTH..=MAX_LONG_JUMP_WIDTH {
            for y in -MAX_LONG_JUMP_HEIGHT..=MAX_LONG_JUMP_HEIGHT {
                for z in -MAX_LONG_JUMP_WIDTH..=MAX_LONG_JUMP_WIDTH {
                    if x == 0 && y == 0 && z == 0 {
                        continue;
                    }
                    let dist_sq = x * x + y * y + z * z;
                    self.candidates.push((
                        BlockPos::new(body.0.x + x, body.0.y + y, body.0.z + z),
                        dist_sq,
                    ));
                }
            }
        }
    }

    fn stop(&mut self, _mob: &dyn Mob) {
        self.chosen_jump = None;
        self.initial_position = None;
        self.candidates.clear();
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(frog) = frog(mob) else { return };
        let entity = mob.get_entity();
        let world = entity.world.load_full();
        let now = world.get_world_age();
        if let Some(jump) = self.chosen_jump {
            if now - self.prepare_jump_start >= PREPARE_JUMP_TICKS {
                entity.yaw.store(entity.body_yaw.load());
                let length = jump.length();
                let boost = 0.0;
                entity.velocity.store(jump * ((length + boost) / length));
                frog.set_mid_jump(true);
                world.play_sound_fine(
                    Sound::EntityFrogLongJump,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                    1.0,
                    1.0,
                );
            }
            return;
        }
        self.find_jump_tries -= 1;
        let body = entity.block_pos.load();
        let max_velocity = mob
            .get_mob_entity()
            .living_entity
            .get_attribute_value(&Attributes::JUMP_STRENGTH)
            * MAX_JUMP_VELOCITY_MULTIPLIER;
        while let Some((target, _)) = self.next_candidate(&world) {
            if target.0.x == body.0.x && target.0.z == body.0.z {
                continue;
            }
            if !Self::acceptable_landing_spot(&world, &target) {
                continue;
            }
            let center = target.to_centered_f64();
            let mut angles = ALLOWED_ANGLES;
            {
                let mut rng = mob.get_random();
                for i in (1..angles.len()).rev() {
                    angles.swap(i, rng.random_range(0..=i));
                }
            }
            let vector = angles
                .iter()
                .find_map(|angle| Self::jump_vector_for_angle(mob, center, max_velocity, *angle));
            let Some(vector) = vector else { continue };
            let reachable_by_path = mob
                .get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_path(
                    &mob.get_mob_entity().living_entity,
                    center,
                    MIN_PATHFIND_DISTANCE_TO_VALID_JUMP,
                )
                .is_some_and(|path| path.can_reach());
            if !reachable_by_path {
                self.chosen_jump = Some(vector);
                self.prepare_jump_start = now;
                return;
            }
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}
