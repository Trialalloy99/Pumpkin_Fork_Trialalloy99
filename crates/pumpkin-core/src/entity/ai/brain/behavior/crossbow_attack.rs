use std::sync::Arc;

use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_util::Hand;
use rand::RngExt;

use super::super::BrainTick;
use super::super::memory::position_tracker::EntityTracker;
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::timed::Behavior;
use super::utils::entity_is_visible;
use crate::entity::EntityBase;
use crate::entity::ai::goal::ranged_crossbow_attack::RangedCrossbowAttackGoal;

const TIMEOUT: i32 = 1200;
const CROSSBOW_RANGE: f64 = 8.0;
const CHARGE_DURATION: i32 = 25;

const ENTRY_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CrossbowState {
    Uncharged,
    Charging,
    Charged,
    ReadyToAttack,
}

// brain: CrossbowAttack
pub struct CrossbowAttack {
    attack_delay: i32,
    charge_ticks: i32,
    state: CrossbowState,
}

impl Default for CrossbowAttack {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossbowAttack {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            attack_delay: 0,
            charge_ticks: 0,
            state: CrossbowState::Uncharged,
        }
    }

    // crossbow: CrossbowItem.getChargeDuration, Quick Charge shortens it
    fn charge_duration(mob: &dyn crate::entity::mob::Mob) -> i32 {
        let level = mob.get_mob_entity().with_main_hand(|stack| {
            stack.get_enchantment_level(&pumpkin_data::Enchantment::QUICK_CHARGE)
        });
        (CHARGE_DURATION - 5 * level).max(0)
    }

    fn can_attack(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        let target = tick.state.get(types::ATTACK_TARGET).cloned()?;
        let distance_sq = tick
            .mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load());
        (RangedCrossbowAttackGoal::is_holding_crossbow(tick.mob)
            && entity_is_visible(&tick.visibility(), target.as_ref())
            && distance_sq < CROSSBOW_RANGE * CROSSBOW_RANGE)
            .then_some(target)
    }
}

impl Behavior for CrossbowAttack {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ENTRY_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        TIMEOUT
    }

    fn max_duration(&self) -> i32 {
        TIMEOUT
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::can_attack(tick).is_some()
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::ATTACK_TARGET.id()) && Self::can_attack(tick).is_some()
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
            return;
        };
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(target.clone(), true)),
        );
        let mob = tick.mob;
        let living = &mob.get_mob_entity().living_entity;
        let pos = mob.get_entity().pos.load();
        let world = mob.get_entity().world.load();
        match self.state {
            CrossbowState::Uncharged => {
                let stack = living
                    .entity_equipment
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&EquipmentSlot::MAIN_HAND);
                living.set_active_hand(Hand::Right, stack, i32::MAX);
                self.state = CrossbowState::Charging;
                self.charge_ticks = 0;
                if let Some(crossbow_mob) = mob.as_crossbow_attack_mob() {
                    crossbow_mob.set_charging_crossbow(true);
                }
                world.play_sound(
                    Sound::ItemCrossbowLoadingStart,
                    SoundCategory::Hostile,
                    &pos,
                );
            }
            CrossbowState::Charging => {
                self.charge_ticks += 1;
                if self.charge_ticks == 10 {
                    world.play_sound(
                        Sound::ItemCrossbowLoadingMiddle,
                        SoundCategory::Hostile,
                        &pos,
                    );
                }
                if self.charge_ticks >= Self::charge_duration(mob) {
                    living.clear_active_hand();
                    self.state = CrossbowState::Charged;
                    self.attack_delay = 20 + mob.get_random().random_range(0..20);
                    if let Some(crossbow_mob) = mob.as_crossbow_attack_mob() {
                        crossbow_mob.set_charging_crossbow(false);
                    }
                    world.play_sound(Sound::ItemCrossbowLoadingEnd, SoundCategory::Hostile, &pos);
                }
            }
            CrossbowState::Charged => {
                self.attack_delay -= 1;
                if self.attack_delay == 0 {
                    self.state = CrossbowState::ReadyToAttack;
                }
            }
            CrossbowState::ReadyToAttack => {
                RangedCrossbowAttackGoal::shoot(mob, &target);
                self.state = CrossbowState::Uncharged;
            }
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let mob = tick.mob;
        mob.get_mob_entity().living_entity.clear_active_hand();
        if let Some(crossbow_mob) = mob.as_crossbow_attack_mob() {
            crossbow_mob.set_charging_crossbow(false);
        }
        self.state = CrossbowState::Uncharged;
    }

    fn debug_name(&self) -> &'static str {
        "CrossbowAttack"
    }
}
