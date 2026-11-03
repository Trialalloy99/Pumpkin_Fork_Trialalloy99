use std::sync::Arc;

use super::{Controls, Goal, to_goal_ticks};
use crate::entity::EntityBase;
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::mob::Mob;
use crate::entity::passive::llama::LlamaEntity;
use pumpkin_data::entity::EntityType;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::Vector3;

const SEARCH_RADIUS: f64 = 9.0;
const SEARCH_Y_RANGE: f64 = 4.0;
const MIN_DISTANCE_SQ: f64 = 4.0;
const MAX_DISTANCE_SQ: f64 = 676.0;
const CARAVAN_LIMIT: i32 = 8;
const DEFAULT_SPEED: f64 = 2.1;
const MAX_CATCH_UP_SPEED: f64 = 3.0;
const WANTED_DISTANCE: f64 = 2.0;

/// Vanilla `LlamaFollowCaravanGoal`: llamas follow a leashed llama or the end of its caravan.
pub struct LlamaFollowCaravanGoal {
    speed_modifier: f64,
    dist_check_counter: i32,
}

fn llama_of(mob: &dyn Mob) -> Option<&LlamaEntity> {
    mob.cast_any().downcast_ref::<LlamaEntity>()
}

fn llama_ref(entity: &Arc<dyn EntityBase>) -> Option<&LlamaEntity> {
    entity.cast_any().downcast_ref::<LlamaEntity>()
}

impl LlamaFollowCaravanGoal {
    #[must_use]
    pub const fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            dist_check_counter: 0,
        }
    }

    // goal: firstIsLeashed
    fn first_is_leashed(llama: &LlamaEntity, mut counter: i32) -> bool {
        if counter > CARAVAN_LIMIT {
            return false;
        }
        let Some(head) = llama.caravan_head() else {
            return false;
        };
        let Some(head_llama) = llama_ref(&head) else {
            return false;
        };
        if head_llama.is_leashed() {
            return true;
        }
        counter += 1;
        Self::first_is_leashed(head_llama, counter)
    }
}

impl Goal for LlamaFollowCaravanGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(llama) = llama_of(mob) else {
            return false;
        };
        if llama.is_leashed() || llama.in_caravan() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load();
        let bounds = entity.bounding_box.load();
        let area = BoundingBox::new(
            Vector3::new(
                bounds.min.x - SEARCH_RADIUS,
                bounds.min.y - SEARCH_Y_RANGE,
                bounds.min.z - SEARCH_RADIUS,
            ),
            Vector3::new(
                bounds.max.x + SEARCH_RADIUS,
                bounds.max.y + SEARCH_Y_RANGE,
                bounds.max.z + SEARCH_RADIUS,
            ),
        );
        let own_pos = entity.pos.load();
        let llamas: Vec<Arc<dyn EntityBase>> = world
            .get_all_at_box(&area)
            .into_iter()
            .filter(|other| {
                other.get_entity().entity_id != entity.entity_id
                    && (other.get_entity().entity_type == &EntityType::LLAMA
                        || other.get_entity().entity_type == &EntityType::TRADER_LLAMA)
            })
            .collect();
        let nearest = |accept: &dyn Fn(&LlamaEntity) -> bool| {
            let mut closest: Option<(Arc<dyn EntityBase>, f64)> = None;
            for candidate in &llamas {
                let Some(candidate_llama) = llama_ref(candidate) else {
                    continue;
                };
                if !accept(candidate_llama) {
                    continue;
                }
                let dist = candidate
                    .get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&own_pos);
                // vanilla keeps the later one on a tie
                if closest.as_ref().is_none_or(|(_, best)| dist <= *best) {
                    closest = Some((candidate.clone(), dist));
                }
            }
            closest
        };
        let mut closest =
            nearest(&|candidate| candidate.in_caravan() && !candidate.has_caravan_tail());
        if closest.is_none() {
            closest = nearest(&|candidate| candidate.is_leashed() && !candidate.has_caravan_tail());
        }
        let Some((head, dist_sq)) = closest else {
            return false;
        };
        if dist_sq < MIN_DISTANCE_SQ {
            return false;
        }
        let Some(head_llama) = llama_ref(&head) else {
            return false;
        };
        if !head_llama.is_leashed() && !Self::first_is_leashed(head_llama, 1) {
            return false;
        }
        llama.join_caravan(&head);
        true
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(llama) = llama_of(mob) else {
            return false;
        };
        let Some(head) = llama.caravan_head() else {
            return false;
        };
        if !head.get_entity().is_alive() || !Self::first_is_leashed(llama, 0) {
            return false;
        }
        let dist_sq = mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&head.get_entity().pos.load());
        if dist_sq > MAX_DISTANCE_SQ {
            if self.speed_modifier <= MAX_CATCH_UP_SPEED {
                self.speed_modifier *= 1.2;
                self.dist_check_counter = to_goal_ticks(40);
                return true;
            }
            if self.dist_check_counter == 0 {
                return false;
            }
        }
        if self.dist_check_counter > 0 {
            self.dist_check_counter -= 1;
        }
        true
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(llama) = llama_of(mob) {
            llama.leave_caravan();
        }
        self.speed_modifier = DEFAULT_SPEED;
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(llama) = llama_of(mob) else {
            return;
        };
        let Some(head) = llama.caravan_head() else {
            return;
        };
        let entity = mob.get_entity();
        let on_fence_knot = entity
            .leashed_to
            .lock()
            .ok()
            .and_then(|holder| holder.clone())
            .is_some_and(|holder| holder.get_entity().entity_type == &EntityType::LEASH_KNOT);
        if on_fence_knot {
            return;
        }
        let pos = entity.pos.load();
        let head_pos = head.get_entity().pos.load();
        let distance_to = pos.squared_distance_to_vec(&head_pos).sqrt();
        let delta = (head_pos - pos).normalize() * (distance_to - WANTED_DISTANCE).max(0.0);
        let target = pos + delta;
        mob.get_mob_entity()
            .steered()
            .navigator()
            .set_progress(NavigatorGoal::new(pos, target, self.speed_modifier));
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}
