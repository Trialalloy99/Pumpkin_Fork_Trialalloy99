//! Ported from `Villager.BRAIN_PROVIDER` and `VillagerGoalPackages`.

use pumpkin_data::entity::{EntityType, MobCategory};
use pumpkin_data::environment_attribute::Activity;

use super::behaviors::{
    acquirable_job_site, acquire_poi, assign_profession_from_job_site, go_to_closest_village,
    go_to_potential_job_site, inside_brownian_walk, locate_hiding_place,
    look_and_follow_trading_player_sink, move_to_sky_seeing_spot, poi_competitor_scan,
    react_to_bell, reset_profession, reset_raid_status, ring_bell, set_closest_home_as_walk_target,
    set_hidden_state, set_raid_status, set_walk_target_from_block_memory, sleep_in_bed,
    socialize_at_bell, stroll_around_poi, stroll_to_poi, stroll_to_poi_list,
    update_activity_from_schedule, validate_nearby_poi, village_bound_random_stroll,
    villager_calm_down, villager_panic_trigger, wake_up, yield_job_site,
};
use super::routines::{
    celebrate_villagers_survived_raid, give_gift_to_hero, harvest_farmland, jump_on_bed,
    play_tag_with_other_kids, show_trades_to_player, trade_with_villager, use_bonemeal,
    villager_make_love, work_at_poi,
};
use super::{VillagerEntity, VillagerProfession};
use crate::entity::EntityBase;
use crate::entity::ai::brain::activity::ActivityData;
use crate::entity::ai::brain::behavior::{
    BehaviorControl, DoNothing, GateBehavior, LookAtTargetSink, MoveToTargetSink, OrderPolicy,
    RunningPolicy, Swim, Timed, go_to_wanted_item, interact_with, interact_with_door, run_one,
    run_one_with_conditions, set_entity_look_target, set_look_and_interact,
    set_walk_target_away_from, set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, PackedMemories, types};
use crate::entity::ai::brain::sensing::SensorType;
use crate::entity::ai::brain::{Brain, BrainProvider};
use crate::entity::mob::Mob;

const SPEED_MODIFIER: f32 = 0.5;
const STROLL_SPEED_MODIFIER: f32 = 0.4;

const MEMORIES: [MemoryModuleId; 28] = [
    types::HOME.id(),
    types::JOB_SITE.id(),
    types::POTENTIAL_JOB_SITE.id(),
    types::MEETING_POINT.id(),
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::VISIBLE_VILLAGER_BABIES.id(),
    types::NEAREST_PLAYERS.id(),
    types::NEAREST_VISIBLE_PLAYER.id(),
    types::NEAREST_VISIBLE_ATTACKABLE_PLAYER.id(),
    types::NEAREST_VISIBLE_WANTED_ITEM.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::INTERACTION_TARGET.id(),
    types::BREED_TARGET.id(),
    types::PATH.id(),
    types::DOORS_TO_CLOSE.id(),
    types::NEAREST_BED.id(),
    types::HURT_BY.id(),
    types::HURT_BY_ENTITY.id(),
    types::NEAREST_HOSTILE.id(),
    types::SECONDARY_JOB_SITE.id(),
    types::HIDING_PLACE.id(),
    types::HEARD_BELL_TIME.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
    types::GOLEM_DETECTED_RECENTLY.id(),
    types::LAST_SLEPT.id(),
    types::LAST_WOKEN.id(),
];

const SENSORS: [SensorType; 9] = [
    SensorType::NearestLivingEntities,
    SensorType::NearestPlayers,
    SensorType::NearestItems,
    SensorType::NearestBed,
    SensorType::HurtBy,
    SensorType::VillagerHostiles,
    SensorType::VillagerBabies,
    SensorType::SecondaryPois,
    SensorType::GolemDetected,
];

pub(super) fn make_brain(villager: &VillagerEntity, packed: &PackedMemories) -> Brain {
    let mut brain = BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &SENSORS,
        activities,
    }
    .make_brain(villager, packed);
    // villager: registerBrainGoals, the schedule follows the age
    brain.set_schedule(villager.is_baby_villager());
    let world = villager.get_entity().world.load();
    let pos = villager.get_entity().block_pos.load();
    brain.update_activity_from_schedule(&world, &pos);
    brain
}

type Pair = (Box<dyn BehaviorControl>, i32);

fn boxed(behavior: impl BehaviorControl + 'static) -> Box<dyn BehaviorControl> {
    Box::new(behavior)
}

fn weighted(behavior: impl BehaviorControl + 'static, weight: i32) -> Pair {
    (boxed(behavior), weight)
}

fn is_cat(entity: &dyn EntityBase) -> bool {
    entity.get_entity().entity_type == &EntityType::CAT
}

macro_rules! category_filter {
    ($name:ident, $category:ident) => {
        fn $name(entity: &dyn EntityBase) -> bool {
            entity.get_entity().entity_type.category == &MobCategory::$category
        }
    };
}
category_filter!(is_creature, CREATURE);
category_filter!(is_water_creature, WATER_CREATURE);
category_filter!(is_axolotl, AXOLOTLS);
category_filter!(is_underground_water_creature, UNDERGROUND_WATER_CREATURE);
category_filter!(is_water_ambient, WATER_AMBIENT);
category_filter!(is_monster, MONSTER);

fn is_villager(entity: &dyn EntityBase) -> bool {
    entity.get_entity().entity_type == &EntityType::VILLAGER
}

fn is_player(entity: &dyn EntityBase) -> bool {
    entity.get_entity().entity_type == &EntityType::PLAYER
}

// packages: getFullLookBehavior
fn full_look_behavior() -> (i32, Box<dyn BehaviorControl>) {
    (
        5,
        boxed(run_one(vec![
            weighted(set_entity_look_target::create_with(is_cat, 8.0), 8),
            weighted(set_entity_look_target::create_with(is_villager, 8.0), 2),
            weighted(set_entity_look_target::create_with(is_player, 8.0), 2),
            weighted(set_entity_look_target::create_with(is_creature, 8.0), 1),
            weighted(
                set_entity_look_target::create_with(is_water_creature, 8.0),
                1,
            ),
            weighted(set_entity_look_target::create_with(is_axolotl, 8.0), 1),
            weighted(
                set_entity_look_target::create_with(is_underground_water_creature, 8.0),
                1,
            ),
            weighted(
                set_entity_look_target::create_with(is_water_ambient, 8.0),
                1,
            ),
            weighted(set_entity_look_target::create_with(is_monster, 8.0), 1),
            weighted(DoNothing::new(30, 60), 2),
        ])),
    )
}

// packages: getMinimalLookBehavior
fn minimal_look_behavior() -> (i32, Box<dyn BehaviorControl>) {
    (
        5,
        boxed(run_one(vec![
            weighted(set_entity_look_target::create_with(is_villager, 8.0), 2),
            weighted(set_entity_look_target::create_with(is_player, 8.0), 2),
            weighted(DoNothing::new(30, 60), 8),
        ])),
    )
}

fn validate_bed_poi(
    world: &crate::world::World,
    pos: &pumpkin_util::math::position::BlockPos,
) -> bool {
    use pumpkin_data::block_properties::WhiteBedLikeProperties;
    use pumpkin_data::tag::{self, Taggable};
    let (block, state) = world.get_block_and_state(pos);
    block.has_tag(&tag::Block::MINECRAFT_VILLAGERS_CAN_SLEEP_ON_BED)
        && !WhiteBedLikeProperties::from_state_id(state.id).occupied
}

const fn always_valid(
    _world: &crate::world::World,
    _pos: &pumpkin_util::math::position::BlockPos,
) -> bool {
    true
}

// packages: getCorePackage
fn core_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (0, boxed(Timed::new(Swim::new(0.8)))),
        (0, boxed(interact_with_door::create())),
        (0, boxed(Timed::new(LookAtTargetSink::new(45, 90)))),
        (0, boxed(villager_panic_trigger())),
        (0, boxed(wake_up())),
        (0, boxed(react_to_bell())),
        (0, boxed(set_raid_status())),
        (0, boxed(validate_nearby_poi(types::JOB_SITE))),
        (0, boxed(validate_nearby_poi(types::POTENTIAL_JOB_SITE))),
        (1, boxed(Timed::new(MoveToTargetSink::new(150, 250)))),
        (2, boxed(poi_competitor_scan())),
        (
            3,
            boxed(look_and_follow_trading_player_sink(speed_modifier)),
        ),
        (
            5,
            boxed(go_to_wanted_item::create(speed_modifier, false, 4)),
        ),
        (
            6,
            boxed(acquire_poi(
                |villager, poi_type| acquirable_job_site(villager.profession(), poi_type),
                types::JOB_SITE,
                types::POTENTIAL_JOB_SITE,
                true,
                false,
                always_valid,
            )),
        ),
        (7, boxed(go_to_potential_job_site(speed_modifier))),
        (8, boxed(yield_job_site(speed_modifier))),
        (
            10,
            boxed(acquire_poi(
                |_, poi_type| poi_type == "minecraft:home",
                types::HOME,
                types::HOME,
                false,
                true,
                validate_bed_poi,
            )),
        ),
        (
            10,
            boxed(acquire_poi(
                |_, poi_type| poi_type == "minecraft:meeting",
                types::MEETING_POINT,
                types::MEETING_POINT,
                true,
                true,
                always_valid,
            )),
        ),
        (10, boxed(assign_profession_from_job_site())),
        (10, boxed(reset_profession())),
    ]
}

// packages: getWorkPackage
fn work_package(
    profession: VillagerProfession,
    speed_modifier: f32,
) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    let farmer = profession == VillagerProfession::Farmer;
    vec![
        minimal_look_behavior(),
        (
            5,
            boxed(run_one(vec![
                weighted(work_at_poi(farmer), 7),
                weighted(
                    stroll_around_poi(types::JOB_SITE, STROLL_SPEED_MODIFIER, 4),
                    2,
                ),
                weighted(
                    stroll_to_poi(types::JOB_SITE, STROLL_SPEED_MODIFIER, 1, 10),
                    5,
                ),
                weighted(
                    stroll_to_poi_list(
                        types::SECONDARY_JOB_SITE,
                        speed_modifier,
                        1,
                        6,
                        types::JOB_SITE,
                    ),
                    5,
                ),
                weighted(harvest_farmland(), if farmer { 2 } else { 5 }),
                weighted(use_bonemeal(), if farmer { 4 } else { 7 }),
            ])),
        ),
        (10, boxed(show_trades_to_player(400, 1600))),
        (
            10,
            boxed(set_look_and_interact::create(&EntityType::PLAYER, 4)),
        ),
        (
            2,
            boxed(set_walk_target_from_block_memory(
                types::JOB_SITE,
                speed_modifier,
                9,
                100,
                1200,
            )),
        ),
        (3, boxed(give_gift_to_hero(100))),
        (99, boxed(update_activity_from_schedule())),
    ]
}

// packages: getPlayPackage
fn play_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (0, boxed(Timed::new(MoveToTargetSink::new(80, 120)))),
        full_look_behavior(),
        (5, boxed(play_tag_with_other_kids())),
        (
            5,
            boxed(run_one_with_conditions(
                vec![(
                    types::VISIBLE_VILLAGER_BABIES.id(),
                    MemoryStatus::ValueAbsent,
                )],
                vec![
                    weighted(
                        interact_with::of(
                            &EntityType::VILLAGER,
                            8,
                            types::INTERACTION_TARGET,
                            speed_modifier,
                            2,
                        ),
                        2,
                    ),
                    weighted(
                        interact_with::of(
                            &EntityType::CAT,
                            8,
                            types::INTERACTION_TARGET,
                            speed_modifier,
                            2,
                        ),
                        1,
                    ),
                    weighted(village_bound_random_stroll(speed_modifier, 10, 7), 1),
                    weighted(
                        set_walk_target_from_look_target::create(speed_modifier, 2),
                        1,
                    ),
                    weighted(jump_on_bed(speed_modifier), 2),
                    weighted(DoNothing::new(20, 40), 2),
                ],
            )),
        ),
        (99, boxed(update_activity_from_schedule())),
    ]
}

// packages: getRestPackage
fn rest_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (
            2,
            boxed(set_walk_target_from_block_memory(
                types::HOME,
                speed_modifier,
                1,
                150,
                1200,
            )),
        ),
        (3, boxed(validate_nearby_poi(types::HOME))),
        (3, boxed(sleep_in_bed())),
        (
            5,
            boxed(run_one_with_conditions(
                vec![(types::HOME.id(), MemoryStatus::ValueAbsent)],
                vec![
                    weighted(set_closest_home_as_walk_target(speed_modifier), 1),
                    weighted(inside_brownian_walk(speed_modifier), 4),
                    weighted(go_to_closest_village(speed_modifier, 4), 2),
                    weighted(DoNothing::new(20, 40), 2),
                ],
            )),
        ),
        minimal_look_behavior(),
        (99, boxed(update_activity_from_schedule())),
    ]
}

fn single_gate(memory: MemoryModuleId, behavior: impl BehaviorControl + 'static) -> GateBehavior {
    GateBehavior::new(
        "Gate",
        Vec::new(),
        vec![memory],
        OrderPolicy::Ordered,
        RunningPolicy::RunOne,
        vec![weighted(behavior, 1)],
    )
}

// packages: getMeetPackage
fn meet_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (
            2,
            boxed(GateBehavior::new(
                "TriggerGate",
                Vec::new(),
                Vec::new(),
                OrderPolicy::Shuffled,
                RunningPolicy::RunOne,
                vec![
                    weighted(
                        stroll_around_poi(types::MEETING_POINT, STROLL_SPEED_MODIFIER, 40),
                        2,
                    ),
                    weighted(socialize_at_bell(), 2),
                ],
            )),
        ),
        (10, boxed(show_trades_to_player(400, 1600))),
        (
            10,
            boxed(set_look_and_interact::create(&EntityType::PLAYER, 4)),
        ),
        (
            2,
            boxed(set_walk_target_from_block_memory(
                types::MEETING_POINT,
                speed_modifier,
                6,
                100,
                200,
            )),
        ),
        (3, boxed(give_gift_to_hero(100))),
        (3, boxed(validate_nearby_poi(types::MEETING_POINT))),
        (
            3,
            boxed(single_gate(
                types::INTERACTION_TARGET.id(),
                trade_with_villager(),
            )),
        ),
        full_look_behavior(),
        (99, boxed(update_activity_from_schedule())),
    ]
}

fn can_breed_villager(entity: &dyn EntityBase) -> bool {
    entity
        .cast_any()
        .downcast_ref::<VillagerEntity>()
        .is_some_and(VillagerEntity::can_breed_villager)
}

// packages: getIdlePackage
fn idle_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (
            2,
            boxed(run_one(vec![
                weighted(
                    interact_with::of(
                        &EntityType::VILLAGER,
                        8,
                        types::INTERACTION_TARGET,
                        speed_modifier,
                        2,
                    ),
                    2,
                ),
                weighted(
                    interact_with::of_with_predicates(
                        &EntityType::VILLAGER,
                        8,
                        can_breed_villager,
                        can_breed_villager,
                        types::BREED_TARGET,
                        speed_modifier,
                        2,
                    ),
                    1,
                ),
                weighted(
                    interact_with::of(
                        &EntityType::CAT,
                        8,
                        types::INTERACTION_TARGET,
                        speed_modifier,
                        2,
                    ),
                    1,
                ),
                weighted(village_bound_random_stroll(speed_modifier, 10, 7), 1),
                weighted(
                    set_walk_target_from_look_target::create(speed_modifier, 2),
                    1,
                ),
                weighted(jump_on_bed(speed_modifier), 1),
                weighted(DoNothing::new(30, 60), 1),
            ])),
        ),
        (3, boxed(give_gift_to_hero(100))),
        (
            3,
            boxed(set_look_and_interact::create(&EntityType::PLAYER, 4)),
        ),
        (3, boxed(show_trades_to_player(400, 1600))),
        (
            3,
            boxed(single_gate(
                types::INTERACTION_TARGET.id(),
                trade_with_villager(),
            )),
        ),
        (
            3,
            boxed(single_gate(types::BREED_TARGET.id(), villager_make_love())),
        ),
        full_look_behavior(),
        (99, boxed(update_activity_from_schedule())),
    ]
}

// packages: getPanicPackage
fn panic_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    let runaway_speed = speed_modifier * 1.5;
    vec![
        (0, boxed(villager_calm_down())),
        (
            1,
            boxed(set_walk_target_away_from::entity(
                types::NEAREST_HOSTILE,
                runaway_speed,
                6,
                false,
            )),
        ),
        (
            1,
            boxed(set_walk_target_away_from::entity(
                types::HURT_BY_ENTITY,
                runaway_speed,
                6,
                false,
            )),
        ),
        (3, boxed(village_bound_random_stroll(runaway_speed, 2, 2))),
        minimal_look_behavior(),
    ]
}

// packages: getPreRaidPackage
fn pre_raid_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (0, boxed(ring_bell())),
        (
            0,
            boxed(GateBehavior::new(
                "TriggerGate",
                Vec::new(),
                Vec::new(),
                OrderPolicy::Shuffled,
                RunningPolicy::RunOne,
                vec![
                    weighted(
                        set_walk_target_from_block_memory(
                            types::MEETING_POINT,
                            speed_modifier * 1.5,
                            2,
                            150,
                            200,
                        ),
                        6,
                    ),
                    weighted(village_bound_random_stroll(speed_modifier * 1.5, 10, 7), 2),
                ],
            )),
        ),
        minimal_look_behavior(),
        (99, boxed(reset_raid_status())),
    ]
}

fn raid_exists_and_not_victory(tick: &crate::entity::ai::brain::BrainTick<'_>) -> bool {
    let pos = tick.mob.get_entity().block_pos.load();
    tick.world
        .raid_state_at(&pos)
        .is_some_and(|raid| raid.victory)
}

fn raid_exists_and_active(tick: &crate::entity::ai::brain::BrainTick<'_>) -> bool {
    let pos = tick.mob.get_entity().block_pos.load();
    tick.world
        .raid_state_at(&pos)
        .is_some_and(|raid| raid.active && !raid.victory && !raid.loss)
}

// packages: getRaidPackage
fn raid_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (
            0,
            boxed(GateBehavior::new(
                "TriggerGate",
                Vec::new(),
                Vec::new(),
                OrderPolicy::Shuffled,
                RunningPolicy::RunOne,
                vec![
                    weighted(
                        move_to_sky_seeing_spot(speed_modifier).when(raid_exists_and_not_victory),
                        5,
                    ),
                    weighted(
                        village_bound_random_stroll(speed_modifier * 1.1, 10, 7)
                            .when(raid_exists_and_not_victory),
                        2,
                    ),
                ],
            )),
        ),
        (0, boxed(celebrate_villagers_survived_raid(600, 600))),
        (
            2,
            boxed(locate_hiding_place(24, speed_modifier * 1.4, 1).when(raid_exists_and_active)),
        ),
        minimal_look_behavior(),
        (99, boxed(reset_raid_status())),
    ]
}

// packages: getHidePackage
fn hide_package(speed_modifier: f32) -> Vec<(i32, Box<dyn BehaviorControl>)> {
    vec![
        (0, boxed(set_hidden_state(15, 3))),
        (1, boxed(locate_hiding_place(32, speed_modifier * 1.25, 2))),
        minimal_look_behavior(),
    ]
}

// villager: BRAIN_PROVIDER activities
fn activities(mob: &dyn Mob) -> Vec<ActivityData> {
    let Some(villager) = mob.cast_any().downcast_ref::<VillagerEntity>() else {
        return Vec::new();
    };
    let profession = villager.profession();
    let mut activities = Vec::new();
    if villager.is_baby_villager() {
        activities.push(ActivityData::with_pairs(
            Activity::Play,
            play_package(SPEED_MODIFIER),
        ));
    } else {
        activities.push(ActivityData::with_pairs_and_conditions(
            Activity::Work,
            work_package(profession, SPEED_MODIFIER),
            vec![(types::JOB_SITE.id(), MemoryStatus::ValuePresent)],
        ));
    }
    activities.push(ActivityData::with_pairs(
        Activity::Core,
        core_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs_and_conditions(
        Activity::Meet,
        meet_package(SPEED_MODIFIER),
        vec![(types::MEETING_POINT.id(), MemoryStatus::ValuePresent)],
    ));
    activities.push(ActivityData::with_pairs(
        Activity::Rest,
        rest_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs(
        Activity::Idle,
        idle_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs(
        Activity::Panic,
        panic_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs(
        Activity::PreRaid,
        pre_raid_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs(
        Activity::Raid,
        raid_package(SPEED_MODIFIER),
    ));
    activities.push(ActivityData::with_pairs(
        Activity::Hide,
        hide_package(SPEED_MODIFIER),
    ));
    activities
}
