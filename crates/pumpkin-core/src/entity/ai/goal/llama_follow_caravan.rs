use std::sync::Arc;

use super::{Controls, Goal, to_goal_ticks};
use crate::entity::EntityBase;
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::ai::target_match::{SearchArea, TargetMatch};
use crate::entity::mob::Mob;
use pumpkin_data::entity::EntityType;

const SEARCH_RADIUS: f64 = 9.0;
const SEARCH_Y_RANGE: f64 = 4.0;
const MIN_DISTANCE_SQ: f64 = 4.0;
const MAX_DISTANCE_SQ: f64 = 676.0;

// llama: caravan following behavior (LlamaFollowCaravanGoal)
pub struct LlamaFollowCaravanGoal {
    speed_modifier: f64,
    leader: Option<Arc<dyn EntityBase>>,
    delay: i32,
}

impl LlamaFollowCaravanGoal {
    #[must_use]
    pub fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            leader: None,
            delay: 0,
        }
    }

    fn is_llama(candidate: &dyn EntityBase) -> bool {
        let t = candidate.get_entity().entity_type;
        t == &EntityType::LLAMA || t == &EntityType::TRADER_LLAMA
    }
}

impl Goal for LlamaFollowCaravanGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let entity = &mob.get_mob_entity().living_entity.entity;
        if entity.leashed_to.lock().is_ok_and(|guard| guard.is_some()) {
            return false;
        }

        let world = entity.world.load();
        let area = SearchArea::from_feet(entity, SEARCH_RADIUS, SEARCH_Y_RANGE);

        let leader = TargetMatch::Any.find_nearest(&world, &area, |candidate| {
            if candidate.get_entity().entity_id == entity.entity_id {
                return false;
            }
            Self::is_llama(candidate)
        });

        if let Some(ref l) = leader {
            let dist_sq = entity
                .pos
                .load()
                .squared_distance_to_vec(&l.get_entity().pos.load());
            if dist_sq >= MIN_DISTANCE_SQ {
                self.leader = leader;
                return true;
            }
        }
        false
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(leader) = &self.leader else {
            return false;
        };
        if !leader.get_entity().is_alive() {
            return false;
        }
        let mob_pos = mob.get_mob_entity().living_entity.entity.pos.load();
        let leader_pos = leader.get_entity().pos.load();
        let dist_sq = mob_pos.squared_distance_to_vec(&leader_pos);
        (MIN_DISTANCE_SQ..=MAX_DISTANCE_SQ).contains(&dist_sq)
    }

    fn start(&mut self, _mob: &dyn Mob) {
        self.delay = 0;
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.delay -= 1;
        if self.delay > 0 {
            return;
        }
        self.delay = to_goal_ticks(10);
        if let Some(leader) = &self.leader {
            let mob_pos = mob.get_mob_entity().living_entity.entity.pos.load();
            let leader_pos = leader.get_entity().pos.load();
            let steered = mob.get_mob_entity().steered();
            let mut navigator = steered.navigator();
            navigator.set_progress(NavigatorGoal::new(mob_pos, leader_pos, self.speed_modifier));
        }
    }

    fn stop(&mut self, _mob: &dyn Mob) {
        self.leader = None;
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}
