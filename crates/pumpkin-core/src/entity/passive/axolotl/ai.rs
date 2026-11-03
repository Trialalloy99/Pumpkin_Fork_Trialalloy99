use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::potion::Effect;
use pumpkin_data::tag;

use super::AxolotlEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        Behavior, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink, MoveToTargetSink,
        Timed,
        animal_make_love::AnimalMakeLove,
        baby_follow_adult, erase_memory_if,
        gate::{GateBehavior, OrderPolicy, RunningPolicy},
        melee_attack, one_shot, random_stroll, run_one, set_entity_look_target_sometimes,
        set_walk_target_from_attack_target_if_target_out_of_reach,
        set_walk_target_from_look_target, start_attacking, stop_attacking_if_target_invalid,
        try_find_liquid, utils,
    },
    memory::{MemoryModuleId, MemoryStatus, PackedMemories, types},
    sensing::SensorType,
};
use crate::entity::ai::util::goal_utils;
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_MAKING_LOVE: f32 = 0.2;
const SPEED_MULTIPLIER_ON_LAND: f32 = 0.15;
const SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER: f32 = 0.5;
const SPEED_MULTIPLIER_WHEN_CHASING_IN_WATER: f32 = 0.6;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT_IN_WATER: f32 = 0.6;
const ADULT_FOLLOW_RANGE: (i32, i32) = (5, 16);
const LIQUID_SEARCH_RANGE: i32 = 6;
const MELEE_COOLDOWN: i64 = 20;
pub(super) const TOTAL_PLAYDEAD_TIME: i32 = 200;
const HUNTING_COOLDOWN_TICKS: i64 = 2400;
const PLAYER_REGEN_DETECTION_RANGE: f64 = 20.0;

const MEMORIES: [MemoryModuleId; 17] = [
    types::PLAY_DEAD_TICKS.id(),
    types::HAS_HUNTING_COOLDOWN.id(),
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::TEMPTING_PLAYER.id(),
    types::BREED_TARGET.id(),
    types::IS_PANICKING.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::ATTACK_TARGET.id(),
    types::ATTACK_COOLING_DOWN.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
    types::NEAREST_ATTACKABLE.id(),
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_ADULT.id(),
    types::HURT_BY.id(),
    types::HURT_BY_ENTITY.id(),
];

// axolotl: Brain.provider(NEAREST_LIVING_ENTITIES, NEAREST_ADULT, HURT_BY, AXOLOTL_ATTACKABLES, FOOD_TEMPTATIONS)
pub(super) fn make_brain(axolotl: &AxolotlEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::NearestAdult,
            SensorType::HurtBy,
            SensorType::AxolotlAttackables,
            SensorType::FoodTemptations,
        ],
        activities,
    }
    .make_brain(axolotl, packed)
}

fn in_water(mob: &dyn Mob) -> bool {
    mob.get_entity().is_in_water()
}

// axolotl: AxolotlAi.getSpeedModifierChasing
fn speed_modifier_chasing(mob: &dyn Mob) -> f32 {
    if in_water(mob) {
        SPEED_MULTIPLIER_WHEN_CHASING_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

// axolotl: AxolotlAi.getSpeedModifierFollowingAdult
fn speed_modifier_following_adult(mob: &dyn Mob) -> f32 {
    if in_water(mob) {
        SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

// axolotl: AxolotlAi.getSpeedModifier
fn speed_modifier(mob: &dyn Mob) -> f32 {
    if in_water(mob) {
        SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

// axolotl: AxolotlAi.canSetWalkTargetFromLookTarget
fn can_set_walk_target_from_look_target(tick: &BrainTick<'_>) -> bool {
    tick.state.get(types::LOOK_TARGET).is_some_and(|target| {
        goal_utils::is_water(tick.world, &target.current_block_position()) == in_water(tick.mob)
    })
}

// axolotl: AxolotlAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    let old_activity = brain.get_active_non_core_activity();
    if old_activity == Some(Activity::PlayDead) {
        return;
    }
    brain.set_active_activity_to_first_valid(&[
        Activity::PlayDead,
        Activity::Fight,
        Activity::Idle,
    ]);
    if old_activity == Some(Activity::Fight)
        && brain.get_active_non_core_activity() != Some(Activity::Fight)
    {
        brain.set_with_expiry(types::HAS_HUNTING_COOLDOWN, true, HUNTING_COOLDOWN_TICKS);
    }
}

// axolotl: AxolotlAi.findNearestValidAttackTarget
fn find_nearest_valid_attack_target(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
    if utils::is_breeding(tick.state) {
        None
    } else {
        tick.state.get(types::NEAREST_ATTACKABLE).cloned()
    }
}

// axolotl: AxolotlAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        core_activity(),
        idle_activity(),
        fight_activity(),
        play_dead_activity(),
    ]
}

// axolotl: AxolotlAi.initPlayDeadActivity
fn play_dead_activity() -> ActivityData {
    ActivityData::new(
        Activity::PlayDead,
        vec![
            (0, Box::new(Timed::new(PlayDead))),
            (
                1,
                Box::new(erase_memory_if::create(
                    |tick| utils::is_breeding(tick.state),
                    types::PLAY_DEAD_TICKS,
                )),
            ),
        ],
        vec![(types::PLAY_DEAD_TICKS.id(), MemoryStatus::ValuePresent)],
        vec![types::PLAY_DEAD_TICKS.id()],
    )
}

// axolotl: AxolotlAi.initFightActivity
fn fight_activity() -> ActivityData {
    ActivityData::with_gate_memory(
        Activity::Fight,
        0,
        vec![
            Box::new(stop_attacking_if_target_invalid::create_with(
                |_, _| false,
                on_stop_attacking,
                true,
            )),
            Box::new(
                set_walk_target_from_attack_target_if_target_out_of_reach::create_with_speed_fn(
                    speed_modifier_chasing,
                ),
            ),
            Box::new(melee_attack::create(|_| true, MELEE_COOLDOWN)),
            Box::new(erase_memory_if::create(
                |tick| utils::is_breeding(tick.state),
                types::ATTACK_TARGET,
            )),
        ],
        types::ATTACK_TARGET.id(),
    )
}

// axolotl: AxolotlAi.initCoreActivity
fn core_activity() -> ActivityData {
    ActivityData::with_priority_start(
        Activity::Core,
        0,
        vec![
            Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
            Box::new(Timed::new(MoveToTargetSink::default())),
            Box::new(validate_play_dead()),
            Box::new(CountDownCooldownTicks::new(
                types::TEMPTATION_COOLDOWN_TICKS,
            )),
        ],
    )
}

// axolotl: AxolotlAi.initIdleActivity
#[allow(clippy::too_many_lines)]
fn idle_activity() -> ActivityData {
    ActivityData::with_pairs(
        Activity::Idle,
        vec![
            (
                0,
                Box::new(set_entity_look_target_sometimes::create_for_type(
                    &EntityType::PLAYER,
                    6.0,
                    30,
                    60,
                )),
            ),
            (
                1,
                Box::new(Timed::new(
                    AnimalMakeLove::new(&EntityType::AXOLOTL, |_| {})
                        .with_speed_modifier(SPEED_MULTIPLIER_WHEN_MAKING_LOVE)
                        .with_close_enough_distance(2),
                )),
            ),
            (
                2,
                Box::new(run_one(vec![
                    (
                        Box::new(Timed::new(
                            FollowTemptation::new(SPEED_MULTIPLIER_ON_LAND, 2.5, false)
                                .with_speed_fn(speed_modifier),
                        )),
                        1,
                    ),
                    (
                        Box::new(baby_follow_adult::create_with_speed_fn(
                            ADULT_FOLLOW_RANGE.0,
                            ADULT_FOLLOW_RANGE.1,
                            speed_modifier_following_adult,
                            types::NEAREST_VISIBLE_ADULT,
                            false,
                            baby_follow_adult::same_entity,
                        )),
                        1,
                    ),
                ])),
            ),
            (
                3,
                Box::new(start_attacking::create(
                    |_| true,
                    find_nearest_valid_attack_target,
                )),
            ),
            (
                3,
                Box::new(try_find_liquid::create(
                    LIQUID_SEARCH_RANGE,
                    SPEED_MULTIPLIER_ON_LAND,
                    &tag::Fluid::MINECRAFT_AXOLOTL_TRIES_TO_FIND,
                )),
            ),
            (
                4,
                Box::new(GateBehavior::new(
                    "Gate",
                    vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
                    Vec::new(),
                    OrderPolicy::Ordered,
                    RunningPolicy::TryAll,
                    vec![
                        (
                            Box::new(random_stroll::swim(SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER)),
                            2,
                        ),
                        (
                            Box::new(random_stroll::stroll_from_water(
                                SPEED_MULTIPLIER_ON_LAND,
                                false,
                            )),
                            2,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create_with(
                                can_set_walk_target_from_look_target,
                                speed_modifier,
                                3,
                            )),
                            3,
                        ),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), |tick| {
                                in_water(tick.mob)
                            })),
                            5,
                        ),
                        (
                            Box::new(one_shot::trigger_if("TriggerIf", Vec::new(), |tick| {
                                tick.mob.get_entity().on_ground.load(Ordering::Relaxed)
                            })),
                            5,
                        ),
                    ],
                )),
            ),
        ],
    )
}

// axolotl: ValidatePlayDead
fn validate_play_dead() -> one_shot::OneShot {
    one_shot::OneShot::new(
        "ValidatePlayDead",
        vec![
            (types::PLAY_DEAD_TICKS.id(), MemoryStatus::ValuePresent),
            (types::HURT_BY_ENTITY.id(), MemoryStatus::Registered),
        ],
        |tick| {
            let Some(ticks) = tick.state.get(types::PLAY_DEAD_TICKS).copied() else {
                return false;
            };
            if ticks <= 0 {
                tick.state.erase(types::PLAY_DEAD_TICKS.id());
                tick.state.erase(types::HURT_BY_ENTITY.id());
                tick.state.use_default_activity();
            } else {
                tick.state.set(types::PLAY_DEAD_TICKS, ticks - 1);
            }
            true
        },
    )
}

const PLAY_DEAD_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::PLAY_DEAD_TICKS.id(), MemoryStatus::ValuePresent),
    (types::HURT_BY_ENTITY.id(), MemoryStatus::ValuePresent),
];

// axolotl: PlayDead
struct PlayDead;

impl Behavior for PlayDead {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &PLAY_DEAD_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        TOTAL_PLAYDEAD_TIME
    }

    fn max_duration(&self) -> i32 {
        TOTAL_PLAYDEAD_TIME
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        in_water(tick.mob)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        in_water(tick.mob) && tick.state.has_memory_value(types::PLAY_DEAD_TICKS.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::LOOK_TARGET.id());
        tick.mob.get_mob_entity().living_entity.add_effect(Effect {
            effect_type: &StatusEffect::REGENERATION,
            duration: TOTAL_PLAYDEAD_TIME,
            amplifier: 0,
            ambient: false,
            show_particles: true,
            show_icon: true,
            blend: false,
        });
    }

    fn debug_name(&self) -> &'static str {
        "PlayDead"
    }
}

// axolotl: Axolotl.onStopAttacking, the player who killed the target gets regeneration
fn on_stop_attacking(tick: &mut BrainTick<'_>, target: &dyn EntityBase) {
    let Some(axolotl) = tick.mob.cast_any().downcast_ref::<AxolotlEntity>() else {
        return;
    };
    let Some(living) = target.get_living_entity() else {
        return;
    };
    if living.is_alive() {
        return;
    }
    let Some((_, killer_id)) = living.get_last_damage_source() else {
        return;
    };
    let in_range = tick
        .world
        .get_nearby_players(
            axolotl.get_entity().pos.load(),
            PLAYER_REGEN_DETECTION_RANGE,
        )
        .into_iter()
        .find(|player| player.get_entity().entity_id == killer_id);
    if let Some(player) = in_range {
        axolotl.apply_supporting_effects(&player);
    }
}
