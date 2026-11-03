//! Ported from the villager brain behaviors in `net.minecraft.world.entity.ai.behavior`:
//! core, point-of-interest, sleep, bell, raid and panic behaviors.

use std::sync::Arc;

use pumpkin_data::block_properties::WhiteBedLikeProperties as BedProperties;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use rand::seq::SliceRandom;
use rustc_hash::FxHashMap;

use super::{VillagerEntity, VillagerProfession};
use crate::entity::EntityBase;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, Timed, utils};
use crate::entity::ai::brain::memory::position_tracker::EntityTracker;
use crate::entity::ai::brain::memory::walk_target::WalkTarget;
use crate::entity::ai::brain::memory::{
    GlobalPos, MemoryModuleId, MemoryModuleType, MemoryStatus, types,
};
use crate::entity::ai::brain::{BrainState, BrainTick};
use crate::entity::ai::util::{default_random_pos, land_random_pos};
use crate::world::World;
use crate::world::poi_manager::PoiOccupancy;
use crate::world::villager_poi::{poi_type_for_profession, profession_for_poi_type};

const POI_SCAN_RANGE: i32 = 48;
const POI_BATCH_SIZE: usize = 5;
const POI_SCAN_RATE: i64 = 20;
const MAX_RETRY_PATHFINDING_INTERVAL: i32 = 400;
const MEETING_VALID_RANGE: i32 = 3;
const DEFAULT_VALID_RANGE: i32 = 1;

pub(super) fn villager<'a>(tick: &BrainTick<'a>) -> Option<&'a VillagerEntity> {
    tick.mob.cast_any().downcast_ref::<VillagerEntity>()
}

pub(super) fn body_pos(tick: &BrainTick<'_>) -> Vector3<f64> {
    tick.mob.get_entity().pos.load()
}

pub(super) fn block_dist_sq(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.0.x - b.0.x);
    let dy = i64::from(a.0.y - b.0.y);
    let dz = i64::from(a.0.z - b.0.z);
    dx * dx + dy * dy + dz * dz
}

// block pos: BlockPos.closerToCenterThan
pub(super) fn closer_to_center_than(pos: BlockPos, other: Vector3<f64>, distance: f64) -> bool {
    pos.to_centered_f64().squared_distance_to_vec(&other) < distance * distance
}

// vanilla: poi type validRange
fn valid_range(poi_type: &str) -> i32 {
    if poi_type == "minecraft:meeting" {
        MEETING_VALID_RANGE
    } else {
        DEFAULT_VALID_RANGE
    }
}

// profession: heldJobSite
pub(super) fn held_job_site(profession: VillagerProfession, poi_type: &str) -> bool {
    poi_type_for_profession(profession) == Some(poi_type)
}

// profession: acquirableJobSite
pub(super) fn acquirable_job_site(profession: VillagerProfession, poi_type: &str) -> bool {
    match profession {
        VillagerProfession::None => profession_for_poi_type(poi_type).is_some(),
        VillagerProfession::Nitwit => false,
        other => poi_type_for_profession(other) == Some(poi_type),
    }
}

fn broadcast(tick: &BrainTick<'_>, status: EntityStatus, bedrock: ActorEventID) {
    let entity = tick.mob.get_entity();
    tick.world.send_entity_status(entity, status, bedrock);
}

// brain: Brain.updateActivityFromSchedule at the mob position
fn update_schedule(tick: &mut BrainTick<'_>) {
    let pos = tick.mob.get_entity().block_pos.load();
    tick.state.update_activity_from_schedule(tick.world, &pos);
}

// behavior: UpdateActivityFromSchedule
pub fn update_activity_from_schedule() -> OneShot {
    OneShot::new("UpdateActivityFromSchedule", Vec::new(), |tick| {
        update_schedule(tick);
        true
    })
}

// behavior: WakeUp
pub fn wake_up() -> OneShot {
    OneShot::new("WakeUp", Vec::new(), |tick| {
        let Some(villager) = villager(tick) else {
            return false;
        };
        if !tick.state.is_active(Activity::Rest) && villager.is_villager_sleeping() {
            villager.stop_sleeping_in(tick.state);
            true
        } else {
            false
        }
    })
}

// behavior: ReactToBell
pub fn react_to_bell() -> OneShot {
    OneShot::new(
        "ReactToBell",
        vec![(types::HEARD_BELL_TIME.id(), MemoryStatus::ValuePresent)],
        |tick| {
            let pos = tick.mob.get_entity().block_pos.load();
            if tick.world.raid_state_at(&pos).is_none() {
                tick.state.set_active_activity_if_possible(Activity::Hide);
            }
            true
        },
    )
}

// behavior: SetRaidStatus
pub fn set_raid_status() -> OneShot {
    OneShot::new("SetRaidStatus", Vec::new(), |tick| {
        if tick.mob.get_random().random_range(0..20) != 0 {
            return false;
        }
        let pos = tick.mob.get_entity().block_pos.load();
        if let Some(raid) = tick.world.raid_state_at(&pos) {
            let activity = if raid.has_first_wave_spawned && !raid.between_waves {
                Activity::Raid
            } else {
                Activity::PreRaid
            };
            tick.state.set_default_activity(activity);
            tick.state.set_active_activity_if_possible(activity);
        }
        true
    })
}

// behavior: ResetRaidStatus
pub fn reset_raid_status() -> OneShot {
    OneShot::new("ResetRaidStatus", Vec::new(), |tick| {
        if tick.mob.get_random().random_range(0..20) != 0 {
            return false;
        }
        let pos = tick.mob.get_entity().block_pos.load();
        let raid = tick.world.raid_state_at(&pos);
        if raid.is_none_or(|raid| raid.stopped || raid.loss) {
            tick.state.set_default_activity(Activity::Idle);
            update_schedule(tick);
        }
        true
    })
}

// behavior: VillagerPanicTrigger
struct VillagerPanicTrigger;

impl VillagerPanicTrigger {
    fn is_hurt(state: &BrainState) -> bool {
        state.has_memory_value(types::HURT_BY.id())
    }

    fn has_hostile(state: &BrainState) -> bool {
        state.has_memory_value(types::NEAREST_HOSTILE.id())
    }
}

impl Behavior for VillagerPanicTrigger {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &[]
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::is_hurt(tick.state) || Self::has_hostile(tick.state)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if Self::is_hurt(tick.state) || Self::has_hostile(tick.state) {
            if !tick.state.is_active(Activity::Panic) {
                tick.state.erase(types::PATH.id());
                tick.state.erase(types::WALK_TARGET.id());
                tick.state.erase(types::LOOK_TARGET.id());
                tick.state.erase(types::BREED_TARGET.id());
                tick.state.erase(types::INTERACTION_TARGET.id());
            }
            tick.state.set_active_activity_if_possible(Activity::Panic);
        }
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if tick.time % 100 == 0
            && let Some(villager) = villager(tick)
        {
            let time = tick.time;
            villager.spawn_golem_if_needed(tick.world, tick.state, time, 3);
        }
    }

    fn min_duration(&self) -> i32 {
        crate::entity::ai::brain::behavior::DEFAULT_DURATION
    }

    fn debug_name(&self) -> &'static str {
        "VillagerPanicTrigger"
    }
}

pub fn villager_panic_trigger() -> Timed<impl Behavior> {
    Timed::new(VillagerPanicTrigger)
}

// behavior: VillagerCalmDown
pub fn villager_calm_down() -> OneShot {
    OneShot::new(
        "VillagerCalmDown",
        vec![
            (types::HURT_BY.id(), MemoryStatus::Registered),
            (types::HURT_BY_ENTITY.id(), MemoryStatus::Registered),
            (types::NEAREST_HOSTILE.id(), MemoryStatus::Registered),
        ],
        |tick| {
            let position = body_pos(tick);
            let feel_scared = tick.state.has_memory_value(types::HURT_BY.id())
                || tick.state.has_memory_value(types::NEAREST_HOSTILE.id())
                || tick.state.get(types::HURT_BY_ENTITY).is_some_and(|entity| {
                    entity
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&position)
                        <= 36.0
                });
            if !feel_scared {
                tick.state.erase(types::HURT_BY.id());
                tick.state.erase(types::HURT_BY_ENTITY.id());
                update_schedule(tick);
            }
            true
        },
    )
}

// behavior: ValidateNearbyPoi
fn bed_is_occupied(world: &World, pos: BlockPos, body_sleeping: bool) -> bool {
    let (block, state) = world.get_block_and_state(&pos);
    block.has_tag(&tag::Block::MINECRAFT_VILLAGERS_CAN_SLEEP_ON_BED)
        && BedProperties::from_state_id(state.id).occupied
        && !body_sleeping
}

fn bed_is_occupied_by_villager(world: &World, pos: BlockPos) -> bool {
    let area = BoundingBox::new(
        pos.0.to_f64(),
        Vector3::new(
            f64::from(pos.0.x) + 1.0,
            f64::from(pos.0.y) + 1.0,
            f64::from(pos.0.z) + 1.0,
        ),
    );
    world.get_entities_at_box(&area).iter().any(|entity| {
        entity
            .cast_any()
            .downcast_ref::<VillagerEntity>()
            .is_some_and(VillagerEntity::is_villager_sleeping)
    })
}

pub fn validate_nearby_poi(memory: MemoryModuleType<GlobalPos>) -> OneShot {
    OneShot::new(
        "ValidateNearbyPoi",
        vec![(memory.id(), MemoryStatus::ValuePresent)],
        move |tick| {
            let Some(global) = tick.state.get(memory).copied() else {
                return false;
            };
            let Some(villager) = villager(tick) else {
                return false;
            };
            if !global.is_in_world(tick.world)
                || !closer_to_center_than(global.pos, body_pos(tick), 16.0)
            {
                return false;
            }
            if !tick.world.poi_exists(&global.pos, |poi| {
                villager.poi_matches_memory(memory.id(), poi)
            }) {
                tick.state.erase(memory.id());
            } else if bed_is_occupied(tick.world, global.pos, villager.is_villager_sleeping()) {
                tick.state.erase(memory.id());
                if !bed_is_occupied_by_villager(tick.world, global.pos) {
                    tick.world.poi_release(&global.pos);
                }
            }
            true
        },
    )
}

// behavior: PoiCompetitorScan
pub fn poi_competitor_scan() -> OneShot {
    OneShot::new(
        "PoiCompetitorScan",
        vec![
            (types::JOB_SITE.id(), MemoryStatus::ValuePresent),
            (
                types::NEAREST_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        |tick| {
            let Some(me) = villager(tick) else {
                return false;
            };
            let Some(job_site) = tick.state.get(types::JOB_SITE).copied() else {
                return false;
            };
            let Some(poi_type) = tick.world.poi_type_at(&job_site.pos) else {
                return true;
            };
            let nearby: Vec<Arc<dyn EntityBase>> = tick
                .state
                .get(types::NEAREST_LIVING_ENTITIES)
                .cloned()
                .unwrap_or_default();
            // winner of the pairwise reduce, None is the body itself
            let mut winner: Option<Arc<dyn EntityBase>> = None;
            let mut winner_xp = me.villager_xp();
            for entity in nearby {
                let Some(other) = entity.cast_any().downcast_ref::<VillagerEntity>() else {
                    continue;
                };
                if entity.get_entity().entity_id == me.get_entity().entity_id
                    || !entity.get_entity().is_alive()
                {
                    continue;
                }
                let competes = other
                    .mob_entity
                    .try_with_brain(other, |brain| {
                        brain.get(types::JOB_SITE).copied() == Some(job_site)
                    })
                    .unwrap_or(false)
                    && held_job_site(other.profession(), &poi_type);
                if !competes {
                    continue;
                }
                let other_xp = other.villager_xp();
                // selectWinner(first, second): first wins only with strictly more xp
                if winner_xp > other_xp {
                    other.mob_entity.try_with_brain(other, |brain| {
                        brain.erase(types::JOB_SITE.id());
                    });
                } else {
                    match &winner {
                        None => tick.state.erase(types::JOB_SITE.id()),
                        Some(previous) => {
                            if let Some(previous) =
                                previous.cast_any().downcast_ref::<VillagerEntity>()
                            {
                                previous.mob_entity.try_with_brain(previous, |brain| {
                                    brain.erase(types::JOB_SITE.id());
                                });
                            }
                        }
                    }
                    winner = Some(entity.clone());
                    winner_xp = other_xp;
                }
            }
            true
        },
    )
}

// behavior: LookAndFollowTradingPlayerSink
struct LookAndFollowTradingPlayerSink {
    speed_modifier: f32,
}

impl LookAndFollowTradingPlayerSink {
    fn trading_player(tick: &BrainTick<'_>) -> Option<Arc<dyn EntityBase>> {
        use crate::entity::mob::Mob;
        let villager = villager(tick)?;
        villager
            .get_trading_player()
            .map(|player| player as Arc<dyn EntityBase>)
    }

    fn usable(tick: &BrainTick<'_>) -> bool {
        let Some(villager) = villager(tick) else {
            return false;
        };
        let Some(player) = Self::trading_player(tick) else {
            return false;
        };
        let entity = tick.mob.get_entity();
        entity.is_alive()
            && !villager.mob_entity.living_entity.is_in_water()
            && !villager.was_hurt_recently()
            && entity
                .pos
                .load()
                .squared_distance_to_vec(&player.get_entity().pos.load())
                <= 16.0
    }

    fn follow_player(&self, tick: &mut BrainTick<'_>) {
        if let Some(player) = Self::trading_player(tick) {
            tick.state.set(
                types::WALK_TARGET,
                WalkTarget::new(
                    Arc::new(EntityTracker::new(player.clone(), false)),
                    self.speed_modifier,
                    2,
                ),
            );
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(EntityTracker::new(player, true))
                    as Arc<dyn crate::entity::ai::brain::memory::position_tracker::PositionTracker>,
            );
        }
    }
}

const TRADING_SINK_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::WALK_TARGET.id(), MemoryStatus::Registered),
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
];

impl Behavior for LookAndFollowTradingPlayerSink {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &TRADING_SINK_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        i32::MAX / 2
    }

    fn max_duration(&self) -> i32 {
        i32::MAX / 2
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::usable(tick)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        Self::usable(tick)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.follow_player(tick);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        self.follow_player(tick);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::LOOK_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "LookAndFollowTradingPlayerSink"
    }
}

pub fn look_and_follow_trading_player_sink(speed_modifier: f32) -> Timed<impl Behavior> {
    Timed::new(LookAndFollowTradingPlayerSink { speed_modifier })
}

// behavior: AcquirePoi.JitteredLinearRetry
struct JitteredLinearRetry {
    previous_attempt: i64,
    next_attempt: i64,
    current_delay: i32,
}

impl JitteredLinearRetry {
    fn new(first_attempt: i64) -> Self {
        let mut retry = Self {
            previous_attempt: 0,
            next_attempt: 0,
            current_delay: 0,
        };
        retry.mark_attempt(first_attempt);
        retry
    }

    fn mark_attempt(&mut self, timestamp: i64) {
        self.previous_attempt = timestamp;
        let suggested = self.current_delay + rand::rng().random_range(0..40) + 40;
        self.current_delay = suggested.min(MAX_RETRY_PATHFINDING_INTERVAL);
        self.next_attempt = timestamp + i64::from(self.current_delay);
    }

    fn is_still_valid(&self, timestamp: i64) -> bool {
        timestamp - self.previous_attempt < i64::from(MAX_RETRY_PATHFINDING_INTERVAL)
    }

    const fn should_retry(&self, timestamp: i64) -> bool {
        timestamp >= self.next_attempt
    }
}

// behavior: AcquirePoi.findPathToPois
pub(super) fn find_path_to_pois(
    tick: &BrainTick<'_>,
    pois: &[(BlockPos, String)],
) -> Option<crate::entity::ai::pathfinder::path::Path> {
    if pois.is_empty() {
        return None;
    }
    let max_range = pois
        .iter()
        .map(|(_, poi_type)| valid_range(poi_type))
        .max()
        .unwrap_or(1);
    let targets: Vec<BlockPos> = pois.iter().map(|(pos, _)| *pos).collect();
    let mob = tick.mob.get_mob_entity();
    mob.navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .create_path_to_targets(&mob.living_entity, &targets, max_range)
}

pub type PoiMatcher = fn(&VillagerEntity, &str) -> bool;
pub type PoiValidator = fn(&World, &BlockPos) -> bool;

// behavior: AcquirePoi.create
pub fn acquire_poi(
    matches: PoiMatcher,
    memory_to_validate: MemoryModuleType<GlobalPos>,
    memory_to_acquire: MemoryModuleType<GlobalPos>,
    only_if_adult: bool,
    announce_acquisition: bool,
    valid_poi: PoiValidator,
) -> OneShot {
    let mut conditions = vec![(memory_to_acquire.id(), MemoryStatus::ValueAbsent)];
    if memory_to_validate.id() != memory_to_acquire.id() {
        conditions.insert(0, (memory_to_validate.id(), MemoryStatus::ValueAbsent));
    }
    let mut next_scheduled_start: i64 = 0;
    let mut batch_cache: FxHashMap<BlockPos, JitteredLinearRetry> = FxHashMap::default();
    OneShot::new("AcquirePoi", conditions, move |tick| {
        let Some(villager) = villager(tick) else {
            return false;
        };
        if only_if_adult && villager.is_baby_villager() {
            return false;
        }
        let game_time = tick.world.get_world_age();
        if next_scheduled_start == 0 {
            next_scheduled_start = game_time + tick.mob.get_random().random_range(0..20);
            return false;
        }
        if game_time < next_scheduled_start {
            return false;
        }
        next_scheduled_start =
            tick.time + POI_SCAN_RATE + tick.mob.get_random().random_range(0..20);
        batch_cache.retain(|_, retry| retry.is_still_valid(tick.time));
        let center = tick.mob.get_entity().block_pos.load();
        let mut pois: Vec<(BlockPos, String)> = Vec::new();
        for (pos, poi_type) in tick.world.poi_find_all_closest_first(
            center,
            POI_SCAN_RANGE,
            |poi_type| matches(villager, poi_type),
            PoiOccupancy::HasSpace,
        ) {
            if pois.len() >= POI_BATCH_SIZE {
                break;
            }
            let pass = match batch_cache.get_mut(&pos) {
                None => true,
                Some(retry) => {
                    if retry.should_retry(tick.time) {
                        retry.mark_attempt(tick.time);
                        true
                    } else {
                        false
                    }
                }
            };
            if pass {
                pois.push((pos, poi_type));
            }
        }
        pois.retain(|(pos, _)| valid_poi(tick.world, pos));
        let path = find_path_to_pois(tick, &pois);
        if let Some(path) = path.filter(crate::entity::ai::pathfinder::path::Path::can_reach) {
            let target = path.get_target();
            if tick.world.poi_type_at(&target).is_some() {
                tick.world.poi_take(&target);
                if let Some(global) = GlobalPos::of(tick.world, target) {
                    tick.state.set(memory_to_acquire, global);
                }
                if announce_acquisition {
                    broadcast(
                        tick,
                        EntityStatus::VillagerHappy,
                        ActorEventID::VillagerHappy,
                    );
                }
                batch_cache.clear();
            }
        } else {
            for (pos, _) in &pois {
                batch_cache
                    .entry(*pos)
                    .or_insert_with(|| JitteredLinearRetry::new(tick.time));
            }
        }
        true
    })
}

// behavior: GoToPotentialJobSite
struct GoToPotentialJobSite {
    speed_modifier: f32,
}

const POTENTIAL_JOB_SITE_CONDITIONS: [(MemoryModuleId, MemoryStatus); 1] =
    [(types::POTENTIAL_JOB_SITE.id(), MemoryStatus::ValuePresent)];

impl Behavior for GoToPotentialJobSite {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &POTENTIAL_JOB_SITE_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        1200
    }

    fn max_duration(&self) -> i32 {
        1200
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state
            .get_active_non_core_activity()
            .is_none_or(|activity| {
                matches!(activity, Activity::Idle | Activity::Work | Activity::Play)
            })
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::POTENTIAL_JOB_SITE.id())
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(global) = tick.state.get(types::POTENTIAL_JOB_SITE).copied() {
            utils::set_walk_and_look_target_memories_to_block(
                tick.state,
                global.pos,
                self.speed_modifier,
                1,
            );
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(global) = tick.state.get(types::POTENTIAL_JOB_SITE).copied()
            && global.is_in_world(tick.world)
            && tick.world.poi_exists(&global.pos, |_| true)
        {
            tick.world.poi_release(&global.pos);
        }
        tick.state.erase(types::POTENTIAL_JOB_SITE.id());
    }

    fn debug_name(&self) -> &'static str {
        "GoToPotentialJobSite"
    }
}

pub fn go_to_potential_job_site(speed_modifier: f32) -> Timed<impl Behavior> {
    Timed::new(GoToPotentialJobSite { speed_modifier })
}

// behavior: YieldJobSite
fn can_reach_pos(other: &VillagerEntity, pos: BlockPos, valid_range: i32) -> bool {
    let mob = &other.mob_entity;
    mob.navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .create_path(&mob.living_entity, pos.0.to_f64(), valid_range)
        .is_some_and(|path| path.can_reach())
}

fn nearby_wants_jobsite(poi_type: &str, other: &VillagerEntity, poi_pos: BlockPos) -> bool {
    let (has_potential, job_site) = other
        .mob_entity
        .try_with_brain(other, |brain| {
            (
                brain.has_memory_value(types::POTENTIAL_JOB_SITE.id()),
                brain.get(types::JOB_SITE).copied(),
            )
        })
        .unwrap_or((true, None));
    if has_potential {
        return false;
    }
    held_job_site(other.profession(), poi_type)
        && job_site.map_or_else(
            || can_reach_pos(other, poi_pos, valid_range(poi_type)),
            |site| site.pos == poi_pos,
        )
}

pub fn yield_job_site(speed_modifier: f32) -> OneShot {
    OneShot::new(
        "YieldJobSite",
        vec![
            (types::POTENTIAL_JOB_SITE.id(), MemoryStatus::ValuePresent),
            (types::JOB_SITE.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let Some(me) = villager(tick) else {
                return false;
            };
            if me.is_baby_villager() || me.profession() != VillagerProfession::None {
                return false;
            }
            let Some(potential) = tick.state.get(types::POTENTIAL_JOB_SITE).copied() else {
                return false;
            };
            let poi_pos = potential.pos;
            let Some(poi_type) = tick.world.poi_type_at(&poi_pos) else {
                return true;
            };
            let nearby: Vec<Arc<dyn EntityBase>> = tick
                .state
                .get(types::NEAREST_LIVING_ENTITIES)
                .cloned()
                .unwrap_or_default();
            let found = nearby.iter().find_map(|entity| {
                let other = entity.cast_any().downcast_ref::<VillagerEntity>()?;
                (entity.get_entity().entity_id != me.get_entity().entity_id
                    && entity.get_entity().is_alive()
                    && nearby_wants_jobsite(&poi_type, other, poi_pos))
                .then_some(other)
            });
            if let Some(other) = found {
                tick.state.erase(types::WALK_TARGET.id());
                tick.state.erase(types::LOOK_TARGET.id());
                tick.state.erase(types::POTENTIAL_JOB_SITE.id());
                other.mob_entity.try_with_brain(other, |brain| {
                    if !brain.has_memory_value(types::JOB_SITE.id()) {
                        utils::set_walk_and_look_target_memories_to_block(
                            brain,
                            poi_pos,
                            speed_modifier,
                            1,
                        );
                        if let Some(global) = GlobalPos::of(tick.world, poi_pos) {
                            brain.set(types::POTENTIAL_JOB_SITE, global);
                        }
                    }
                });
            }
            true
        },
    )
}

// behavior: AssignProfessionFromJobSite
pub fn assign_profession_from_job_site() -> OneShot {
    OneShot::new(
        "AssignProfessionFromJobSite",
        vec![
            (types::POTENTIAL_JOB_SITE.id(), MemoryStatus::ValuePresent),
            (types::JOB_SITE.id(), MemoryStatus::Registered),
        ],
        |tick| {
            let Some(villager) = villager(tick) else {
                return false;
            };
            let Some(global) = tick.state.get(types::POTENTIAL_JOB_SITE).copied() else {
                return false;
            };
            if !closer_to_center_than(global.pos, body_pos(tick), 2.0) {
                return false;
            }
            tick.state.erase(types::POTENTIAL_JOB_SITE.id());
            tick.state.set(types::JOB_SITE, global);
            broadcast(
                tick,
                EntityStatus::VillagerHappy,
                ActorEventID::VillagerHappy,
            );
            if villager.profession() != VillagerProfession::None {
                return true;
            }
            if let Some(profession) = tick
                .world
                .poi_type_at(&global.pos)
                .and_then(|poi_type| profession_for_poi_type(&poi_type))
            {
                villager.change_profession(profession);
            }
            true
        },
    )
}

// behavior: ResetProfession
pub fn reset_profession() -> OneShot {
    OneShot::new(
        "ResetProfession",
        vec![(types::JOB_SITE.id(), MemoryStatus::ValueAbsent)],
        |tick| {
            let Some(villager) = villager(tick) else {
                return false;
            };
            let profession = villager.profession();
            let can_be_fired =
                profession != VillagerProfession::None && profession != VillagerProfession::Nitwit;
            if can_be_fired && villager.villager_xp() == 0 && villager.villager_level() <= 1 {
                villager.change_profession(VillagerProfession::None);
                true
            } else {
                false
            }
        },
    )
}

// behavior: SetWalkTargetFromBlockMemory
pub fn set_walk_target_from_block_memory(
    memory: MemoryModuleType<GlobalPos>,
    speed_modifier: f32,
    close_enough_dist: i32,
    too_far_distance: i32,
    too_long_unreachable_duration: i64,
) -> OneShot {
    OneShot::new(
        "SetWalkTargetFromBlockMemory",
        vec![
            (
                types::CANT_REACH_WALK_TARGET_SINCE.id(),
                MemoryStatus::Registered,
            ),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (memory.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let Some(villager) = villager(tick) else {
                return false;
            };
            let Some(target) = tick.state.get(memory).copied() else {
                return false;
            };
            let cant_reach_since = tick.state.get(types::CANT_REACH_WALK_TARGET_SINCE).copied();
            let game_time = tick.world.get_world_age();
            let body = tick.mob.get_entity().block_pos.load();
            if target.is_in_world(tick.world)
                && cant_reach_since
                    .is_none_or(|since| game_time - since <= too_long_unreachable_duration)
            {
                let manhattan = target.pos.manhattan_distance(body);
                if manhattan > too_far_distance {
                    let mut towards: Option<Vector3<f64>> = None;
                    let mut tries = 0;
                    while towards.is_none_or(|pos| {
                        BlockPos::floored_v(pos).manhattan_distance(body) > too_far_distance
                    }) {
                        towards = default_random_pos::get_pos_towards(
                            tick.mob,
                            15,
                            7,
                            Vector3::new(
                                f64::from(target.pos.0.x) + 0.5,
                                f64::from(target.pos.0.y),
                                f64::from(target.pos.0.z) + 0.5,
                            ),
                            std::f64::consts::FRAC_PI_2,
                        );
                        tries += 1;
                        if tries == 1000 {
                            villager.release_poi_in(tick.state, memory);
                            tick.state.erase(memory.id());
                            tick.state
                                .set(types::CANT_REACH_WALK_TARGET_SINCE, tick.time);
                            return true;
                        }
                    }
                    if let Some(pos) = towards {
                        tick.state.set(
                            types::WALK_TARGET,
                            WalkTarget::from_vec(pos, speed_modifier, close_enough_dist),
                        );
                    }
                } else if manhattan > close_enough_dist {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_block_pos(target.pos, speed_modifier, close_enough_dist),
                    );
                }
            } else {
                villager.release_poi_in(tick.state, memory);
                tick.state.erase(memory.id());
                tick.state
                    .set(types::CANT_REACH_WALK_TARGET_SINCE, tick.time);
            }
            true
        },
    )
}

// behavior: StrollAroundPoi
pub fn stroll_around_poi(
    memory: MemoryModuleType<GlobalPos>,
    speed_modifier: f32,
    max_distance_from_poi: i32,
) -> OneShot {
    let mut next_ok_start_time: i64 = 0;
    OneShot::new(
        "StrollAroundPoi",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (memory.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let Some(pos) = tick.state.get(memory).copied() else {
                return false;
            };
            if !pos.is_in_world(tick.world)
                || !closer_to_center_than(pos.pos, body_pos(tick), f64::from(max_distance_from_poi))
            {
                return false;
            }
            if tick.time <= next_ok_start_time {
                return true;
            }
            match land_random_pos::get_pos(tick.mob, 8, 6) {
                Some(land) => tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::from_vec(land, speed_modifier, 1),
                ),
                None => tick.state.erase(types::WALK_TARGET.id()),
            }
            next_ok_start_time = tick.time + 180;
            true
        },
    )
}

// behavior: StrollToPoi
pub fn stroll_to_poi(
    memory: MemoryModuleType<GlobalPos>,
    speed_modifier: f32,
    close_enough_dist: i32,
    max_distance_from_poi: i32,
) -> OneShot {
    let mut next_ok_start_time: i64 = 0;
    OneShot::new(
        "StrollToPoi",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (memory.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let Some(pos) = tick.state.get(memory).copied() else {
                return false;
            };
            if !pos.is_in_world(tick.world)
                || !closer_to_center_than(pos.pos, body_pos(tick), f64::from(max_distance_from_poi))
            {
                return false;
            }
            if tick.time <= next_ok_start_time {
                return true;
            }
            tick.state.set(
                types::WALK_TARGET,
                WalkTarget::from_block_pos(pos.pos, speed_modifier, close_enough_dist),
            );
            next_ok_start_time = tick.time + 80;
            true
        },
    )
}

// behavior: StrollToPoiList
pub fn stroll_to_poi_list(
    stroll_to_memory: MemoryModuleType<Vec<GlobalPos>>,
    speed_modifier: f32,
    close_enough_dist: i32,
    max_distance_from_poi: i32,
    must_be_close_to: MemoryModuleType<GlobalPos>,
) -> OneShot {
    let mut next_ok_start_time: i64 = 0;
    OneShot::new(
        "StrollToPoiList",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (stroll_to_memory.id(), MemoryStatus::ValuePresent),
            (must_be_close_to.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let Some(stroll_to) = tick.state.get(stroll_to_memory).cloned() else {
                return false;
            };
            let Some(stay_close) = tick.state.get(must_be_close_to).copied() else {
                return false;
            };
            if stroll_to.is_empty() {
                return false;
            }
            let index = tick.mob.get_random().random_range(0..stroll_to.len());
            let target = stroll_to[index];
            if target.is_in_world(tick.world)
                && closer_to_center_than(
                    stay_close.pos,
                    body_pos(tick),
                    f64::from(max_distance_from_poi),
                )
            {
                if tick.time > next_ok_start_time {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_block_pos(target.pos, speed_modifier, close_enough_dist),
                    );
                    next_ok_start_time = tick.time + 100;
                }
                true
            } else {
                false
            }
        },
    )
}

// behavior: SleepInBed
struct SleepInBed {
    next_ok_start_time: i64,
}

const SLEEP_CONDITIONS: [(MemoryModuleId, MemoryStatus); 5] = [
    (types::HOME.id(), MemoryStatus::ValuePresent),
    (types::LAST_WOKEN.id(), MemoryStatus::Registered),
    (types::LAST_SLEPT.id(), MemoryStatus::Registered),
    (types::WALK_TARGET.id(), MemoryStatus::Registered),
    (
        types::CANT_REACH_WALK_TARGET_SINCE.id(),
        MemoryStatus::Registered,
    ),
];
const COOLDOWN_AFTER_BEING_WOKEN: i64 = 100;

impl Behavior for SleepInBed {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &SLEEP_CONDITIONS
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        if tick.mob.get_entity().has_vehicle() {
            return false;
        }
        let Some(target) = tick.state.get(types::HOME).copied() else {
            return false;
        };
        if !target.is_in_world(tick.world) {
            return false;
        }
        if let Some(woken) = tick.state.get(types::LAST_WOKEN) {
            let since = tick.world.get_world_age() - *woken;
            if since > 0 && since < COOLDOWN_AFTER_BEING_WOKEN {
                return false;
            }
        }
        let (block, state) = tick.world.get_block_and_state(&target.pos);
        closer_to_center_than(target.pos, body_pos(tick), 2.0)
            && block.has_tag(&tag::Block::MINECRAFT_VILLAGERS_CAN_SLEEP_ON_BED)
            && !BedProperties::from_state_id(state.id).occupied
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let Some(home) = tick.state.get(types::HOME).copied() else {
            return false;
        };
        let pos = body_pos(tick);
        tick.state.is_active(Activity::Rest)
            && pos.y > f64::from(home.pos.0.y) + 0.4
            && closer_to_center_than(home.pos, pos, 1.14)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        if tick.time <= self.next_ok_start_time {
            return;
        }
        let Some(villager) = villager(tick) else {
            return;
        };
        if tick.state.has_memory_value(types::DOORS_TO_CLOSE.id()) {
            let mut doors = tick
                .state
                .get(types::DOORS_TO_CLOSE)
                .cloned()
                .unwrap_or_default();
            crate::entity::ai::brain::behavior::interact_with_door::close_doors_that_i_have_opened_or_passed_through(
                tick, None, None, &mut doors,
            );
            tick.state.set(types::DOORS_TO_CLOSE, doors);
        }
        if let Some(home) = tick.state.get(types::HOME).copied()
            && villager.start_sleeping(home.pos)
        {
            tick.state.set(types::LAST_SLEPT, tick.time);
        }
        tick.state.erase(types::WALK_TARGET.id());
        tick.state.erase(types::CANT_REACH_WALK_TARGET_SINCE.id());
    }

    fn stop_timed(&mut self, tick: &mut BrainTick<'_>, _timed_out: bool) {
        if let Some(villager) = villager(tick)
            && villager.is_villager_sleeping()
        {
            villager.stop_sleeping_in(tick.state);
            self.next_ok_start_time = tick.time + 40;
        }
    }

    fn debug_name(&self) -> &'static str {
        "SleepInBed"
    }
}

pub fn sleep_in_bed() -> Timed<impl Behavior> {
    Timed::new(SleepInBed {
        next_ok_start_time: 0,
    })
}

// behavior: SetClosestHomeAsWalkTarget
pub fn set_closest_home_as_walk_target(speed_modifier: f32) -> OneShot {
    let mut batch_cache: FxHashMap<BlockPos, i64> = FxHashMap::default();
    let mut last_update: i64 = 0;
    OneShot::new(
        "SetClosestHomeAsWalkTarget",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::HOME.id(), MemoryStatus::ValueAbsent),
        ],
        move |tick| {
            let game_time = tick.world.get_world_age();
            if game_time - last_update < POI_SCAN_RATE {
                return false;
            }
            let center = tick.mob.get_entity().block_pos.load();
            let homes = tick.world.poi_find_all_closest_first(
                center,
                POI_SCAN_RANGE,
                |poi_type| poi_type == "minecraft:home",
                PoiOccupancy::Any,
            );
            let Some((closest, _)) = homes.first() else {
                return false;
            };
            if block_dist_sq(*closest, center) <= 4 {
                return false;
            }
            last_update = game_time + i64::from(tick.mob.get_random().random_range(0..20));
            let mut tried = 0;
            let mut pois: Vec<(BlockPos, String)> = Vec::new();
            for (pos, poi_type) in homes {
                if batch_cache.contains_key(&pos) {
                    continue;
                }
                tried += 1;
                if tried >= POI_BATCH_SIZE {
                    break;
                }
                batch_cache.insert(pos, last_update + 40);
                pois.push((pos, poi_type));
            }
            let path = find_path_to_pois(tick, &pois);
            if let Some(path) = path.filter(crate::entity::ai::pathfinder::path::Path::can_reach) {
                let target = path.get_target();
                if tick.world.poi_type_at(&target).is_some() {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_block_pos(target, speed_modifier, 1),
                    );
                }
            } else if tried < POI_BATCH_SIZE {
                batch_cache.retain(|_, until| *until >= last_update);
            }
            true
        },
    )
}

// behavior: InsideBrownianWalk
pub fn inside_brownian_walk(speed_modifier: f32) -> OneShot {
    OneShot::new(
        "InsideBrownianWalk",
        vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
        move |tick| {
            let body = tick.mob.get_entity().block_pos.load();
            if tick.world.can_see_sky(&body) {
                return false;
            }
            let mut candidates: Vec<BlockPos> = Vec::with_capacity(27);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        candidates.push(BlockPos::new(body.0.x + dx, body.0.y + dy, body.0.z + dz));
                    }
                }
            }
            candidates.shuffle(&mut tick.mob.get_random());
            if let Some(target) = candidates.into_iter().find(|pos| {
                !tick.world.can_see_sky(pos) && tick.world.get_block_state(&pos.down()).is_solid()
            }) {
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::from_block_pos(target, speed_modifier, 0),
                );
            }
            true
        },
    )
}

// behavior util: BehaviorUtils.findSectionClosestToVillage
fn find_section_closest_to_village(
    world: &World,
    center: BlockPos,
    radius: i32,
) -> Option<BlockPos> {
    let (sx, sy, sz) = (center.0.x >> 4, center.0.y >> 4, center.0.z >> 4);
    let current = world.sections_to_village(&center);
    let mut best: Option<(i32, BlockPos)> = None;
    for dx in -radius..=radius {
        for dy in -radius..=radius {
            for dz in -radius..=radius {
                let section_center =
                    BlockPos::new((sx + dx) * 16 + 8, (sy + dy) * 16 + 8, (sz + dz) * 16 + 8);
                let distance = world.sections_to_village(&section_center);
                if distance < current && best.is_none_or(|(known, _)| distance < known) {
                    best = Some((distance, section_center));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

// behavior: GoToClosestVillage
pub fn go_to_closest_village(speed_modifier: f32, close_enough_distance: i32) -> OneShot {
    OneShot::new(
        "GoToClosestVillage",
        vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
        move |tick| {
            let body = tick.mob.get_entity().block_pos.load();
            if tick.world.is_village(&body) {
                return false;
            }
            let sections_to_village = tick.world.sections_to_village(&body);
            let mut target: Option<Vector3<f64>> = None;
            for _ in 0..5 {
                let land = land_random_pos::get_pos_weighted(tick.mob, 15, 7, |pos| {
                    -f64::from(tick.world.sections_to_village(pos))
                });
                if let Some(land) = land {
                    let land_sections = tick.world.sections_to_village(&BlockPos::floored_v(land));
                    if land_sections < sections_to_village {
                        target = Some(land);
                        break;
                    }
                    if land_sections == sections_to_village {
                        target = Some(land);
                    }
                }
            }
            if let Some(target) = target {
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::from_vec(target, speed_modifier, close_enough_distance),
                );
            }
            true
        },
    )
}

// behavior: VillageBoundRandomStroll
pub fn village_bound_random_stroll(speed_modifier: f32, max_xz: i32, max_y: i32) -> OneShot {
    OneShot::new(
        "VillageBoundRandomStroll",
        vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
        move |tick| {
            let body = tick.mob.get_entity().block_pos.load();
            let land = if tick.world.is_village(&body) {
                land_random_pos::get_pos(tick.mob, max_xz, max_y)
            } else if let Some(optimal) = find_section_closest_to_village(tick.world, body, 2) {
                default_random_pos::get_pos_towards(
                    tick.mob,
                    max_xz,
                    max_y,
                    Vector3::new(
                        f64::from(optimal.0.x) + 0.5,
                        f64::from(optimal.0.y),
                        f64::from(optimal.0.z) + 0.5,
                    ),
                    std::f64::consts::FRAC_PI_2,
                )
            } else {
                land_random_pos::get_pos(tick.mob, max_xz, max_y)
            };
            match land {
                Some(pos) => tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::from_vec(pos, speed_modifier, 0),
                ),
                None => tick.state.erase(types::WALK_TARGET.id()),
            }
            true
        },
    )
}

// behavior util: MoveToSkySeeingSpot.hasNoBlocksAbove
pub(super) fn has_no_blocks_above(world: &World, body_y: f64, target: &BlockPos) -> bool {
    world.can_see_sky(target)
        && f64::from(
            world.get_heightmap_height(
                pumpkin_world::chunk::ChunkHeightmapType::MotionBlocking,
                target.0.x,
                target.0.z,
            ) + 1,
        ) <= body_y
}

// behavior: MoveToSkySeeingSpot
pub fn move_to_sky_seeing_spot(speed_modifier: f32) -> OneShot {
    OneShot::new(
        "MoveToSkySeeingSpot",
        vec![(types::WALK_TARGET.id(), MemoryStatus::ValueAbsent)],
        move |tick| {
            let body = tick.mob.get_entity().block_pos.load();
            if tick.world.can_see_sky(&body) {
                return false;
            }
            let body_y = tick.mob.get_entity().pos.load().y;
            for _ in 0..10 {
                let (dx, dy, dz) = {
                    let mut random = tick.mob.get_random();
                    (
                        random.random_range(0..20) - 10,
                        random.random_range(0..6) - 3,
                        random.random_range(0..20) - 10,
                    )
                };
                let random_pos = BlockPos::new(body.0.x + dx, body.0.y + dy, body.0.z + dz);
                if has_no_blocks_above(tick.world, body_y, &random_pos) {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_vec(
                            Vector3::new(
                                f64::from(random_pos.0.x) + 0.5,
                                f64::from(random_pos.0.y),
                                f64::from(random_pos.0.z) + 0.5,
                            ),
                            speed_modifier,
                            0,
                        ),
                    );
                    break;
                }
            }
            true
        },
    )
}

// behavior: LocateHidingPlace
pub fn locate_hiding_place(radius: i32, speed_modifier: f32, close_enough_dist: i32) -> OneShot {
    OneShot::new(
        "LocateHidingPlace",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::HOME.id(), MemoryStatus::Registered),
            (types::HIDING_PLACE.id(), MemoryStatus::Registered),
            (types::PATH.id(), MemoryStatus::Registered),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::BREED_TARGET.id(), MemoryStatus::Registered),
            (types::INTERACTION_TARGET.id(), MemoryStatus::Registered),
        ],
        move |tick| {
            let body = tick.mob.get_entity().block_pos.load();
            let body_pos = tick.mob.get_entity().pos.load();
            let is_home = |poi_type: &str| poi_type == "minecraft:home";
            let near = tick
                .world
                .poi_find_all_closest_first(body, close_enough_dist + 1, is_home, PoiOccupancy::Any)
                .first()
                .map(|(pos, _)| *pos)
                .filter(|pos| closer_to_center_than(*pos, body_pos, f64::from(close_enough_dist)));
            let chosen = near
                .or_else(|| {
                    let all = tick.world.poi_find_all_closest_first(
                        body,
                        radius,
                        is_home,
                        PoiOccupancy::Any,
                    );
                    if all.is_empty() {
                        None
                    } else {
                        let index = tick.mob.get_random().random_range(0..all.len());
                        Some(all[index].0)
                    }
                })
                .or_else(|| tick.state.get(types::HOME).map(|home| home.pos));
            if let Some(pos) = chosen {
                tick.state.erase(types::PATH.id());
                tick.state.erase(types::LOOK_TARGET.id());
                tick.state.erase(types::BREED_TARGET.id());
                tick.state.erase(types::INTERACTION_TARGET.id());
                if let Some(global) = GlobalPos::of(tick.world, pos) {
                    tick.state.set(types::HIDING_PLACE, global);
                }
                if !closer_to_center_than(pos, body_pos, f64::from(close_enough_dist)) {
                    tick.state.set(
                        types::WALK_TARGET,
                        WalkTarget::from_block_pos(pos, speed_modifier, close_enough_dist),
                    );
                }
            }
            true
        },
    )
}

// behavior: SetHiddenState
pub fn set_hidden_state(seconds: i32, close_enough_dist: i32) -> OneShot {
    const HIDE_TIMEOUT: i64 = 300;
    let stay_hidden_ticks = seconds * 20;
    let mut ticks_hidden = 0;
    OneShot::new(
        "SetHiddenState",
        vec![
            (types::HIDING_PLACE.id(), MemoryStatus::ValuePresent),
            (types::HEARD_BELL_TIME.id(), MemoryStatus::ValuePresent),
        ],
        move |tick| {
            let Some(heard) = tick.state.get(types::HEARD_BELL_TIME).copied() else {
                return false;
            };
            let timed_out_trying_to_hide = heard + HIDE_TIMEOUT <= tick.time;
            if ticks_hidden <= stay_hidden_ticks && !timed_out_trying_to_hide {
                if let Some(place) = tick.state.get(types::HIDING_PLACE).copied() {
                    let body = tick.mob.get_entity().block_pos.load();
                    if block_dist_sq(place.pos, body) < i64::from(close_enough_dist).pow(2) {
                        ticks_hidden += 1;
                    }
                }
            } else {
                tick.state.erase(types::HEARD_BELL_TIME.id());
                tick.state.erase(types::HIDING_PLACE.id());
                update_schedule(tick);
                ticks_hidden = 0;
            }
            true
        },
    )
}

// behavior: RingBell
pub fn ring_bell() -> OneShot {
    const BELL_RING_CHANCE: f32 = 0.95;
    const RING_BELL_FROM_DISTANCE: i64 = 3;
    OneShot::new(
        "RingBell",
        vec![(types::MEETING_POINT.id(), MemoryStatus::ValuePresent)],
        |tick| {
            if tick.mob.get_random().random::<f32>() <= BELL_RING_CHANCE {
                return false;
            }
            let Some(meeting) = tick.state.get(types::MEETING_POINT).copied() else {
                return false;
            };
            let body = tick.mob.get_entity().block_pos.load();
            if block_dist_sq(meeting.pos, body) < RING_BELL_FROM_DISTANCE * RING_BELL_FROM_DISTANCE
                && tick.world.get_block(&meeting.pos) == &pumpkin_data::Block::BELL
            {
                let entity = tick.world.get_entity_by_id(tick.mob.get_entity().entity_id);
                crate::block::blocks::redstone::bell::ring_bell(
                    meeting.pos,
                    tick.world,
                    None,
                    entity,
                );
            }
            true
        },
    )
}

// behavior: SocializeAtBell
pub fn socialize_at_bell() -> OneShot {
    OneShot::new(
        "SocializeAtBell",
        vec![
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::MEETING_POINT.id(), MemoryStatus::ValuePresent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
            (types::INTERACTION_TARGET.id(), MemoryStatus::ValueAbsent),
        ],
        |tick| {
            let Some(meeting) = tick.state.get(types::MEETING_POINT).copied() else {
                return false;
            };
            let position = body_pos(tick);
            if tick.mob.get_random().random_range(0..100) != 0
                || !meeting.is_in_world(tick.world)
                || !closer_to_center_than(meeting.pos, position, 4.0)
            {
                return false;
            }
            let Some(visible) = tick.state.get(types::NEAREST_VISIBLE_LIVING_ENTITIES) else {
                return false;
            };
            let ctx = tick.visibility();
            let is_villager = |entity: &Arc<dyn EntityBase>| {
                entity.get_entity().entity_type == &EntityType::VILLAGER
            };
            if !visible.contains_any(&ctx, is_villager) {
                return false;
            }
            let closest = visible.find_closest(&ctx, |entity| {
                is_villager(entity)
                    && entity
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&position)
                        <= 32.0
            });
            if let Some(other) = closest {
                tick.state.set(types::INTERACTION_TARGET, other.clone());
                tick.state.set(
                    types::LOOK_TARGET,
                    Arc::new(EntityTracker::new(other.clone(), true))
                        as Arc<
                            dyn crate::entity::ai::brain::memory::position_tracker::PositionTracker,
                        >,
                );
                tick.state.set(
                    types::WALK_TARGET,
                    WalkTarget::new(Arc::new(EntityTracker::new(other, false)), 0.3, 1),
                );
            }
            true
        },
    )
}
