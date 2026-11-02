use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use pumpkin_data::attributes::Attributes;
use pumpkin_data::tag::Taggable;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use super::sulfur_cube::SulfurCubeEntity;
use crate::entity::EntityBase;
use crate::entity::ai::control::look_control::LookControl;
use crate::entity::ai::control::move_control::MoveControl;
use crate::entity::ai::control::{Control, LookControlTrait, MoveControlTrait};
use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::mob::Mob;
use crate::entity::player::Player;

const FLOAT_WANTED_SPEED: f64 = 1.2;
const KEEP_JUMPING_WANTED_SPEED: f64 = 1.0;
const TEMPT_STOP_DISTANCE: f64 = 1.0;
const TEMPT_CALM_DOWN_TICKS: i32 = 100;
const ITEM_SEARCH_RANGE: f64 = 8.0;
const LOOK_ROTATION_STEP: f32 = 10.0;
const MAX_TURN: f32 = 90.0;

// cube mob: AbstractCubeMob.CubeMobMoveControl, SulfurCubeMobMoveControl skips it with a body item
pub struct SulfurCubeMoveControl {
    cube: Weak<SulfurCubeEntity>,
}

impl SulfurCubeMoveControl {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self { cube }
    }
}

impl Control for SulfurCubeMoveControl {}

impl MoveControlTrait for SulfurCubeMoveControl {
    // cube mob: setWantedMovement
    fn set_wanted_position(&mut self, _x: f64, _y: f64, _z: f64, speed_modifier: f64) {
        if let Some(cube) = self.cube.upgrade() {
            cube.set_wanted_movement(speed_modifier);
        }
    }

    fn strafe(&mut self, _forward: f32, _right: f32) {
        if let Some(cube) = self.cube.upgrade() {
            cube.set_wait();
        }
    }

    fn has_wanted(&self) -> bool {
        self.cube
            .upgrade()
            .is_some_and(|cube| cube.has_wanted_movement())
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(cube) = self.cube.upgrade() else {
            return;
        };
        if cube.has_body_item() {
            return;
        }
        let mob_entity = mob.get_mob_entity();
        let living = &mob_entity.living_entity;
        let entity = &living.entity;

        let yaw = MoveControl::rotlerp(entity.yaw.load(), cube.target_yaw(), MAX_TURN);
        entity.yaw.store(yaw);
        entity.head_yaw.store(yaw);
        entity.body_yaw.store(yaw);

        let Some(speed_modifier) = cube.take_wanted_movement() else {
            let input = living.movement_input.load();
            living
                .movement_input
                .store(Vector3::new(input.x, input.y, 0.0));
            return;
        };
        let speed =
            (speed_modifier * living.get_attribute_value(&Attributes::MOVEMENT_SPEED)) as f32;
        mob_entity.set_speed(speed);
        if entity.on_ground.load(Ordering::Relaxed) {
            if cube.tick_jump_delay() {
                let mut next_delay = cube.get_jump_delay();
                if cube.is_aggressive() {
                    next_delay /= 3;
                }
                cube.set_jump_delay(next_delay);
                mob_entity
                    .jump_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .jump();
                cube.play_jump_sound();
            } else {
                let input = living.movement_input.load();
                living.movement_input.store(Vector3::new(0.0, input.y, 0.0));
                mob_entity.set_speed(0.0);
            }
        }
    }
}

// sulfur cube: SulfurCubeLookControl, a body item only snaps the facing to 90 degrees
pub struct SulfurCubeLookControl {
    inner: LookControl,
}

impl SulfurCubeLookControl {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: LookControl::default(),
        }
    }
}

impl Default for SulfurCubeLookControl {
    fn default() -> Self {
        Self::new()
    }
}

impl Control for SulfurCubeLookControl {}

impl LookControlTrait for SulfurCubeLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let has_body = mob
            .cast_any()
            .downcast_ref::<SulfurCubeEntity>()
            .is_some_and(SulfurCubeEntity::has_body_item);
        if !has_body {
            LookControlTrait::tick(&mut self.inner, mob);
            return;
        }
        let entity = mob.get_entity();
        let yaw = entity.yaw.load();
        let close_angle = wrap_degrees_90(yaw);
        entity.yaw.store(yaw - close_angle);
        entity.head_yaw.store(entity.yaw.load());
    }

    fn looking_at(&self) -> Option<Vector3<f64>> {
        self.inner.looking_at()
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        max_yaw_change: f32,
        max_pitch_change: f32,
    ) {
        self.inner
            .look_at_with_range(x, y, z, max_yaw_change, max_pitch_change);
    }
}

// vanilla Mth.wrapDegrees90
pub(super) fn wrap_degrees_90(angle: f32) -> f32 {
    let normalized = angle % 90.0;
    if normalized >= 45.0 {
        normalized - 90.0
    } else if normalized < -45.0 {
        normalized + 90.0
    } else {
        normalized
    }
}

// cube mob: AbstractCubeMob.CubeMobFloatGoal
pub struct CubeFloatGoal {
    cube: Weak<SulfurCubeEntity>,
}

impl CubeFloatGoal {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self { cube }
    }
}

impl Goal for CubeFloatGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.cube.upgrade().is_some_and(|cube| {
            let entity = cube.get_entity();
            !cube.has_body_item()
                && (entity.touching_water.load(Ordering::Relaxed)
                    || entity.touching_lava.load(Ordering::Relaxed))
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(cube) = self.cube.upgrade() else {
            return;
        };
        if cube.get_random().random::<f32>() < 0.8 {
            cube.jump_now();
        }
        cube.set_wanted_movement(FLOAT_WANTED_SPEED);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::JUMP | Controls::MOVE
    }
}

// cube mob: AbstractCubeMob.CubeMobRandomDirectionGoal
pub struct CubeRandomDirectionGoal {
    cube: Weak<SulfurCubeEntity>,
    chosen_degrees: f32,
    next_randomize_time: i32,
}

impl CubeRandomDirectionGoal {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self {
            cube,
            chosen_degrees: 0.0,
            next_randomize_time: 0,
        }
    }
}

impl Goal for CubeRandomDirectionGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.cube.upgrade().is_some_and(|cube| {
            let entity = cube.get_entity();
            !cube.has_body_item()
                && cube.get_mob_entity().get_target().is_none()
                && (entity.on_ground.load(Ordering::Relaxed)
                    || entity.touching_water.load(Ordering::Relaxed)
                    || entity.touching_lava.load(Ordering::Relaxed)
                    || cube
                        .get_mob_entity()
                        .living_entity
                        .has_effect(&pumpkin_data::effect::StatusEffect::LEVITATION))
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(cube) = self.cube.upgrade() else {
            return;
        };
        self.next_randomize_time -= 1;
        if self.next_randomize_time <= 0 {
            let mut rng = cube.get_random();
            self.next_randomize_time = 40 + rng.random_range(0..60);
            self.chosen_degrees = rng.random_range(0..360) as f32;
        }
        cube.set_direction(self.chosen_degrees, false);
    }

    fn controls(&self) -> Controls {
        Controls::LOOK
    }
}

// cube mob: AbstractCubeMob.CubeMobKeepOnJumpingGoal
pub struct CubeKeepOnJumpingGoal {
    cube: Weak<SulfurCubeEntity>,
}

impl CubeKeepOnJumpingGoal {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self { cube }
    }
}

impl Goal for CubeKeepOnJumpingGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.cube
            .upgrade()
            .is_some_and(|cube| !cube.has_body_item() && !cube.get_entity().has_vehicle())
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        if let Some(cube) = self.cube.upgrade() {
            cube.set_wanted_movement(KEEP_JUMPING_WANTED_SPEED);
        }
    }

    fn controls(&self) -> Controls {
        Controls::JUMP | Controls::MOVE
    }
}

// sulfur cube: SulfurCubeTemptGoal (TemptGoal.ForNonPathfinders, LOOK only)
pub struct CubeTemptGoal {
    cube: Weak<SulfurCubeEntity>,
    player: Option<Arc<Player>>,
    calm_down: i32,
}

impl CubeTemptGoal {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self {
            cube,
            player: None,
            calm_down: 0,
        }
    }

    fn finds_player(&mut self, cube: &SulfurCubeEntity) -> bool {
        let range = cube
            .get_mob_entity()
            .living_entity
            .get_attribute_value(&Attributes::TEMPT_RANGE);
        let entity = cube.get_entity();
        let world = entity.world.load();
        let conditions = TargetPredicate::create_non_attackable()
            .ignore_visibility()
            .set_base_max_distance(range);
        self.player = world.get_nearest_player(entity.pos.load(), range, |player| {
            let inventory = player.inventory();
            (cube.is_tempt_item(&inventory.held_item())
                || cube.is_tempt_item(&inventory.off_hand_item()))
                && conditions.test(&world, Some(cube), player.as_ref())
        });
        self.player.is_some()
    }
}

impl Goal for CubeTemptGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        let Some(cube) = self.cube.upgrade() else {
            return false;
        };
        if cube.has_body_item() {
            return false;
        }
        if self.calm_down > 0 {
            self.calm_down -= 1;
            return false;
        }
        self.finds_player(&cube)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    // tempt goal: stopNavigation, the cube stands still but keeps its jump rhythm
    fn stop(&mut self, _mob: &dyn Mob) {
        self.player = None;
        if let Some(cube) = self.cube.upgrade() {
            cube.set_wanted_movement(0.0);
        }
        self.calm_down = TEMPT_CALM_DOWN_TICKS;
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let (Some(cube), Some(player)) = (self.cube.upgrade(), self.player.clone()) else {
            return;
        };
        let player_entity = player.get_entity();
        let player_pos = player_entity.pos.load();
        cube.get_mob_entity()
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_with_range(
                player_pos.x,
                player_entity.get_eye_y(),
                player_pos.z,
                mob.get_max_head_rotation() + 20.0,
                mob.get_max_look_pitch_change(),
            );
        let pos = cube.get_entity().pos.load();
        if pos.squared_distance_to_vec(&player_pos) < TEMPT_STOP_DISTANCE * TEMPT_STOP_DISTANCE {
            cube.set_wanted_movement(0.0);
        } else {
            // sulfur cube: navigateTowards looks at the player and jumps towards it
            cube.look_at_entity(player.as_ref(), LOOK_ROTATION_STEP);
            cube.set_direction_to_yaw(true);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::LOOK
    }
}

// sulfur cube: SulfurCubeSearchForItemsGoal
pub struct CubeSearchForItemsGoal {
    cube: Weak<SulfurCubeEntity>,
    target_item: Option<Arc<dyn EntityBase>>,
}

impl CubeSearchForItemsGoal {
    #[must_use]
    pub const fn new(cube: Weak<SulfurCubeEntity>) -> Self {
        Self {
            cube,
            target_item: None,
        }
    }
}

impl Goal for CubeSearchForItemsGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        let Some(cube) = self.cube.upgrade() else {
            return false;
        };
        if cube.has_body_item() || cube.is_baby_cube() || cube.pickup_timer() > 0 {
            return false;
        }
        let entity = cube.get_entity();
        let pos = entity.pos.load();
        let search = entity.bounding_box.load().expand(
            ITEM_SEARCH_RANGE,
            ITEM_SEARCH_RANGE,
            ITEM_SEARCH_RANGE,
        );
        self.target_item = entity
            .world
            .load()
            .get_entities_at_box(&search)
            .into_iter()
            .filter(|candidate| {
                candidate.get_item_entity().is_some_and(|item| {
                    item.get_pickup_delay() == 0
                        && item.get_entity().is_alive()
                        && SulfurCubeEntity::is_swallowable_item(
                            &item
                                .get_item_stack()
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                        )
                })
            })
            .min_by(|a, b| {
                pos.squared_distance_to_vec(&a.get_entity().pos.load())
                    .total_cmp(&pos.squared_distance_to_vec(&b.get_entity().pos.load()))
            });
        self.target_item.is_some()
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let (Some(cube), Some(target)) = (self.cube.upgrade(), self.target_item.clone()) else {
            return;
        };
        cube.look_at_entity(target.as_ref(), LOOK_ROTATION_STEP);
        cube.set_direction_to_yaw(true);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::LOOK
    }
}

impl SulfurCubeEntity {
    // sulfur cube: TemptGoal item predicate (baby eats food, adults swallow blocks)
    pub(super) fn is_tempt_item(&self, stack: &pumpkin_data::item_stack::ItemStack) -> bool {
        if stack.is_empty() {
            return false;
        }
        if self.is_baby_cube() {
            stack
                .item
                .has_tag(&pumpkin_data::tag::Item::MINECRAFT_SULFUR_CUBE_FOOD)
        } else {
            Self::is_swallowable_item(stack)
        }
    }
}
