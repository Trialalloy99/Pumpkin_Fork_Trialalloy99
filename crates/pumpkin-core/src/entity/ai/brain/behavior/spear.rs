use std::sync::Arc;

use pumpkin_data::data_component_impl::{EquipmentSlot, KineticConditionImpl, KineticWeaponImpl};
use pumpkin_util::Hand;
use pumpkin_util::math::vector3::Vector3;

use super::super::BrainTick;
use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::value::SpearStatus;
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::timed::Behavior;
use crate::entity::EntityBase;
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::ai::util::land_random_pos;
use crate::entity::mob::Mob;
use crate::item::items::spear::SpearItem;

const MIN_REPOSITION_DISTANCE: f64 = 6.0;
const MAX_REPOSITION_DISTANCE: f64 = 7.0;
const MIN_COOLDOWN_DISTANCE: f64 = 9.0;
const MAX_COOLDOWN_DISTANCE: f64 = 11.0;
const MAX_FLEEING_TIME: i32 = 100;
const AWAY_VERTICAL_DIST: i32 = 7;
const NEVER_TIMES_OUT: i32 = i32::MAX - 1;

fn target_of(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
    tick.state.get(types::ATTACK_TARGET).cloned()
}

fn able_to_attack(tick: &BrainTick<'_>) -> bool {
    target_of(tick).is_some()
        && tick
            .mob
            .get_mob_entity()
            .with_main_hand(|stack| stack.get_data_component::<KineticWeaponImpl>().is_some())
}

fn is_using_item(mob: &dyn Mob) -> bool {
    mob.get_mob_entity()
        .living_entity
        .item_in_use
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

fn charge_speed_modifier(mob: &dyn Mob) -> f64 {
    f64::from(
        mob.get_entity()
            .get_root_vehicle()
            .and_then(|vehicle| vehicle.get_mob().map(Mob::charge_speed_modifier))
            .unwrap_or(1.0),
    )
}

fn move_to(mob: &dyn Mob, destination: Vector3<f64>, speed: f64) {
    mob.get_mob_entity()
        .steered()
        .navigator()
        .set_progress(NavigatorGoal::new(
            mob.get_entity().pos.load(),
            destination,
            speed,
        ));
}

// brain: SpearApproach
pub struct SpearApproach {
    speed_modifier_when_repositioning: f64,
    approach_distance_sq: f64,
}

impl SpearApproach {
    #[must_use]
    pub fn new(speed_modifier_when_repositioning: f64, approach_distance: f32) -> Self {
        Self {
            speed_modifier_when_repositioning,
            approach_distance_sq: f64::from(approach_distance * approach_distance),
        }
    }
}

const APPROACH_ENTRY: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::SPEAR_STATUS.id(), MemoryStatus::ValueAbsent)];

impl Behavior for SpearApproach {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &APPROACH_ENTRY
    }

    fn min_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn max_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        able_to_attack(tick) && !is_using_item(tick.mob)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob.get_mob_entity().set_attacking(true);
        tick.state.set(types::SPEAR_STATUS, SpearStatus::Approach);
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(target) = target_of(tick) else {
            return false;
        };
        able_to_attack(tick)
            && tick
                .mob
                .get_entity()
                .pos
                .load()
                .squared_distance_to_vec(&target.get_entity().pos.load())
                > self.approach_distance_sq
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = target_of(tick) else {
            return;
        };
        let speed = charge_speed_modifier(tick.mob) * self.speed_modifier_when_repositioning;
        let target_pos = target.get_entity().pos.load();
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(target, true)),
        );
        move_to(tick.mob, target_pos, speed);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob.get_mob_entity().steered().navigator().stop();
        tick.state.set(types::SPEAR_STATUS, SpearStatus::Charging);
    }

    fn debug_name(&self) -> &'static str {
        "SpearApproach"
    }
}

// brain: SpearAttack
pub struct SpearAttack {
    speed_modifier_when_charging: f64,
    speed_modifier_when_repositioning: f64,
    target_in_range_radius_sq: f64,
}

impl SpearAttack {
    #[must_use]
    pub fn new(
        speed_modifier_when_charging: f64,
        speed_modifier_when_repositioning: f64,
        target_in_range_radius: f32,
    ) -> Self {
        Self {
            speed_modifier_when_charging,
            speed_modifier_when_repositioning,
            target_in_range_radius_sq: f64::from(target_in_range_radius * target_in_range_radius),
        }
    }
}

const ATTACK_ENTRY: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::SPEAR_STATUS.id(), MemoryStatus::ValuePresent)];

impl Behavior for SpearAttack {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ATTACK_ENTRY
    }

    fn min_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn max_duration(&self) -> i32 {
        NEVER_TIMES_OUT
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.get(types::SPEAR_STATUS).copied() == Some(SpearStatus::Charging)
            && able_to_attack(tick)
            && !is_using_item(tick.mob)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let mob_entity = tick.mob.get_mob_entity();
        mob_entity.set_attacking(true);
        let duration = mob_entity.with_main_hand(|stack| {
            stack
                .get_data_component::<KineticWeaponImpl>()
                .map_or(0, |weapon| {
                    weapon.delay_ticks
                        + weapon
                            .damage_conditions
                            .as_ref()
                            .map_or(0, KineticConditionImpl::max_duration_ticks)
                })
        });
        tick.state.set(types::SPEAR_ENGAGE_TIME, duration);
        tick.state.erase(types::SPEAR_CHARGE_POSITION.id());
        let stack = mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::MAIN_HAND);
        if !stack.is_empty() {
            mob_entity
                .living_entity
                .set_active_hand(Hand::Right, stack, SpearItem::USE_DURATION);
        }
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state
            .get(types::SPEAR_ENGAGE_TIME)
            .copied()
            .unwrap_or(0)
            > 0
            && able_to_attack(tick)
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = target_of(tick) else {
            return;
        };
        let mob = tick.mob;
        let entity = mob.get_entity();
        let target_pos = target.get_entity().pos.load();
        let target_dist_sq = entity.pos.load().squared_distance_to_vec(&target_pos);
        let speed = charge_speed_modifier(mob);
        let mount_distance = if entity.has_vehicle() { 2.0 } else { 0.0 };
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(target, true)),
        );
        let remaining = tick
            .state
            .get(types::SPEAR_ENGAGE_TIME)
            .copied()
            .unwrap_or(0);
        tick.state.set(types::SPEAR_ENGAGE_TIME, remaining - 1);
        if let Some(away_pos) = tick.state.get(types::SPEAR_CHARGE_POSITION).copied() {
            move_to(
                mob,
                away_pos,
                speed * self.speed_modifier_when_repositioning,
            );
            if mob.is_navigator_idle() {
                tick.state.erase(types::SPEAR_CHARGE_POSITION.id());
            }
        } else {
            move_to(mob, target_pos, speed * self.speed_modifier_when_charging);
            if target_dist_sq < self.target_in_range_radius_sq || mob.is_navigator_idle() {
                let distance = target_dist_sq.sqrt();
                let away = land_random_pos::get_pos_away(
                    mob,
                    MIN_REPOSITION_DISTANCE + mount_distance - distance,
                    MAX_REPOSITION_DISTANCE + mount_distance - distance,
                    AWAY_VERTICAL_DIST,
                    target_pos,
                );
                tick.state.set_optional(types::SPEAR_CHARGE_POSITION, away);
            }
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let mob_entity = tick.mob.get_mob_entity();
        mob_entity.steered().navigator().stop();
        mob_entity.living_entity.clear_active_hand();
        tick.state.erase(types::SPEAR_CHARGE_POSITION.id());
        tick.state.erase(types::SPEAR_ENGAGE_TIME.id());
        tick.state.set(types::SPEAR_STATUS, SpearStatus::Retreat);
    }

    fn debug_name(&self) -> &'static str {
        "SpearAttack"
    }
}

// brain: SpearRetreat
pub struct SpearRetreat {
    speed_modifier_when_repositioning: f64,
}

impl SpearRetreat {
    #[must_use]
    pub const fn new(speed_modifier_when_repositioning: f64) -> Self {
        Self {
            speed_modifier_when_repositioning,
        }
    }
}

impl Behavior for SpearRetreat {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ATTACK_ENTRY
    }

    fn min_duration(&self) -> i32 {
        MAX_FLEEING_TIME
    }

    fn max_duration(&self) -> i32 {
        MAX_FLEEING_TIME
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if !able_to_attack(tick) || is_using_item(tick.mob) {
            return false;
        }
        if tick.state.get(types::SPEAR_STATUS).copied() != Some(SpearStatus::Retreat) {
            return false;
        }
        let Some(target) = target_of(tick) else {
            return false;
        };
        let entity = tick.mob.get_entity();
        let target_pos = target.get_entity().pos.load();
        let distance = entity
            .pos
            .load()
            .squared_distance_to_vec(&target_pos)
            .sqrt();
        let mount_distance = if entity.has_vehicle() { 2.0 } else { 0.0 };
        let Some(away) = land_random_pos::get_pos_away(
            tick.mob,
            (MIN_COOLDOWN_DISTANCE + mount_distance - distance).max(0.0),
            (MAX_COOLDOWN_DISTANCE + mount_distance - distance).max(1.0),
            AWAY_VERTICAL_DIST,
            target_pos,
        ) else {
            return false;
        };
        tick.state.set(types::SPEAR_FLEEING_POSITION, away);
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob.get_mob_entity().set_attacking(true);
        tick.state.set(types::SPEAR_FLEEING_TIME, 0);
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state
            .get(types::SPEAR_FLEEING_TIME)
            .copied()
            .unwrap_or(MAX_FLEEING_TIME)
            < MAX_FLEEING_TIME
            && tick
                .state
                .has_memory_value(types::SPEAR_FLEEING_POSITION.id())
            && !tick.mob.is_navigator_idle()
            && able_to_attack(tick)
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = target_of(tick) else {
            return;
        };
        let speed = charge_speed_modifier(tick.mob) * self.speed_modifier_when_repositioning;
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(target, true)),
        );
        let time = tick
            .state
            .get(types::SPEAR_FLEEING_TIME)
            .copied()
            .unwrap_or(0);
        tick.state.set(types::SPEAR_FLEEING_TIME, time + 1);
        if let Some(flee) = tick.state.get(types::SPEAR_FLEEING_POSITION).copied() {
            move_to(tick.mob, flee, speed);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let mob_entity = tick.mob.get_mob_entity();
        mob_entity.steered().navigator().stop();
        mob_entity.set_attacking(false);
        mob_entity.living_entity.clear_active_hand();
        tick.state.erase(types::SPEAR_FLEEING_TIME.id());
        tick.state.erase(types::SPEAR_FLEEING_POSITION.id());
        tick.state.erase(types::SPEAR_STATUS.id());
    }

    fn debug_name(&self) -> &'static str {
        "SpearRetreat"
    }
}
