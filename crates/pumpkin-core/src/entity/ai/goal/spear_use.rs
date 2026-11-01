use pumpkin_data::data_component_impl::KineticWeaponImpl;
use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::{Controls, Goal};
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

// spear use: approach, charge with lowered spear, back off (SpearUseGoal.SpearUseState)
struct SpearUseState {
    engage_time: i32,
    fleeing_time: i32,
    away_pos: Option<Vector3<f64>>,
    done: bool,
}

impl SpearUseState {
    const fn new() -> Self {
        Self {
            engage_time: -1,
            fleeing_time: -1,
            away_pos: None,
            done: false,
        }
    }

    const fn tick_and_check_engagement(&mut self) -> bool {
        if self.engage_time > 0 {
            self.engage_time -= 1;
            return self.engage_time == 0;
        }
        false
    }

    const fn tick_and_check_fleeing(&mut self, max_fleeing_time: i32) -> bool {
        if self.fleeing_time > 0 {
            self.fleeing_time += 1;
            if self.fleeing_time > max_fleeing_time {
                self.done = true;
                return true;
            }
        }
        false
    }
}

// spear use goal: zombie family spear charge (SpearUseGoal)
pub struct SpearUseGoal {
    state: Option<SpearUseState>,
    speed_when_charging: f64,
    speed_when_repositioning: f64,
    approach_distance_sq: f64,
    target_in_range_radius_sq: f64,
    last_destination: Option<BlockPos>,
}

impl SpearUseGoal {
    #[must_use]
    pub fn new(
        speed_when_charging: f64,
        speed_when_repositioning: f64,
        approach_distance: f32,
        target_in_range_radius: f32,
    ) -> Box<Self> {
        Box::new(Self {
            state: None,
            speed_when_charging,
            speed_when_repositioning,
            approach_distance_sq: f64::from(approach_distance * approach_distance),
            target_in_range_radius_sq: f64::from(target_in_range_radius * target_in_range_radius),
            last_destination: None,
        })
    }

    fn able_to_attack(mob: &dyn Mob) -> bool {
        let mob_entity = mob.get_mob_entity();
        mob_entity.get_target().is_some()
            && mob_entity
                .with_main_hand(|stack| stack.get_data_component::<KineticWeaponImpl>().is_some())
    }

    fn kinetic_weapon_use_duration(&self, mob: &dyn Mob) -> i32 {
        let duration = mob.get_mob_entity().with_main_hand(|stack| {
            stack
                .get_data_component::<KineticWeaponImpl>()
                .map_or(0, |weapon| {
                    weapon.delay_ticks
                        + weapon
                            .damage_conditions
                            .as_ref()
                            .map_or(0, |condition| condition.max_duration_ticks())
                })
        });
        self.get_tick_count(duration)
    }

    fn is_using_item(mob: &dyn Mob) -> bool {
        mob.get_mob_entity()
            .living_entity
            .item_in_use
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    // spear use: repath only when the target block changes (PathNavigation.createPath cache)
    fn move_to(
        last_destination: &mut Option<BlockPos>,
        mob: &dyn Mob,
        destination: Vector3<f64>,
        speed: f64,
    ) {
        let block = destination.to_block_pos();
        if *last_destination == Some(block) {
            mob.get_mob_entity().steered().navigator().set_speed(speed);
            return;
        }
        *last_destination = Some(block);
        mob.get_mob_entity()
            .steered()
            .navigator()
            .set_progress(NavigatorGoal::new(
                mob.get_entity().pos.load(),
                destination,
                speed,
            ));
    }
}

impl Goal for SpearUseGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        Self::able_to_attack(mob) && !Self::is_using_item(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.state.as_ref().is_some_and(|state| !state.done) && Self::able_to_attack(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().set_attacking(true);
        self.state = Some(SpearUseState::new());
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        mob_entity.steered().navigator().stop();
        mob_entity.set_attacking(false);
        self.state = None;
        self.last_destination = None;
        mob_entity.living_entity.clear_active_hand();
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let max_fleeing_time = self.get_tick_count(MAX_FLEEING_TIME);
        let use_duration = self.kinetic_weapon_use_duration(mob);
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let mob_entity = mob.get_mob_entity();
        let Some(target) = mob_entity.get_target() else {
            return;
        };
        let entity = mob.get_entity();
        let target_pos = target.get_entity().pos.load();
        let target_dist_sq = entity.pos.load().squared_distance_to_vec(&target_pos);
        let speed_modifier = f64::from(
            entity
                .get_root_vehicle()
                .and_then(|vehicle| vehicle.get_mob().map(Mob::charge_speed_modifier))
                .unwrap_or(1.0),
        );
        let mount_distance = if entity.has_vehicle() { 2.0 } else { 0.0 };
        mob_entity.look_at_entity_body(target.as_ref(), 30.0, 30.0);
        mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_entity_with_range(&target, 30.0, 30.0);

        if state.engage_time < 0 {
            if target_dist_sq > self.approach_distance_sq {
                Self::move_to(
                    &mut self.last_destination,
                    mob,
                    target_pos,
                    speed_modifier * self.speed_when_repositioning,
                );
                return;
            }
            state.engage_time = use_duration;
            let stack = mob_entity
                .living_entity
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&pumpkin_data::data_component_impl::EquipmentSlot::MAIN_HAND);
            if !stack.is_empty() && !Self::is_using_item(mob) {
                mob_entity.living_entity.set_active_hand(
                    Hand::Right,
                    stack,
                    SpearItem::USE_DURATION,
                );
            }
        }

        if state.tick_and_check_engagement() {
            mob_entity.living_entity.clear_active_hand();
            let distance = target_dist_sq.sqrt();
            state.away_pos = land_random_pos::get_pos_away(
                mob,
                (MIN_COOLDOWN_DISTANCE + mount_distance - distance).max(0.0),
                (MAX_COOLDOWN_DISTANCE + mount_distance - distance).max(1.0),
                AWAY_VERTICAL_DIST,
                target_pos,
            );
            state.fleeing_time = 1;
        }

        if state.tick_and_check_fleeing(max_fleeing_time) {
            return;
        }
        if let Some(away_pos) = state.away_pos {
            Self::move_to(
                &mut self.last_destination,
                mob,
                away_pos,
                speed_modifier * self.speed_when_repositioning,
            );
            if mob.is_navigator_idle() {
                if state.fleeing_time > 0 {
                    state.done = true;
                    return;
                }
                state.away_pos = None;
            }
        } else {
            Self::move_to(
                &mut self.last_destination,
                mob,
                target_pos,
                speed_modifier * self.speed_when_charging,
            );
            if target_dist_sq < self.target_in_range_radius_sq || mob.is_navigator_idle() {
                let distance = target_dist_sq.sqrt();
                state.away_pos = land_random_pos::get_pos_away(
                    mob,
                    MIN_REPOSITION_DISTANCE + mount_distance - distance,
                    MAX_REPOSITION_DISTANCE + mount_distance - distance,
                    AWAY_VERTICAL_DIST,
                    target_pos,
                );
            }
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}
