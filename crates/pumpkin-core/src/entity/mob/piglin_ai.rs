use std::sync::Arc;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::{BlocksAttacksImpl, EquipmentSlot, KineticWeaponImpl};
use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use crate::entity::ai::brain::activity::ActivityData;
use crate::entity::ai::brain::behavior::{
    DoNothing, LookAtTargetSink, MoveToTargetSink, OneShot, Timed, back_up_if_too_close,
    copy_memory_with_expiry,
    crossbow_attack::CrossbowAttack,
    erase_memory_if, go_to_target_location, go_to_wanted_item, interact_with, interact_with_door,
    melee_attack, random_stroll, run_one, set_entity_look_target, set_look_and_interact,
    set_walk_target_away_from, set_walk_target_from_attack_target_if_target_out_of_reach,
    set_walk_target_from_look_target,
    spear::{SpearApproach, SpearAttack, SpearRetreat},
    start_attacking, start_celebrating_if_target_dead, stop_attacking_if_target_invalid,
    stop_being_angry_if_target_dead, utils,
};
use crate::entity::ai::brain::memory::position_tracker::EntityTracker;
use crate::entity::ai::brain::memory::walk_target::WalkTarget;
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, PackedMemories, types};
use crate::entity::ai::brain::sensing::{
    SensorType, is_entity_attackable, is_entity_attackable_ignoring_line_of_sight,
};
use crate::entity::ai::brain::{Brain, BrainProvider, BrainState, BrainTick, VisibilityContext};
use crate::entity::ai::random_pos::land_random_pos;
use crate::entity::item::ItemEntity;
use crate::entity::living::LivingEntity;
use crate::entity::mob::Mob;
use crate::entity::mob::piglin::PiglinEntity;
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::world::World;

pub struct PiglinAi;

// piglin: Brain.provider memory types besides the sensor ones
const MEMORIES: [MemoryModuleId; 25] = [
    types::UNIVERSAL_ANGER.id(),
    types::ATE_RECENTLY.id(),
    types::SPEAR_FLEEING_TIME.id(),
    types::SPEAR_FLEEING_POSITION.id(),
    types::SPEAR_CHARGE_POSITION.id(),
    types::SPEAR_ENGAGE_TIME.id(),
    types::SPEAR_STATUS.id(),
    types::ADMIRING_ITEM.id(),
    types::ADMIRING_DISABLED.id(),
    types::DISABLE_WALK_TO_ADMIRE_ITEM.id(),
    types::TIME_TRYING_TO_REACH_ADMIRE_ITEM.id(),
    types::HUNTED_RECENTLY.id(),
    types::CELEBRATE_LOCATION.id(),
    types::DANCING.id(),
    types::ANGRY_AT.id(),
    types::AVOID_TARGET.id(),
    types::RIDE_TARGET.id(),
    types::INTERACTION_TARGET.id(),
    types::ATTACK_TARGET.id(),
    types::ATTACK_COOLING_DOWN.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::PATH.id(),
    types::NEAREST_VISIBLE_WANTED_ITEM.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
];

impl PiglinAi {
    pub const REPELLENT_DETECTION_RANGE_HORIZONTAL: i32 = 8;
    pub const REPELLENT_DETECTION_RANGE_VERTICAL: i32 = 4;
    pub const BARTERING_ITEM: &'static Item = &Item::GOLD_INGOT;
    pub const PLAYER_ANGER_RANGE: f64 = 16.0;
    pub const ANGER_DURATION: i64 = 600;
    pub const ADMIRE_DURATION: i64 = 119;
    pub const ADMIRING_DISABLED_DURATION: i64 = 400;
    pub const EAT_COOLDOWN: i64 = 200;
    pub const BABY_FLEE_DURATION: i64 = 100;
    pub const CELEBRATION_TIME: i64 = 300;
    pub const MIN_TIME_BETWEEN_HUNTS: i32 = 600;
    pub const MAX_TIME_BETWEEN_HUNTS: i32 = 2400;
    pub const DESIRED_DISTANCE_FROM_ZOMBIFIED: f64 = 6.0;
    pub const PROBABILITY_OF_CELEBRATION_DANCE: f32 = 0.1;
    pub const MAX_DISTANCE_TO_WALK_TO_ITEM: i32 = 9;
    pub const MAX_TIME_TRYING_TO_REACH_ITEM: i32 = 200;
    pub const DISABLE_WALK_TO_ADMIRE_DURATION: i64 = 200;
    pub const THROW_SPEED: f64 = 0.3;
    pub const THROW_HAND_Y_DISTANCE_FROM_EYE: f64 = 0.3;
    pub const RANDOM_THROW_POS_RANGE_HORIZONTAL: i32 = 4;
    pub const RANDOM_THROW_POS_RANGE_VERTICAL: i32 = 2;
    const RETREAT_DURATION: (i32, i32) = (100, 400);
    const RIDE_START_INTERVAL: (i32, i32) = (200, 800);
    const RIDE_DURATION: (i32, i32) = (200, 600);
    const AVOID_ZOMBIFIED_DURATION: (i32, i32) = (100, 140);
    const HIT_BY_PLAYER_MEMORY_TIMEOUT: i64 = 400;
    const MAX_WALK_DISTANCE_TO_START_RIDING: i32 = 8;
    const SPEED_WHEN_STRAFING_BACK_FROM_TARGET: f32 = 0.75;
    const MIN_DESIRED_DIST_FROM_TARGET_WHEN_HOLDING_CROSSBOW: i32 = 5;
    const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.6;
    const SPEED_MULTIPLIER_WHEN_MOUNTING: f32 = 0.8;
    const SPEED_MULTIPLIER_WHEN_DANCING: f32 = 0.6;

    #[must_use]
    pub const fn is_barter_currency(item_stack: &ItemStack) -> bool {
        item_stack.item.id == Self::BARTERING_ITEM.id
    }

    #[must_use]
    pub fn is_loved_item(item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_PIGLIN_LOVED)
    }

    #[must_use]
    pub fn is_food(item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_PIGLIN_FOOD)
    }

    #[must_use]
    pub const fn is_zombified(entity_type: &EntityType) -> bool {
        entity_type.id == EntityType::ZOMBIFIED_PIGLIN.id || entity_type.id == EntityType::ZOGLIN.id
    }

    #[must_use]
    pub fn is_wearing_safe_armor(entity: &LivingEntity) -> bool {
        let Ok(guard) = entity.entity_equipment.try_lock() else {
            return false;
        };
        [
            EquipmentSlot::HEAD,
            EquipmentSlot::CHEST,
            EquipmentSlot::LEGS,
            EquipmentSlot::FEET,
        ]
        .iter()
        .any(|slot| {
            guard.equipment.get(slot).is_some_and(|stack| {
                !stack.is_empty() && stack.item.has_tag(&tag::Item::MINECRAFT_PIGLIN_SAFE_ARMOR)
            })
        })
    }

    #[must_use]
    pub fn is_player_holding_loved_item(player: &Player) -> bool {
        let inventory = player.inventory();
        Self::is_loved_item(&inventory.held_item())
            || Self::is_loved_item(&inventory.off_hand_item())
    }

    // piglin: PiglinAi.make brain
    pub(super) fn make_brain(piglin: &PiglinEntity, packed: &PackedMemories) -> Brain {
        BrainProvider {
            memory_types: &MEMORIES,
            sensor_types: &[
                SensorType::NearestLivingEntities,
                SensorType::NearestPlayers,
                SensorType::NearestItems,
                SensorType::HurtBy,
                SensorType::PiglinSpecific,
            ],
            activities: Self::activities,
        }
        .make_brain(piglin, packed)
    }

    // piglin: PiglinAi.initMemories
    pub fn init_memories(state: &mut BrainState, mob: &dyn Mob) {
        let delay = mob
            .get_random()
            .random_range(Self::MIN_TIME_BETWEEN_HUNTS..=Self::MAX_TIME_BETWEEN_HUNTS);
        state.set_with_expiry(types::HUNTED_RECENTLY, true, i64::from(delay));
    }

    // piglin: PiglinAi.getActivities
    fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::admire_item_activity(),
            Self::fight_activity(),
            Self::celebrate_activity(),
            Self::retreat_activity(),
            Self::ride_hoglin_activity(),
        ]
    }

    // piglin: PiglinAi.initCoreActivity
    fn core_activity() -> ActivityData {
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
                Box::new(interact_with_door::create()),
                Box::new(copy_memory_with_expiry::create(
                    |tick| piglin_of(tick.mob).is_some_and(PiglinEntity::is_baby),
                    types::NEAREST_VISIBLE_NEMESIS,
                    types::AVOID_TARGET,
                    Self::AVOID_ZOMBIFIED_DURATION.0,
                    Self::AVOID_ZOMBIFIED_DURATION.1,
                )),
                Box::new(copy_memory_with_expiry::create(
                    |tick| Self::is_near_zombified(tick.state, tick.mob),
                    types::NEAREST_VISIBLE_ZOMBIFIED,
                    types::AVOID_TARGET,
                    Self::AVOID_ZOMBIFIED_DURATION.0,
                    Self::AVOID_ZOMBIFIED_DURATION.1,
                )),
                Box::new(Self::stop_holding_item_if_no_longer_admiring()),
                Box::new(Self::start_admiring_item_if_seen()),
                Box::new(start_celebrating_if_target_dead::create(
                    Self::CELEBRATION_TIME,
                    Self::wants_to_dance,
                )),
                Box::new(stop_being_angry_if_target_dead::create()),
            ],
        )
    }

    // piglin: PiglinAi.initIdleActivity
    fn idle_activity() -> ActivityData {
        ActivityData::with_priority_start(
            Activity::Idle,
            10,
            vec![
                Box::new(set_entity_look_target::create_with(
                    Self::is_entity_player_holding_loved_item,
                    14.0,
                )),
                Box::new(start_attacking::create(
                    |tick| piglin_of(tick.mob).is_some_and(PiglinEntity::is_adult),
                    Self::find_nearest_valid_attack_target,
                )),
                Box::new(
                    Self::start_hunting_hoglin()
                        .when(|tick| piglin_of(tick.mob).is_some_and(PiglinEntity::can_hunt)),
                ),
                Box::new(Self::avoid_repellent()),
                Box::new(Self::baby_sometimes_ride_baby_hoglin()),
                Box::new(Self::idle_look_behaviors()),
                Box::new(Self::idle_movement_behaviors()),
                Box::new(set_look_and_interact::create(&EntityType::PLAYER, 4)),
            ],
        )
    }

    // piglin: PiglinAi.initFightActivity
    fn fight_activity() -> ActivityData {
        ActivityData::with_priority_start_and_conditions(
            Activity::Fight,
            10,
            vec![
                Box::new(stop_attacking_if_target_invalid::create(|tick, target| {
                    !Self::is_nearest_valid_attack_target(tick, target)
                })),
                Box::new(
                    back_up_if_too_close::create(
                        Self::MIN_DESIRED_DIST_FROM_TARGET_WHEN_HOLDING_CROSSBOW,
                        Self::SPEED_WHEN_STRAFING_BACK_FROM_TARGET,
                    )
                    .when(|tick| Self::has_crossbow(tick.mob)),
                ),
                Box::new(set_walk_target_from_attack_target_if_target_out_of_reach::create(1.0)),
                Box::new(Timed::new(SpearApproach::new(1.0, 10.0))),
                Box::new(Timed::new(SpearAttack::new(1.0, 1.0, 2.0))),
                Box::new(Timed::new(SpearRetreat::new(1.0))),
                Box::new(melee_attack::create(
                    |mob| !Self::is_holding_usable_non_melee_weapon(mob),
                    20,
                )),
                Box::new(Timed::new(CrossbowAttack::new())),
                Box::new(Self::remember_if_hoglin_was_killed()),
                Box::new(erase_memory_if::create(
                    |tick| Self::is_near_zombified(tick.state, tick.mob),
                    types::ATTACK_TARGET,
                )),
            ],
            vec![(types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent)],
        )
    }

    // piglin: PiglinAi.initCelebrateActivity
    fn celebrate_activity() -> ActivityData {
        ActivityData::with_priority_start_and_conditions(
            Activity::Celebrate,
            10,
            vec![
                Box::new(Self::avoid_repellent()),
                Box::new(set_entity_look_target::create_with(
                    Self::is_entity_player_holding_loved_item,
                    14.0,
                )),
                Box::new(start_attacking::create(
                    |tick| piglin_of(tick.mob).is_some_and(PiglinEntity::is_adult),
                    Self::find_nearest_valid_attack_target,
                )),
                Box::new(
                    go_to_target_location::create(types::CELEBRATE_LOCATION, 2, 1.0)
                        .when(|tick| !piglin_of(tick.mob).is_some_and(PiglinEntity::is_dancing)),
                ),
                Box::new(
                    go_to_target_location::create(
                        types::CELEBRATE_LOCATION,
                        4,
                        Self::SPEED_MULTIPLIER_WHEN_DANCING,
                    )
                    .when(|tick| piglin_of(tick.mob).is_some_and(PiglinEntity::is_dancing)),
                ),
                Box::new(run_one(vec![
                    (
                        Box::new(set_entity_look_target::create_for_type(
                            &EntityType::PIGLIN,
                            8.0,
                        )),
                        1,
                    ),
                    (
                        Box::new(random_stroll::stroll_in_range(
                            Self::SPEED_MULTIPLIER_WHEN_IDLING,
                            2,
                            1,
                        )),
                        1,
                    ),
                    (Box::new(DoNothing::new(10, 20)), 1),
                ])),
            ],
            vec![(types::CELEBRATE_LOCATION.id(), MemoryStatus::ValuePresent)],
        )
    }

    // piglin: PiglinAi.initAdmireItemActivity
    fn admire_item_activity() -> ActivityData {
        ActivityData::with_priority_start_and_conditions(
            Activity::AdmireItem,
            10,
            vec![
                Box::new(
                    go_to_wanted_item::create(1.0, true, Self::MAX_DISTANCE_TO_WALK_TO_ITEM)
                        .when(|tick| Self::is_not_holding_loved_item_in_off_hand(tick.mob)),
                ),
                Box::new(Self::stop_admiring_if_item_too_far_away(
                    Self::MAX_DISTANCE_TO_WALK_TO_ITEM,
                )),
                Box::new(Self::stop_admiring_if_tired_of_trying_to_reach_item(
                    Self::MAX_TIME_TRYING_TO_REACH_ITEM,
                    Self::DISABLE_WALK_TO_ADMIRE_DURATION,
                )),
            ],
            vec![(types::ADMIRING_ITEM.id(), MemoryStatus::ValuePresent)],
        )
    }

    // piglin: PiglinAi.initRetreatActivity
    fn retreat_activity() -> ActivityData {
        ActivityData::with_priority_start_and_conditions(
            Activity::Avoid,
            10,
            vec![
                Box::new(set_walk_target_away_from::entity(
                    types::AVOID_TARGET,
                    1.0,
                    12,
                    true,
                )),
                Box::new(Self::idle_look_behaviors()),
                Box::new(Self::idle_movement_behaviors()),
                Box::new(erase_memory_if::create(
                    |tick| Self::wants_to_stop_fleeing(tick.state, tick.mob),
                    types::AVOID_TARGET,
                )),
            ],
            vec![(types::AVOID_TARGET.id(), MemoryStatus::ValuePresent)],
        )
    }

    // piglin: PiglinAi.initRideHoglinActivity
    fn ride_hoglin_activity() -> ActivityData {
        ActivityData::with_priority_start_and_conditions(
            Activity::Ride,
            10,
            vec![
                Box::new(Self::mount(Self::SPEED_MULTIPLIER_WHEN_MOUNTING)),
                Box::new(set_entity_look_target::create_with(
                    Self::is_entity_player_holding_loved_item,
                    8.0,
                )),
                Box::new(Self::ride_look_behaviors()),
                Box::new(Self::dismount_or_skip_mounting(
                    Self::MAX_WALK_DISTANCE_TO_START_RIDING,
                )),
            ],
            vec![(types::RIDE_TARGET.id(), MemoryStatus::ValuePresent)],
        )
    }

    // piglin: PiglinAi.createIdleLookBehaviors
    fn idle_look_behaviors() -> crate::entity::ai::brain::behavior::GateBehavior {
        run_one(vec![
            (
                Box::new(set_entity_look_target::create_for_type(
                    &EntityType::PLAYER,
                    8.0,
                )),
                1,
            ),
            (
                Box::new(set_entity_look_target::create_for_type(
                    &EntityType::PIGLIN,
                    8.0,
                )),
                1,
            ),
            (Box::new(set_entity_look_target::create_any(8.0)), 1),
            (Box::new(DoNothing::new(30, 60)), 1),
        ])
    }

    // piglin: PiglinAi.createIdleMovementBehaviors
    fn idle_movement_behaviors() -> crate::entity::ai::brain::behavior::GateBehavior {
        run_one(vec![
            (
                Box::new(random_stroll::stroll(Self::SPEED_MULTIPLIER_WHEN_IDLING)),
                2,
            ),
            (
                Box::new(interact_with::of(
                    &EntityType::PIGLIN,
                    8,
                    types::INTERACTION_TARGET,
                    Self::SPEED_MULTIPLIER_WHEN_IDLING,
                    2,
                )),
                2,
            ),
            (
                Box::new(
                    set_walk_target_from_look_target::create(Self::SPEED_MULTIPLIER_WHEN_IDLING, 3)
                        .when(|tick| {
                            !tick
                                .state
                                .has_memory_value(types::NEAREST_PLAYER_HOLDING_WANTED_ITEM.id())
                        }),
                ),
                2,
            ),
            (Box::new(DoNothing::new(30, 60)), 1),
        ])
    }

    // piglin: PiglinAi.avoidRepellent
    fn avoid_repellent() -> OneShot {
        set_walk_target_away_from::pos(types::NEAREST_REPELLENT, 1.0, 8, false)
    }

    // piglin: PiglinAi.babySometimesRideBabyHoglin, SetEntityLookTargetSometimes.Ticker
    fn baby_sometimes_ride_baby_hoglin() -> OneShot {
        let mut ticks_until_next_start = 0;
        OneShot::new(
            "CopyMemoryWithExpiry",
            vec![
                (
                    types::NEAREST_VISIBLE_BABY_HOGLIN.id(),
                    MemoryStatus::ValuePresent,
                ),
                (types::RIDE_TARGET.id(), MemoryStatus::ValueAbsent),
            ],
            move |tick| {
                if !piglin_of(tick.mob).is_some_and(PiglinEntity::is_baby) {
                    return false;
                }
                let start = if ticks_until_next_start == 0 {
                    ticks_until_next_start =
                        tick.mob.get_random().random_range(
                            Self::RIDE_START_INTERVAL.0..=Self::RIDE_START_INTERVAL.1,
                        ) - 1;
                    false
                } else {
                    ticks_until_next_start -= 1;
                    ticks_until_next_start == 0
                };
                if !start {
                    return false;
                }
                let Some(hoglin) = tick.state.get(types::NEAREST_VISIBLE_BABY_HOGLIN).cloned()
                else {
                    return false;
                };
                let duration = tick
                    .mob
                    .get_random()
                    .random_range(Self::RIDE_DURATION.0..=Self::RIDE_DURATION.1);
                tick.state
                    .set_with_expiry(types::RIDE_TARGET, hoglin, i64::from(duration));
                true
            },
        )
    }

    // piglin: StartAdmiringItemIfSeen
    fn start_admiring_item_if_seen() -> OneShot {
        OneShot::new(
            "StartAdmiringItemIfSeen",
            vec![
                (
                    types::NEAREST_VISIBLE_WANTED_ITEM.id(),
                    MemoryStatus::ValuePresent,
                ),
                (types::ADMIRING_ITEM.id(), MemoryStatus::ValueAbsent),
                (types::ADMIRING_DISABLED.id(), MemoryStatus::ValueAbsent),
                (
                    types::DISABLE_WALK_TO_ADMIRE_ITEM.id(),
                    MemoryStatus::ValueAbsent,
                ),
            ],
            |tick| {
                let loved = tick
                    .state
                    .get(types::NEAREST_VISIBLE_WANTED_ITEM)
                    .and_then(|item| item.get_item_entity())
                    .is_some_and(|item| Self::is_loved_item(&item_stack_of(item)));
                if !loved {
                    return false;
                }
                tick.state
                    .set_with_expiry(types::ADMIRING_ITEM, true, Self::ADMIRE_DURATION);
                true
            },
        )
    }

    // piglin: StopAdmiringIfItemTooFarAway
    fn stop_admiring_if_item_too_far_away(max_distance_to_item: i32) -> OneShot {
        let max_sq = f64::from(max_distance_to_item * max_distance_to_item);
        OneShot::new(
            "StopAdmiringIfItemTooFarAway",
            vec![
                (types::ADMIRING_ITEM.id(), MemoryStatus::ValuePresent),
                (
                    types::NEAREST_VISIBLE_WANTED_ITEM.id(),
                    MemoryStatus::Registered,
                ),
            ],
            move |tick| {
                if !off_hand_of(tick.mob).is_empty() {
                    return false;
                }
                let own = tick.mob.get_entity().pos.load();
                let near = tick
                    .state
                    .get(types::NEAREST_VISIBLE_WANTED_ITEM)
                    .is_some_and(|item| {
                        item.get_entity().pos.load().squared_distance_to_vec(&own) < max_sq
                    });
                if near {
                    return false;
                }
                tick.state.erase(types::ADMIRING_ITEM.id());
                true
            },
        )
    }

    // piglin: StopAdmiringIfTiredOfTryingToReachItem
    fn stop_admiring_if_tired_of_trying_to_reach_item(
        max_time_to_reach_item: i32,
        disable_time: i64,
    ) -> OneShot {
        OneShot::new(
            "StopAdmiringIfTiredOfTryingToReachItem",
            vec![
                (types::ADMIRING_ITEM.id(), MemoryStatus::ValuePresent),
                (
                    types::NEAREST_VISIBLE_WANTED_ITEM.id(),
                    MemoryStatus::ValuePresent,
                ),
                (
                    types::TIME_TRYING_TO_REACH_ADMIRE_ITEM.id(),
                    MemoryStatus::Registered,
                ),
                (
                    types::DISABLE_WALK_TO_ADMIRE_ITEM.id(),
                    MemoryStatus::Registered,
                ),
            ],
            move |tick| {
                if !off_hand_of(tick.mob).is_empty() {
                    return false;
                }
                match tick
                    .state
                    .get(types::TIME_TRYING_TO_REACH_ADMIRE_ITEM)
                    .copied()
                {
                    None => tick.state.set(types::TIME_TRYING_TO_REACH_ADMIRE_ITEM, 0),
                    Some(time) if time > max_time_to_reach_item => {
                        tick.state.erase(types::ADMIRING_ITEM.id());
                        tick.state
                            .erase(types::TIME_TRYING_TO_REACH_ADMIRE_ITEM.id());
                        tick.state.set_with_expiry(
                            types::DISABLE_WALK_TO_ADMIRE_ITEM,
                            true,
                            disable_time,
                        );
                    }
                    Some(time) => tick
                        .state
                        .set(types::TIME_TRYING_TO_REACH_ADMIRE_ITEM, time + 1),
                }
                true
            },
        )
    }

    // piglin: StopHoldingItemIfNoLongerAdmiring
    fn stop_holding_item_if_no_longer_admiring() -> OneShot {
        OneShot::new(
            "StopHoldingItemIfNoLongerAdmiring",
            vec![(types::ADMIRING_ITEM.id(), MemoryStatus::ValueAbsent)],
            |tick| {
                let off_hand = off_hand_of(tick.mob);
                if off_hand.is_empty()
                    || off_hand.get_data_component::<BlocksAttacksImpl>().is_some()
                {
                    return false;
                }
                let Some(piglin) = piglin_of(tick.mob) else {
                    return false;
                };
                Self::stop_holding_off_hand_item(piglin, tick.state, true);
                true
            },
        )
    }

    // piglin: StartHuntingHoglin
    fn start_hunting_hoglin() -> OneShot {
        OneShot::new(
            "StartHuntingHoglin",
            vec![
                (
                    types::NEAREST_VISIBLE_HUNTABLE_HOGLIN.id(),
                    MemoryStatus::ValuePresent,
                ),
                (types::ANGRY_AT.id(), MemoryStatus::ValueAbsent),
                (types::HUNTED_RECENTLY.id(), MemoryStatus::ValueAbsent),
                (
                    types::NEAREST_VISIBLE_ADULT_PIGLINS.id(),
                    MemoryStatus::Registered,
                ),
            ],
            |tick| {
                let Some(piglin) = piglin_of(tick.mob) else {
                    return false;
                };
                if piglin.is_baby() {
                    return false;
                }
                let others = tick
                    .state
                    .get(types::NEAREST_VISIBLE_ADULT_PIGLINS)
                    .cloned()
                    .unwrap_or_default();
                if others.iter().any(|other| {
                    other.get_mob().is_some_and(|mob| {
                        mob.get_mob_entity()
                            .try_with_brain(mob, |brain| {
                                brain.has_memory_value(types::HUNTED_RECENTLY.id())
                            })
                            .unwrap_or(false)
                    })
                }) {
                    return false;
                }
                let Some(target) = tick
                    .state
                    .get(types::NEAREST_VISIBLE_HUNTABLE_HOGLIN)
                    .cloned()
                else {
                    return false;
                };
                Self::set_anger_target(tick.state, tick.world, tick.mob, &target);
                Self::dont_kill_any_more_hoglins_for_a_while(tick.state, tick.mob);
                Self::broadcast_anger_target(tick.state, tick.world, &target);
                for other in &others {
                    if let Some(mob) = other.get_mob() {
                        mob.get_mob_entity().try_with_brain(mob, |brain| {
                            Self::dont_kill_any_more_hoglins_for_a_while(brain, mob);
                        });
                    }
                }
                true
            },
        )
    }

    // piglin: RememberIfHoglinWasKilled
    fn remember_if_hoglin_was_killed() -> OneShot {
        OneShot::new(
            "RememberIfHoglinWasKilled",
            vec![
                (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
                (types::HUNTED_RECENTLY.id(), MemoryStatus::Registered),
            ],
            |tick| {
                let killed_hoglin = tick.state.get(types::ATTACK_TARGET).is_some_and(|target| {
                    target.get_entity().entity_type == &EntityType::HOGLIN
                        && target
                            .get_living_entity()
                            .is_some_and(LivingEntity::is_dead_or_dying)
                });
                if killed_hoglin {
                    Self::dont_kill_any_more_hoglins_for_a_while(tick.state, tick.mob);
                }
                true
            },
        )
    }

    // piglin: Mount.create
    fn mount(speed_modifier: f32) -> OneShot {
        OneShot::new(
            "Mount",
            vec![
                (types::LOOK_TARGET.id(), MemoryStatus::Registered),
                (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
                (types::RIDE_TARGET.id(), MemoryStatus::ValuePresent),
            ],
            move |tick| {
                let body = tick.mob.get_entity();
                if body.has_vehicle() {
                    return false;
                }
                let Some(vehicle) = tick.state.get(types::RIDE_TARGET).cloned() else {
                    return false;
                };
                let distance_sq = body
                    .pos
                    .load()
                    .squared_distance_to_vec(&vehicle.get_entity().pos.load());
                if distance_sq < 1.0 {
                    Self::start_riding(tick, vehicle);
                } else {
                    tick.state.set(
                        types::LOOK_TARGET,
                        Arc::new(EntityTracker::new(vehicle.clone(), true)),
                    );
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::new(
                            Arc::new(EntityTracker::new(vehicle, false)),
                            speed_modifier,
                            1,
                        ),
                    );
                }
                true
            },
        )
    }

    // piglin: Piglin.startRiding, babies climb to the top of a hoglin stack
    fn start_riding(tick: &BrainTick<'_>, vehicle: Arc<dyn EntityBase>) {
        let is_baby = piglin_of(tick.mob).is_some_and(PiglinEntity::is_baby);
        let vehicle = if is_baby && vehicle.get_entity().entity_type == &EntityType::HOGLIN {
            Self::top_passenger(&vehicle, 3)
        } else {
            vehicle
        };
        if let Some(body) = tick.world.get_entity_by_id(tick.mob.get_entity().entity_id) {
            vehicle.get_entity().add_passenger(vehicle.clone(), body);
        }
    }

    // piglin: Piglin.getTopPassenger
    fn top_passenger(vehicle: &Arc<dyn EntityBase>, counter: i32) -> Arc<dyn EntityBase> {
        let first = vehicle
            .get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .cloned();
        match first {
            Some(first) if counter != 1 => Self::top_passenger(&first, counter - 1),
            _ => vehicle.clone(),
        }
    }

    // piglin: DismountOrSkipMounting.create
    fn dismount_or_skip_mounting(max_walk_dist_to_ride_target: i32) -> OneShot {
        let max_sq = f64::from(max_walk_dist_to_ride_target * max_walk_dist_to_ride_target);
        OneShot::new(
            "DismountOrSkipMounting",
            vec![(types::RIDE_TARGET.id(), MemoryStatus::Registered)],
            move |tick| {
                let body = tick.mob.get_entity();
                let current = body
                    .vehicle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let target = tick.state.get(types::RIDE_TARGET).cloned();
                let Some(vehicle) = current.or(target) else {
                    return false;
                };
                let valid = vehicle.get_entity().is_alive()
                    && vehicle
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&body.pos.load())
                        < max_sq
                    && vehicle.get_entity().world.load().uuid == tick.world.uuid;
                if valid && !Self::wants_to_stop_riding(tick.state, tick.mob, &vehicle) {
                    return false;
                }
                vehicle.get_entity().remove_passenger(body.entity_id);
                tick.state.erase(types::RIDE_TARGET.id());
                true
            },
        )
    }

    // piglin: ride activity look behaviours, only while a passenger
    fn ride_look_behaviors() -> OneShot {
        OneShot::new(
            "RideLook",
            vec![
                (types::LOOK_TARGET.id(), MemoryStatus::Registered),
                (
                    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                    MemoryStatus::Registered,
                ),
            ],
            |tick| {
                if !tick.mob.get_entity().has_vehicle() {
                    return false;
                }
                let body = tick.mob.get_entity();
                let position = body.pos.load();
                let pick = tick
                    .state
                    .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                    .and_then(|visible| {
                        let ctx = tick.visibility();
                        let within = |entity: &Arc<dyn EntityBase>| {
                            entity
                                .get_entity()
                                .pos
                                .load()
                                .squared_distance_to_vec(&position)
                                <= 64.0
                                && !body.has_passenger(entity.get_entity().entity_id)
                        };
                        let options = [
                            visible.find_closest(&ctx, |e| {
                                within(e) && e.get_entity().entity_type == &EntityType::PLAYER
                            }),
                            visible.find_closest(&ctx, |e| {
                                within(e) && e.get_entity().entity_type == &EntityType::PIGLIN
                            }),
                            visible.find_closest(&ctx, within),
                        ];
                        let start = tick.mob.get_random().random_range(0..options.len());
                        (0..options.len())
                            .map(|i| &options[(start + i) % options.len()])
                            .find_map(Clone::clone)
                    });
                if let Some(target) = pick {
                    utils::look_at_entity(tick.state, target);
                }
                true
            },
        )
    }

    // piglin: PiglinAi.updateActivity
    pub fn update_activity(piglin: &PiglinEntity, brain: &mut Brain) {
        let old_activity = brain.get_active_non_core_activity();
        brain.set_active_activity_to_first_valid(&[
            Activity::AdmireItem,
            Activity::Fight,
            Activity::Avoid,
            Activity::Celebrate,
            Activity::Ride,
            Activity::Idle,
        ]);
        let new_activity = brain.get_active_non_core_activity();
        if old_activity != new_activity
            && let Some(sound) = Self::get_sound_for_current_activity(piglin, brain)
        {
            piglin.make_sound(sound);
        }
        piglin
            .mob_entity
            .set_attacking(brain.has_memory_value(types::ATTACK_TARGET.id()));
        if !brain.has_memory_value(types::RIDE_TARGET.id()) && Self::is_baby_riding_baby(piglin) {
            let entity = piglin.get_entity();
            let vehicle = entity
                .vehicle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(vehicle) = vehicle {
                vehicle.get_entity().remove_passenger(entity.entity_id);
            }
        }
        if !brain.has_memory_value(types::CELEBRATE_LOCATION.id()) {
            brain.erase(types::DANCING.id());
        }
        piglin.set_dancing(brain.has_memory_value(types::DANCING.id()));
    }

    // piglin: PiglinAi.isBabyRidingBaby
    fn is_baby_riding_baby(piglin: &PiglinEntity) -> bool {
        if !piglin.is_baby() {
            return false;
        }
        let vehicle = piglin
            .get_entity()
            .vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        vehicle.is_some_and(|vehicle| {
            let vehicle_type = vehicle.get_entity().entity_type;
            (vehicle_type == &EntityType::PIGLIN || vehicle_type == &EntityType::HOGLIN)
                && vehicle
                    .get_living_entity()
                    .is_some_and(LivingEntity::is_baby)
        })
    }

    // piglin: PiglinAi.pickUpItem
    pub fn pick_up_item(piglin: &PiglinEntity, item: &ItemEntity) {
        Self::stop_walking(piglin);
        let count = {
            let stack = item_stack_of(item);
            if stack.item.id == Item::GOLD_NUGGET.id {
                stack.item_count
            } else {
                stack.item_count.min(1)
            }
        };
        if count == 0
            || !piglin
                .mob_entity
                .living_entity
                .pickup(item.get_entity(), u32::from(count))
        {
            return;
        }
        let taken = item
            .get_item_stack()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .split(count);
        let emptied = item
            .get_item_stack()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if emptied {
            item.get_entity().remove();
        } else {
            item.init_data_tracker();
        }

        if Self::is_loved_item(&taken) {
            piglin.trigger_thrower_advancement(item, &taken);
            piglin.mob_entity.with_brain(piglin, |brain| {
                brain.erase(types::TIME_TRYING_TO_REACH_ADMIRE_ITEM.id());
                Self::admire_gold_item(brain);
            });
            Self::hold_in_off_hand(piglin, taken);
        } else if Self::is_food(&taken)
            && !piglin
                .mob_entity
                .with_brain(piglin, |brain| Self::has_eaten_recently(brain))
        {
            piglin.mob_entity.with_brain(piglin, |brain| {
                brain.set_with_expiry(types::ATE_RECENTLY, true, Self::EAT_COOLDOWN);
            });
        } else if crate::entity::mob::equipment::equip_item_if_possible(piglin, taken.clone())
            .is_empty()
        {
            Self::put_in_inventory(piglin, taken);
        }
    }

    // piglin: PiglinAi.holdInOffhand
    pub fn hold_in_off_hand(piglin: &PiglinEntity, item: ItemStack) {
        if !piglin.off_hand_item().is_empty() {
            piglin
                .get_entity()
                .spawn_at_location(piglin.off_hand_item());
        }
        piglin.hold_in_off_hand(item);
    }

    // piglin: PiglinAi.stopHoldingOffHandItem
    pub fn stop_holding_off_hand_item(
        piglin: &PiglinEntity,
        state: &BrainState,
        bartering_enabled: bool,
    ) {
        let item = piglin
            .mob_entity
            .set_item_slot(&EquipmentSlot::OFF_HAND, ItemStack::EMPTY.clone());
        if piglin.is_adult() {
            let is_barter = Self::is_barter_currency(&item);
            if bartering_enabled && is_barter {
                let outcomes = Self::get_barter_response_items();
                let entity = piglin.get_entity();
                let mut event =
                    crate::plugin::api::events::entity::piglin_barter::PiglinBarterEvent::new(
                        entity.entity_id,
                        item,
                        outcomes,
                    );
                if let Some(server) = entity.world.load().server.upgrade() {
                    server.plugin_manager.fire_blocking(&server, &mut event);
                }
                if !event.cancelled {
                    Self::throw_items(piglin, state, event.outcome);
                }
            } else if !is_barter
                && crate::entity::mob::equipment::equip_item_if_possible(piglin, item.clone())
                    .is_empty()
            {
                Self::put_in_inventory(piglin, item);
            }
        } else if crate::entity::mob::equipment::equip_item_if_possible(piglin, item.clone())
            .is_empty()
        {
            let main_hand = piglin.main_hand_item();
            if Self::is_loved_item(&main_hand) {
                Self::put_in_inventory(piglin, main_hand);
            } else {
                Self::throw_items(piglin, state, vec![main_hand]);
            }
            piglin.hold_in_main_hand(item);
        }
    }

    // piglin: PiglinAi.cancelAdmiring
    pub fn cancel_admiring(piglin: &PiglinEntity, state: &BrainState) {
        if state.has_memory_value(types::ADMIRING_ITEM.id()) && !piglin.off_hand_item().is_empty() {
            piglin
                .get_entity()
                .spawn_at_location(piglin.off_hand_item());
            piglin
                .mob_entity
                .set_item_slot(&EquipmentSlot::OFF_HAND, ItemStack::EMPTY.clone());
        }
    }

    // piglin: PiglinAi.putInInventory
    fn put_in_inventory(piglin: &PiglinEntity, item: ItemStack) {
        let remainder = piglin
            .add_to_inventory(item)
            .unwrap_or_else(|| ItemStack::EMPTY.clone());
        Self::throw_items_toward_random_pos(piglin, vec![remainder]);
    }

    // piglin: PiglinAi.getBarterResponseItems
    fn get_barter_response_items() -> Vec<ItemStack> {
        crate::world::loot::generate_loot(
            &pumpkin_data::loot_table::GAMEPLAY_PIGLIN_BARTERING,
            rand::random::<i64>(),
        )
    }

    // piglin: PiglinAi.wantsToDance
    fn wants_to_dance(tick: &BrainTick<'_>, killed: &dyn EntityBase) -> bool {
        killed.get_entity().entity_type == &EntityType::HOGLIN
            && tick.mob.get_random().random::<f32>() < Self::PROBABILITY_OF_CELEBRATION_DANCE
    }

    // piglin: PiglinAi.wantsToPickup
    #[must_use]
    pub fn wants_to_pickup(piglin: &PiglinEntity, state: &BrainState, stack: &ItemStack) -> bool {
        if piglin.is_baby()
            && stack
                .item
                .has_tag(&tag::Item::MINECRAFT_IGNORED_BY_PIGLIN_BABIES)
        {
            return false;
        }
        if stack.item.has_tag(&tag::Item::MINECRAFT_PIGLIN_REPELLENTS) {
            return false;
        }
        if state.has_memory_value(types::ADMIRING_DISABLED.id())
            && state.has_memory_value(types::ATTACK_TARGET.id())
        {
            return false;
        }
        if Self::is_barter_currency(stack) {
            return Self::is_not_holding_loved_item_in_off_hand(piglin);
        }
        let has_space = piglin.can_add_to_inventory(stack);
        if stack.item.id == Item::GOLD_NUGGET.id {
            return has_space;
        }
        if Self::is_food(stack) {
            return !Self::has_eaten_recently(state) && has_space;
        }
        if !Self::is_loved_item(stack) {
            return piglin.can_replace_current_item_for(stack);
        }
        Self::is_not_holding_loved_item_in_off_hand(piglin) && has_space
    }

    // piglin: PiglinAi.wantsToStopRiding
    fn wants_to_stop_riding(
        state: &BrainState,
        _mob: &dyn Mob,
        vehicle: &Arc<dyn EntityBase>,
    ) -> bool {
        let Some(vehicle_mob) = vehicle.get_mob() else {
            return false;
        };
        let vehicle_living = vehicle.get_living_entity();
        !vehicle_living.is_some_and(LivingEntity::is_baby)
            || !vehicle.get_entity().is_alive()
            || state.has_memory_value(types::HURT_BY.id())
            || vehicle_mob
                .get_mob_entity()
                .try_with_brain(vehicle_mob, |brain| {
                    brain.has_memory_value(types::HURT_BY.id())
                })
                .unwrap_or(false)
            || (vehicle.get_entity().entity_type == &EntityType::PIGLIN
                && !vehicle.get_entity().has_vehicle())
    }

    // piglin: PiglinAi.isNearestValidAttackTarget
    fn is_nearest_valid_attack_target(tick: &BrainTick<'_>, target: &dyn EntityBase) -> bool {
        Self::find_nearest_valid_attack_target(tick)
            .is_some_and(|nearest| nearest.get_entity().entity_id == target.get_entity().entity_id)
    }

    // piglin: PiglinAi.isNearZombified
    fn is_near_zombified(state: &BrainState, mob: &dyn Mob) -> bool {
        state
            .get(types::NEAREST_VISIBLE_ZOMBIFIED)
            .is_some_and(|zombified| {
                mob.get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&zombified.get_entity().pos.load())
                    < Self::DESIRED_DISTANCE_FROM_ZOMBIFIED * Self::DESIRED_DISTANCE_FROM_ZOMBIFIED
            })
    }

    // piglin: PiglinAi.findNearestValidAttackTarget
    fn find_nearest_valid_attack_target(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        if Self::is_near_zombified(tick.state, tick.mob) {
            return None;
        }
        let ctx = tick.visibility();
        if let Some(angry_at) =
            utils::get_living_entity_from_uuid_memory(tick.state, tick.world, types::ANGRY_AT)
            && is_entity_attackable_ignoring_line_of_sight(&ctx, angry_at.as_ref())
        {
            return Some(angry_at);
        }
        if tick.state.has_memory_value(types::UNIVERSAL_ANGER.id())
            && let Some(player) = tick.state.get(types::NEAREST_VISIBLE_ATTACKABLE_PLAYER)
        {
            return Some(Arc::clone(player) as Arc<dyn EntityBase>);
        }
        if let Some(nemesis) = tick.state.get(types::NEAREST_VISIBLE_NEMESIS) {
            return Some(nemesis.clone());
        }
        tick.state
            .get(types::NEAREST_TARGETABLE_PLAYER_NOT_WEARING_GOLD)
            .filter(|player| is_entity_attackable(&ctx, player.as_ref()))
            .map(|player| Arc::clone(player) as Arc<dyn EntityBase>)
    }

    // piglin: PiglinAi.angerNearbyPiglins
    pub fn anger_nearby_piglins(player: &Arc<Player>, only_if_they_see_the_player: bool) {
        let player_entity = player.get_entity();
        let world = player_entity.world.load();
        let area = player_entity.bounding_box.load().expand(
            Self::PLAYER_ANGER_RANGE,
            Self::PLAYER_ANGER_RANGE,
            Self::PLAYER_ANGER_RANGE,
        );
        let universal_anger = world.level_info.load().game_rules.universal_anger;
        let player_arc: Arc<dyn EntityBase> = player.clone();
        for candidate in world.get_entities_at_box(&area) {
            if candidate.get_entity().entity_type != &EntityType::PIGLIN {
                continue;
            }
            with_piglin_brain(&candidate, |piglin, brain| {
                if !brain.is_active(Activity::Idle) {
                    return;
                }
                if only_if_they_see_the_player
                    && !utils::can_see(&visibility_of(brain, &world, piglin), player_arc.as_ref())
                {
                    return;
                }
                if universal_anger {
                    Self::set_anger_target_to_nearest_targetable_player_if_found(
                        brain,
                        &world,
                        piglin,
                        &player_arc,
                    );
                } else {
                    Self::set_anger_target(brain, &world, piglin, &player_arc);
                }
            });
        }
    }

    // piglin: PiglinAi.wasHurtBy
    pub fn was_hurt_by(
        piglin: &PiglinEntity,
        state: &mut BrainState,
        world: &Arc<World>,
        attacker: &Arc<dyn EntityBase>,
    ) {
        if attacker.get_entity().entity_type == &EntityType::PIGLIN {
            return;
        }
        if !piglin.off_hand_item().is_empty() {
            Self::stop_holding_off_hand_item(piglin, state, false);
        }
        state.erase(types::CELEBRATE_LOCATION.id());
        state.erase(types::DANCING.id());
        state.erase(types::ADMIRING_ITEM.id());
        if attacker.get_entity().entity_type == &EntityType::PLAYER {
            state.set_with_expiry(
                types::ADMIRING_DISABLED,
                true,
                Self::HIT_BY_PLAYER_MEMORY_TIMEOUT,
            );
        }
        if state.get(types::AVOID_TARGET).is_some_and(|avoid| {
            avoid.get_entity().entity_type != attacker.get_entity().entity_type
        }) {
            state.erase(types::AVOID_TARGET.id());
        }
        if piglin.is_baby() {
            state.set_with_expiry(
                types::AVOID_TARGET,
                attacker.clone(),
                Self::BABY_FLEE_DURATION,
            );
            let attackable = is_entity_attackable_ignoring_line_of_sight(
                &visibility_of(state, world, piglin),
                attacker.as_ref(),
            );
            if attackable {
                Self::broadcast_anger_target(state, world, attacker);
            }
        } else if attacker.get_entity().entity_type == &EntityType::HOGLIN
            && Self::hoglins_outnumber_piglins(state)
        {
            Self::set_avoid_target_and_dont_hunt_for_a_while(state, piglin, attacker);
            Self::broadcast_retreat(state, attacker);
        } else {
            Self::maybe_retaliate(state, world, piglin, attacker);
        }
    }

    // piglin: PiglinAi.maybeRetaliate
    fn maybe_retaliate(
        state: &mut BrainState,
        world: &Arc<World>,
        piglin: &PiglinEntity,
        attacker: &Arc<dyn EntityBase>,
    ) {
        if state.is_active(Activity::Avoid) {
            return;
        }
        if !is_entity_attackable_ignoring_line_of_sight(
            &visibility_of(state, world, piglin),
            attacker.as_ref(),
        ) {
            return;
        }
        if utils::is_other_target_much_further_away_than_current_attack_target(
            state,
            piglin,
            attacker.as_ref(),
            4.0,
        ) {
            return;
        }
        if attacker.get_entity().entity_type == &EntityType::PLAYER
            && world.level_info.load().game_rules.universal_anger
        {
            Self::set_anger_target_to_nearest_targetable_player_if_found(
                state, world, piglin, attacker,
            );
            Self::broadcast_universal_anger(state, world);
        } else {
            Self::set_anger_target(state, world, piglin, attacker);
            Self::broadcast_anger_target(state, world, attacker);
        }
    }

    // piglin: PiglinAi.setAngerTarget
    pub fn set_anger_target(
        state: &mut BrainState,
        world: &Arc<World>,
        mob: &dyn Mob,
        target: &Arc<dyn EntityBase>,
    ) {
        if !is_entity_attackable_ignoring_line_of_sight(
            &visibility_of(state, world, mob),
            target.as_ref(),
        ) {
            return;
        }
        state.erase(types::CANT_REACH_WALK_TARGET_SINCE.id());
        state.set_with_expiry(
            types::ANGRY_AT,
            target.get_entity().entity_uuid,
            Self::ANGER_DURATION,
        );
        let can_hunt = piglin_of(mob).is_some_and(PiglinEntity::can_hunt);
        if target.get_entity().entity_type == &EntityType::HOGLIN && can_hunt {
            Self::dont_kill_any_more_hoglins_for_a_while(state, mob);
        }
        if target.get_entity().entity_type == &EntityType::PLAYER
            && world.level_info.load().game_rules.universal_anger
        {
            state.set_with_expiry(types::UNIVERSAL_ANGER, true, Self::ANGER_DURATION);
        }
    }

    // piglin: PiglinAi.setAngerTargetToNearestTargetablePlayerIfFound
    fn set_anger_target_to_nearest_targetable_player_if_found(
        state: &mut BrainState,
        world: &Arc<World>,
        mob: &dyn Mob,
        target_if_no_player_found: &Arc<dyn EntityBase>,
    ) {
        let nearest = state
            .get(types::NEAREST_VISIBLE_ATTACKABLE_PLAYER)
            .map(|player| Arc::clone(player) as Arc<dyn EntityBase>);
        match nearest {
            Some(player) => Self::set_anger_target(state, world, mob, &player),
            None => Self::set_anger_target(state, world, mob, target_if_no_player_found),
        }
    }

    // piglin: PiglinAi.setAngerTargetIfCloserThanCurrent
    fn set_anger_target_if_closer_than_current(
        state: &mut BrainState,
        world: &Arc<World>,
        mob: &dyn Mob,
        new_target: &Arc<dyn EntityBase>,
    ) {
        let current = utils::get_living_entity_from_uuid_memory(state, world, types::ANGRY_AT);
        let nearest = utils::get_nearest_target(mob, current.clone(), new_target.clone());
        if current
            .is_none_or(|current| current.get_entity().entity_id != nearest.get_entity().entity_id)
        {
            Self::set_anger_target(state, world, mob, &nearest);
        }
    }

    // piglin: PiglinAi.broadcastAngerTarget
    fn broadcast_anger_target(
        state: &BrainState,
        world: &Arc<World>,
        target: &Arc<dyn EntityBase>,
    ) {
        let adults = state
            .get(types::NEARBY_ADULT_PIGLINS)
            .cloned()
            .unwrap_or_default();
        let target_is_hoglin = target.get_entity().entity_type == &EntityType::HOGLIN;
        let hoglin_huntable = target.get_mob().is_none_or(Mob::can_be_hunted);
        for other in adults {
            if other.get_entity().entity_type == &EntityType::PIGLIN_BRUTE {
                if let Some(mob) = other.get_mob() {
                    mob.set_mob_target(
                        Some(target.clone()),
                        crate::plugin::api::events::entity::entity_target::TargetReason::Angry,
                    );
                }
                continue;
            }
            with_piglin_brain(&other, |piglin, brain| {
                if target_is_hoglin && (!piglin.can_hunt() || !hoglin_huntable) {
                    return;
                }
                Self::set_anger_target_if_closer_than_current(brain, world, piglin, target);
            });
        }
    }

    // piglin: PiglinAi.broadcastUniversalAnger
    fn broadcast_universal_anger(state: &BrainState, world: &Arc<World>) {
        let adults = state
            .get(types::NEARBY_ADULT_PIGLINS)
            .cloned()
            .unwrap_or_default();
        for other in adults {
            with_piglin_brain(&other, |piglin, brain| {
                let player = brain
                    .get(types::NEAREST_VISIBLE_ATTACKABLE_PLAYER)
                    .map(|player| Arc::clone(player) as Arc<dyn EntityBase>);
                if let Some(player) = player {
                    Self::set_anger_target(brain, world, piglin, &player);
                }
            });
        }
    }

    // piglin: PiglinAi.broadcastRetreat
    fn broadcast_retreat(state: &BrainState, target: &Arc<dyn EntityBase>) {
        let visible = state
            .get(types::NEAREST_VISIBLE_ADULT_PIGLINS)
            .cloned()
            .unwrap_or_default();
        for other in visible {
            with_piglin_brain(&other, |piglin, brain| {
                Self::retreat_from_nearest_target(brain, piglin, target);
            });
        }
    }

    // piglin: PiglinAi.retreatFromNearestTarget
    fn retreat_from_nearest_target(
        state: &mut BrainState,
        piglin: &PiglinEntity,
        new_avoid_target: &Arc<dyn EntityBase>,
    ) {
        let avoid = state.get(types::AVOID_TARGET).cloned();
        let mut nearest = utils::get_nearest_target(piglin, avoid, new_avoid_target.clone());
        let attack = state.get(types::ATTACK_TARGET).cloned();
        nearest = utils::get_nearest_target(piglin, attack, nearest);
        Self::set_avoid_target_and_dont_hunt_for_a_while(state, piglin, &nearest);
    }

    // piglin: PiglinAi.wantsToStopFleeing
    fn wants_to_stop_fleeing(state: &BrainState, _mob: &dyn Mob) -> bool {
        let Some(avoided) = state.get(types::AVOID_TARGET) else {
            return true;
        };
        let avoided_type = avoided.get_entity().entity_type;
        if avoided_type == &EntityType::HOGLIN {
            return !Self::hoglins_outnumber_piglins(state);
        }
        if Self::is_zombified(avoided_type) {
            return !state.is_entity_value(types::NEAREST_VISIBLE_ZOMBIFIED, avoided.as_ref());
        }
        false
    }

    // piglin: PiglinAi.hoglinsOutnumberPiglins
    fn hoglins_outnumber_piglins(state: &BrainState) -> bool {
        let piglins = state
            .get(types::VISIBLE_ADULT_PIGLIN_COUNT)
            .copied()
            .unwrap_or(0)
            + 1;
        let hoglins = state
            .get(types::VISIBLE_ADULT_HOGLIN_COUNT)
            .copied()
            .unwrap_or(0);
        hoglins > piglins
    }

    // piglin: PiglinAi.setAvoidTargetAndDontHuntForAWhile
    fn set_avoid_target_and_dont_hunt_for_a_while(
        state: &mut BrainState,
        mob: &dyn Mob,
        target: &Arc<dyn EntityBase>,
    ) {
        state.erase(types::ANGRY_AT.id());
        state.erase(types::ATTACK_TARGET.id());
        state.erase(types::WALK_TARGET.id());
        let duration = mob
            .get_random()
            .random_range(Self::RETREAT_DURATION.0..=Self::RETREAT_DURATION.1);
        state.set_with_expiry(types::AVOID_TARGET, target.clone(), i64::from(duration));
        Self::dont_kill_any_more_hoglins_for_a_while(state, mob);
    }

    // piglin: PiglinAi.dontKillAnyMoreHoglinsForAWhile
    pub fn dont_kill_any_more_hoglins_for_a_while(state: &mut BrainState, mob: &dyn Mob) {
        let delay = mob
            .get_random()
            .random_range(Self::MIN_TIME_BETWEEN_HUNTS..=Self::MAX_TIME_BETWEEN_HUNTS);
        state.set_with_expiry(types::HUNTED_RECENTLY, true, i64::from(delay));
    }

    // piglin: PiglinAi.admireGoldItem
    pub fn admire_gold_item(state: &mut BrainState) {
        state.set_with_expiry(types::ADMIRING_ITEM, true, Self::ADMIRE_DURATION);
    }

    fn has_eaten_recently(state: &BrainState) -> bool {
        state.has_memory_value(types::ATE_RECENTLY.id())
    }

    // piglin: PiglinAi.canAdmire
    #[must_use]
    pub fn can_admire(piglin: &PiglinEntity, state: &BrainState, stack: &ItemStack) -> bool {
        !state.has_memory_value(types::ADMIRING_DISABLED.id())
            && !state.has_memory_value(types::ADMIRING_ITEM.id())
            && piglin.is_adult()
            && Self::is_barter_currency(stack)
    }

    #[must_use]
    pub fn is_not_holding_loved_item_in_off_hand(mob: &dyn Mob) -> bool {
        let off_hand = off_hand_of(mob);
        off_hand.is_empty() || !Self::is_loved_item(&off_hand)
    }

    // piglin: PiglinAi.stopWalking
    fn stop_walking(piglin: &PiglinEntity) {
        piglin.mob_entity.with_brain(piglin, |brain| {
            brain.erase(types::WALK_TARGET.id());
        });
        piglin.mob_entity.steered().navigator().stop();
    }

    fn is_entity_player_holding_loved_item(entity: &dyn EntityBase) -> bool {
        entity
            .get_player()
            .is_some_and(Self::is_player_holding_loved_item)
    }

    fn has_crossbow(mob: &dyn Mob) -> bool {
        holds_item(mob, |stack| stack.item.id == Item::CROSSBOW.id)
    }

    // piglin: Mob.canUseNonMeleeWeapon via isHoldingUsableNonMeleeWeapon
    fn is_holding_usable_non_melee_weapon(mob: &dyn Mob) -> bool {
        holds_item(mob, |stack| {
            stack.item.id == Item::CROSSBOW.id
                || stack.get_data_component::<KineticWeaponImpl>().is_some()
        })
    }

    // piglin: PiglinAi.getSoundForCurrentActivity
    #[must_use]
    pub fn get_sound_for_current_activity(
        piglin: &PiglinEntity,
        state: &BrainState,
    ) -> Option<Sound> {
        let activity = state.get_active_non_core_activity()?;
        Some(Self::get_sound_for_activity(piglin, state, activity))
    }

    fn get_sound_for_activity(
        piglin: &PiglinEntity,
        state: &BrainState,
        activity: Activity,
    ) -> Sound {
        if activity == Activity::Fight {
            return Sound::EntityPiglinAngry;
        }
        let world = piglin.get_entity().world.load();
        if piglin.is_converting(&world) {
            return Sound::EntityPiglinRetreat;
        }
        if activity == Activity::Avoid
            && state.get(types::AVOID_TARGET).is_some_and(|avoid| {
                piglin
                    .get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&avoid.get_entity().pos.load())
                    < 144.0
            })
        {
            return Sound::EntityPiglinRetreat;
        }
        if activity == Activity::AdmireItem {
            return Sound::EntityPiglinAdmiringItem;
        }
        if activity == Activity::Celebrate {
            return Sound::EntityPiglinCelebrate;
        }
        if state.has_memory_value(types::NEAREST_PLAYER_HOLDING_WANTED_ITEM.id()) {
            return Sound::EntityPiglinJealous;
        }
        if state.has_memory_value(types::NEAREST_REPELLENT.id()) {
            Sound::EntityPiglinRetreat
        } else {
            Sound::EntityPiglinAmbient
        }
    }

    // piglin: BehaviorUtils.throwItem items towards the nearest visible player or a random spot
    fn throw_items(piglin: &PiglinEntity, state: &BrainState, items: Vec<ItemStack>) {
        match state.get(types::NEAREST_VISIBLE_PLAYER) {
            Some(player) => {
                Self::throw_items_toward_pos(piglin, items, player.get_entity().pos.load());
            }
            None => Self::throw_items_toward_random_pos(piglin, items),
        }
    }

    pub fn throw_items_toward_random_pos(piglin: &PiglinEntity, items: Vec<ItemStack>) {
        Self::throw_items_toward_pos(piglin, items, Self::random_nearby_pos(piglin));
    }

    fn throw_items_toward_pos(piglin: &PiglinEntity, items: Vec<ItemStack>, target: Vector3<f64>) {
        if items.is_empty() {
            return;
        }
        let living = &piglin.mob_entity.living_entity;
        living.swing_off_hand();

        let entity = &living.entity;
        let world = entity.world.load();
        let pos = entity.pos.load();
        let hand_pos = Vector3::new(
            pos.x,
            entity.get_eye_y() - Self::THROW_HAND_Y_DISTANCE_FROM_EYE,
            pos.z,
        );
        let direction =
            Vector3::new(target.x - pos.x, target.y + 1.0 - pos.y, target.z - pos.z).normalize();
        let velocity = Vector3::new(
            direction.x * Self::THROW_SPEED,
            direction.y * Self::THROW_SPEED,
            direction.z * Self::THROW_SPEED,
        );
        for item in items {
            if item.is_empty() {
                continue;
            }
            let item_entity = ItemEntity::new_with_velocity(
                Entity::new(world.clone(), hand_pos, &EntityType::ITEM),
                item,
                velocity,
                ItemEntity::DEFAULT_PICKUP_DELAY,
            );
            world.spawn_entity(Arc::new(item_entity));
        }
    }

    fn random_nearby_pos(piglin: &PiglinEntity) -> Vector3<f64> {
        land_random_pos(
            piglin,
            Self::RANDOM_THROW_POS_RANGE_HORIZONTAL,
            Self::RANDOM_THROW_POS_RANGE_VERTICAL,
        )
        .unwrap_or_else(|| piglin.get_entity().pos.load())
    }
}

fn piglin_of(mob: &dyn Mob) -> Option<&PiglinEntity> {
    mob.cast_any().downcast_ref::<PiglinEntity>()
}

fn visibility_of<'a>(
    state: &'a BrainState,
    world: &'a Arc<World>,
    mob: &'a dyn Mob,
) -> VisibilityContext<'a> {
    VisibilityContext {
        state,
        world,
        mob,
        follow_range: mob
            .get_mob_entity()
            .living_entity
            .get_attribute_value(&Attributes::FOLLOW_RANGE),
    }
}

fn with_piglin_brain<R>(
    entity: &Arc<dyn EntityBase>,
    f: impl FnOnce(&PiglinEntity, &mut Brain) -> R,
) -> Option<R> {
    let mob = entity.get_mob()?;
    let piglin = piglin_of(mob)?;
    piglin
        .mob_entity
        .try_with_brain(mob, |brain| f(piglin, brain))
}

fn item_stack_of(item: &ItemEntity) -> ItemStack {
    item.get_item_stack()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn off_hand_of(mob: &dyn Mob) -> ItemStack {
    mob.get_mob_entity()
        .living_entity
        .entity_equipment
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&EquipmentSlot::OFF_HAND)
}

fn holds_item(mob: &dyn Mob, predicate: impl Fn(&ItemStack) -> bool) -> bool {
    let equipment = mob
        .get_mob_entity()
        .living_entity
        .entity_equipment
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    predicate(&equipment.get(&EquipmentSlot::MAIN_HAND))
        || predicate(&equipment.get(&EquipmentSlot::OFF_HAND))
}
