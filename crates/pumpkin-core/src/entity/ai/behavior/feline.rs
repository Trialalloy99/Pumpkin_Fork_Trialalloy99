//! Goals cats and ocelots share

use std::sync::Weak;

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;

use crate::entity::ai::behavior::fleeing::Flee;
use crate::entity::ai::goal::breed::BreedGoal;
use crate::entity::ai::goal::goal_selector::GoalSelector;
use crate::entity::ai::goal::leap_at_target::LeapAtTargetGoal;
use crate::entity::ai::goal::look_at_entity::LookAtEntityGoal;
use crate::entity::ai::goal::ocelot_attack::OcelotAttackGoal;
use crate::entity::ai::goal::revenge::MobFilter;
use crate::entity::ai::goal::tempt::TemptGoal;
use crate::entity::ai::goal::wander_around::WanderAroundGoal;
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::mob::Mob;

pub const TEMPT_ITEMS: &[&Item] = &[&Item::COD, &Item::SALMON];

#[must_use]
pub fn tempt_goal() -> Box<TemptGoal> {
    Box::new(TemptGoal::new(0.6, TEMPT_ITEMS, true))
}

/// Shy of players while `gate` holds: untamed cats, wary ocelots.
#[must_use]
pub const fn flee_players(priority: u8, gate: MobFilter) -> Flee {
    Flee::new(
        priority,
        TargetMatch::Type(&EntityType::PLAYER),
        16.0,
        0.8,
        1.33,
    )
    .as_long_as(gate)
}

/// Leap, pounce, breed, stroll and look at players, on consecutive priorities from `first`.
pub fn apply_tail(goals: &GoalSelector, mob: Weak<dyn Mob>, first: u8) {
    goals.add_goal(first, Box::new(LeapAtTargetGoal::new(0.3)));
    goals.add_goal(first + 1, Box::new(OcelotAttackGoal::new()));
    goals.add_goal(first + 2, BreedGoal::new(0.8));
    goals.add_goal(first + 3, Box::new(WanderAroundGoal::water_avoiding(0.8)));
    goals.add_goal(
        first + 4,
        LookAtEntityGoal::with_default(mob, &EntityType::PLAYER, 10.0),
    );
}

/// Vanilla `Cat.CatTemptGoal`: untamed cats only, and one picked player never scares them.
#[must_use]
pub fn cat_tempt_goal() -> Box<TemptGoal> {
    Box::new(TemptGoal::new(0.6, TEMPT_ITEMS, true).for_cat(|mob| {
        mob.cast_any()
            .downcast_ref::<crate::entity::passive::cat::CatEntity>()
            .is_some_and(|cat| !crate::entity::passive::tamable::TamableAnimal::is_tame(cat))
    }))
}

/// Vanilla `Ocelot.OcelotTemptGoal`: a trusting ocelot is no longer scared.
#[must_use]
pub fn ocelot_tempt_goal() -> Box<TemptGoal> {
    Box::new(
        TemptGoal::new(0.6, TEMPT_ITEMS, true).with_scare_gate(|mob| {
            mob.cast_any()
                .downcast_ref::<crate::entity::passive::ocelot::OcelotEntity>()
                .is_some_and(|ocelot| !ocelot.is_trusting())
        }),
    )
}
