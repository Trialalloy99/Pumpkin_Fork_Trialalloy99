use super::{Controls, Goal};
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::mob::Mob;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

// flee sun: burning by day under open sky -> walk to a covered spot (FleeSunGoal)
pub struct FleeSunGoal {
    goal_control: Controls,
    speed_modifier: f64,
    wanted: Vector3<f64>,
}

impl FleeSunGoal {
    #[must_use]
    pub const fn new(speed_modifier: f64) -> Self {
        Self {
            goal_control: Controls::MOVE,
            speed_modifier,
            wanted: Vector3::new(0.0, 0.0, 0.0),
        }
    }

    // flee sun: 10 tries, no sky and walk value < 0 (FleeSunGoal.getHidePos)
    fn find_hide_pos(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let mob_pos = mob.get_entity().block_pos.load();
        let world = mob.get_entity().world.load();
        let mut rng = mob.get_random();
        for _ in 0..10 {
            let candidate = BlockPos::new(
                mob_pos.0.x + rng.random_range(0..20) - 10,
                mob_pos.0.y + rng.random_range(0..6) - 3,
                mob_pos.0.z + rng.random_range(0..20) - 10,
            );
            if !world.can_see_sky(&candidate) && mob.get_walk_target_value(&candidate) < 0.0 {
                return Some(Vector3::new(
                    f64::from(candidate.0.x) + 0.5,
                    f64::from(candidate.0.y),
                    f64::from(candidate.0.z) + 0.5,
                ));
            }
        }
        None
    }
}

impl Goal for FleeSunGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let mob_entity = mob.get_mob_entity();
        let entity = &mob_entity.living_entity.entity;
        let world = entity.world.load();
        if mob_entity.get_target().is_some()
            || !world.is_bright_outside()
            || !entity.is_on_fire()
            || !world.can_see_sky(&entity.block_pos.load())
            || !mob_entity
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&EquipmentSlot::HEAD)
                .is_empty()
        {
            return false;
        }
        let Some(pos) = Self::find_hide_pos(mob) else {
            return false;
        };
        self.wanted = pos;
        true
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        !mob.is_navigator_idle()
    }

    fn start(&mut self, mob: &dyn Mob) {
        let mob_pos = mob.get_entity().pos.load();
        mob.get_mob_entity()
            .steered()
            .navigator()
            .set_progress(NavigatorGoal::new(
                mob_pos,
                self.wanted,
                self.speed_modifier,
            ));
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}
