use std::sync::Arc;

use pumpkin_data::entity::EntityType;
use rand::RngExt;

use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::walk_target::WalkTarget;
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::super::{BrainState, BrainTick};
use super::timed::Behavior;
use super::utils::entity_is_visible;
use crate::entity::EntityBase;
use crate::entity::ai::goal::breed::BreedGoal;

const BREED_RANGE: f64 = 3.0;
const MIN_DURATION: i32 = 60;
const MAX_DURATION: i32 = 110;
const BREED_JITTER: i32 = 50;

const ENTRY_CONDITIONS: [(MemoryModuleId, MemoryStatus); 5] = [
    (
        types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
        MemoryStatus::ValuePresent,
    ),
    (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::WALK_TARGET.id(), MemoryStatus::Registered),
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::IS_PANICKING.id(), MemoryStatus::ValueAbsent),
];

// brain: AnimalMakeLove
pub struct AnimalMakeLove {
    partner_type: &'static EntityType,
    speed_modifier: f32,
    close_enough_distance: i32,
    spawn_child_at_time: i64,
    on_start: fn(&mut BrainTick<'_>),
}

impl AnimalMakeLove {
    #[must_use]
    pub const fn new(partner_type: &'static EntityType, on_start: fn(&mut BrainTick<'_>)) -> Self {
        Self {
            partner_type,
            speed_modifier: 1.0,
            close_enough_distance: 2,
            spawn_child_at_time: 0,
            on_start,
        }
    }

    /// Vanilla `AnimalMakeLove(type, speed, closeEnoughDistance)`.
    #[must_use]
    pub const fn with_close_enough_distance(mut self, distance: i32) -> Self {
        self.close_enough_distance = distance;
        self
    }

    fn is_panicking(entity: &Arc<dyn EntityBase>) -> bool {
        entity.get_mob().is_some_and(|mob| {
            mob.get_mob_entity()
                .try_with_brain(mob, |brain| {
                    brain.has_memory_value(types::IS_PANICKING.id())
                })
                .unwrap_or(false)
        })
    }

    // brain: AnimalMakeLove.findValidBreedPartner
    fn find_valid_partner(&self, tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        let visible = tick.state.get(types::NEAREST_VISIBLE_LIVING_ENTITIES)?;
        visible.find_closest(&tick.visibility(), |entity| {
            entity.get_entity().entity_type == self.partner_type
                && entity.get_mob().is_some_and(|mob| {
                    mob.as_animal().is_some() && tick.mob.can_mate(entity.as_ref())
                })
                && !Self::is_panicking(entity)
        })
    }

    // brain: BehaviorUtils.lockGazeAndWalkToEachOther
    fn lock_gaze_and_walk(&self, tick: &mut BrainTick<'_>, partner: &Arc<dyn EntityBase>) {
        let Some(body) = tick.world.get_entity_by_id(tick.mob.get_entity().entity_id) else {
            return;
        };
        let (speed, close) = (self.speed_modifier, self.close_enough_distance);
        let apply = |state: &mut BrainState, other: &Arc<dyn EntityBase>| {
            state.set(
                types::LOOK_TARGET,
                Arc::new(EntityTracker::new(other.clone(), true)),
            );
            state.set(
                types::WALK_TARGET,
                WalkTarget::new(
                    Arc::new(EntityTracker::new(other.clone(), false)),
                    speed,
                    close,
                ),
            );
        };
        apply(tick.state, partner);
        if let Some(mob) = partner.get_mob() {
            mob.get_mob_entity()
                .try_with_brain(mob, |brain| apply(brain, &body));
        }
    }

    fn breed_target(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        tick.state.get(types::BREED_TARGET).cloned()
    }
}

impl Behavior for AnimalMakeLove {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ENTRY_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        MAX_DURATION
    }

    fn max_duration(&self) -> i32 {
        MAX_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.mob.get_mob_entity().is_in_love() && self.find_valid_partner(tick).is_some()
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        (self.on_start)(tick);
        let Some(partner) = self.find_valid_partner(tick) else {
            return;
        };
        if let Some(body) = tick.world.get_entity_by_id(tick.mob.get_entity().entity_id)
            && let Some(mob) = partner.get_mob()
        {
            mob.get_mob_entity()
                .try_with_brain(mob, |brain| brain.set(types::BREED_TARGET, body));
        }
        tick.state.set(types::BREED_TARGET, partner.clone());
        self.lock_gaze_and_walk(tick, &partner);
        let jitter = tick.mob.get_random().random_range(0..BREED_JITTER);
        self.spawn_child_at_time = tick.time + i64::from(MIN_DURATION + jitter);
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(partner) = Self::breed_target(tick) else {
            return false;
        };
        partner.get_entity().entity_type == self.partner_type
            && partner.get_entity().is_alive()
            && tick.mob.can_mate(partner.as_ref())
            && entity_is_visible(&tick.visibility(), partner.as_ref())
            && tick.time <= self.spawn_child_at_time
            && !Self::is_panicking(&partner)
            && !tick.state.has_memory_value(types::IS_PANICKING.id())
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(partner) = Self::breed_target(tick) else {
            return;
        };
        self.lock_gaze_and_walk(tick, &partner);
        let distance_sq = tick
            .mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&partner.get_entity().pos.load());
        if distance_sq < BREED_RANGE * BREED_RANGE && tick.time >= self.spawn_child_at_time {
            BreedGoal::breed(tick.mob, partner.as_ref());
            tick.state.erase(types::BREED_TARGET.id());
            if let Some(mob) = partner.get_mob() {
                mob.get_mob_entity()
                    .try_with_brain(mob, |brain| brain.erase(types::BREED_TARGET.id()));
            }
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::BREED_TARGET.id());
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::LOOK_TARGET.id());
        self.spawn_child_at_time = 0;
    }

    fn debug_name(&self) -> &'static str {
        "AnimalMakeLove"
    }
}
