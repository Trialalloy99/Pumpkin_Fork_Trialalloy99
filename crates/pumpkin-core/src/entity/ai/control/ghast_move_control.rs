use std::sync::atomic::Ordering;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use crate::entity::ai::control::move_control::Operation;
use crate::entity::ai::control::{Control, MoveControlTrait};
use crate::entity::ai::util::goal_utils;
use crate::entity::mob::Mob;
use crate::world::World;

// ghast: Ghast.GhastMoveControl
pub struct GhastMoveControl {
    wanted: Vector3<f64>,
    operation: Operation,
    float_duration: i32,
    careful: bool,
    should_be_stopped: fn(&dyn Mob) -> bool,
}

impl GhastMoveControl {
    #[must_use]
    pub const fn new(careful: bool, should_be_stopped: fn(&dyn Mob) -> bool) -> Self {
        Self {
            wanted: Vector3::new(0.0, 0.0, 0.0),
            operation: Operation::Wait,
            float_duration: 0,
            careful,
            should_be_stopped,
        }
    }

    // ghast: GhastMoveControl.canReach
    fn can_reach(&self, mob: &dyn Mob, travel: Vector3<f64>) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let aabb = entity.bounding_box.load();
        let at_destination = aabb.shift(travel);
        if self.careful {
            let inflated = at_destination.expand_all(1.0);
            let (min, max) = (inflated.min_block_pos(), inflated.max_block_pos());
            for x in min.0.x..=max.0.x {
                for y in min.0.y..=max.0.y {
                    for z in min.0.z..=max.0.z {
                        if !self.block_traversal_possible(&world, &BlockPos::new(x, y, z)) {
                            return false;
                        }
                    }
                }
            }
        }
        // ghast: forEachBlockIntersectedBetween, sampled along the travel
        let length = travel.length();
        let steps = length.ceil().max(1.0);
        let step = travel * (1.0 / steps);
        let mut moved = aabb;
        for _ in 0..steps as i32 {
            moved = moved.shift(step);
            let (min, max) = (moved.min_block_pos(), moved.max_block_pos());
            for x in min.0.x..=max.0.x {
                for y in min.0.y..=max.0.y {
                    for z in min.0.z..=max.0.z {
                        let pos = BlockPos::new(x, y, z);
                        if aabb.intersects(&BoundingBox::from_block(&pos)) {
                            continue;
                        }
                        if !world.get_block_state(&pos).collision_shapes.is_empty() {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    // ghast: GhastMoveControl.blockTraversalPossible, imprecise collisions
    fn block_traversal_possible(&self, world: &World, pos: &BlockPos) -> bool {
        let state = world.get_block_state(pos);
        if state.is_air() {
            return true;
        }
        let no_collision = state.collision_shapes.is_empty();
        if !self.careful {
            return no_collision;
        }
        if world
            .get_block(pos)
            .has_tag(&tag::Block::MINECRAFT_HAPPY_GHAST_AVOIDS)
        {
            return false;
        }
        if goal_utils::is_water(world, pos) || state.is_liquid() {
            return false;
        }
        no_collision
    }
}

impl Control for GhastMoveControl {}

impl MoveControlTrait for GhastMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if (self.should_be_stopped)(mob) {
            self.operation = Operation::Wait;
            mob.get_mob_entity().stop_in_place();
        }
        if self.operation != Operation::MoveTo {
            return;
        }
        let ready = self.float_duration <= 0;
        self.float_duration -= 1;
        if !ready {
            return;
        }
        self.float_duration += mob.get_random().random_range(0..5) + 2;
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        let travel = Vector3::new(
            self.wanted.x - pos.x,
            self.wanted.y - pos.y,
            self.wanted.z - pos.z,
        );
        if self.can_reach(mob, travel) {
            let flying_speed = mob
                .get_mob_entity()
                .living_entity
                .get_attribute_value(&Attributes::FLYING_SPEED);
            let delta = travel.normalize() * (flying_speed * 5.0 / 3.0);
            entity.velocity.store(entity.velocity.load() + delta);
            entity.velocity_dirty.store(true, Ordering::Relaxed);
        } else {
            self.operation = Operation::Wait;
        }
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, _speed_modifier: f64) {
        self.wanted = Vector3::new(x, y, z);
        self.operation = Operation::MoveTo;
    }

    fn has_wanted(&self) -> bool {
        self.operation == Operation::MoveTo
    }

    fn wanted_position(&self) -> Option<Vector3<f64>> {
        self.has_wanted().then_some(self.wanted)
    }

    fn set_wait(&mut self) {
        self.operation = Operation::Wait;
    }
}
