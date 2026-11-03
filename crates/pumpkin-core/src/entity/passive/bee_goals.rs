use std::sync::atomic::Ordering::Relaxed;

use pumpkin_data::{
    Block,
    block_properties::{
        DoubleBlockHalf, NetherWartLikeProperties, TallSeagrassLikeProperties,
        TorchflowerCropLikeProperties, WheatLikeProperties,
    },
    sound::{Sound, SoundCategory},
    tag::{self, Taggable},
};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::world::BlockFlags;
use rustc_hash::FxHashMap;

use super::bee::BeeEntity;
use crate::entity::{
    EntityBase,
    ai::{
        goal::{Controls, Goal, to_goal_ticks},
        util::{air_and_water_random_pos, hover_random_pos},
    },
    mob::{Mob, neutral::NeutralMob},
};
use crate::world::World;

// bee: constants (Bee)
const MAX_TRAVELLING_TICKS: i32 = 2400;
const MAX_BLACKLISTED_TARGETS: usize = 3;
const TICKS_BEFORE_HIVE_DROP: i32 = 60;
const HIVE_SEARCH_DISTANCE: i32 = 20;
const PATHFIND_TO_HIVE_WHEN_CLOSER_THAN: i32 = 16;
const MAX_CROPS_GROWABLE: i32 = 10;
const GROW_CHANCE: i32 = 30;
const TICKS_BEFORE_GOING_TO_KNOWN_FLOWER: i32 = 600;
const FLOWER_SEARCH_RADIUS: i32 = 5;
const MIN_POLLINATION_TICKS: i32 = 400;
const MAX_POLLINATING_TICKS: i32 = 600;
const POSITION_CHANGE_CHANCE: i32 = 25;
const POLLINATE_SPEED_MODIFIER: f64 = 0.35;
const HOVER_HEIGHT_WITHIN_FLOWER: f64 = 0.6;
const HOVER_POS_OFFSET: f32 = 0.333_333_34;
const ARRIVAL_THRESHOLD: f64 = 0.1;
const RESTRICTED_WANDER_DISTANCE_REDUCTION: i32 = 24;
const DEFAULT_WANDER_DISTANCE_REDUCTION: i32 = 16;
const TOO_FAR_DISTANCE: i32 = 48;

fn nav_of(bee: &BeeEntity) -> std::sync::MutexGuard<'_, crate::entity::ai::pathfinder::Navigator> {
    bee.mob_entity
        .navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn bee(mob: &dyn Mob) -> Option<&BeeEntity> {
    mob.cast_any().downcast_ref::<BeeEntity>()
}

fn is_angry(mob: &dyn Mob) -> bool {
    mob.as_neutral().is_some_and(NeutralMob::is_angry)
}

fn game_time(bee: &BeeEntity) -> i64 {
    bee.get_entity()
        .world
        .load()
        .level_time
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .world_age
}

// bee: Bee.attractsBees
#[must_use]
pub fn attracts_bees(world: &World, pos: &BlockPos) -> bool {
    let (block, state_id) = world.get_block_and_state_id(pos);
    if !block.has_tag(&tag::Block::MINECRAFT_BEE_ATTRACTIVE) {
        return false;
    }
    if world.get_fluid(pos).has_tag(&tag::Fluid::MINECRAFT_WATER) {
        return false;
    }
    if block == &Block::SUNFLOWER {
        return TallSeagrassLikeProperties::from_state_id(state_id).half == DoubleBlockHalf::Upper;
    }
    true
}

// bee: Vec3.atBottomCenterOf
fn bottom_center(pos: &BlockPos) -> Vector3<f64> {
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y),
        f64::from(pos.0.z) + 0.5,
    )
}

// bee: BlockPos.closerToCenterThan(position, distance)
fn closer_to_center_than(pos: &BlockPos, at: &Vector3<f64>, distance: f64) -> bool {
    let center = pos.to_centered_f64();
    center.squared_distance_to_vec(at) < distance * distance
}

// bee: Bee.EnterHiveGoal (BeeEnterHiveGoal)
pub struct EnterHiveGoal;

impl Goal for EnterHiveGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        if is_angry(mob) {
            return false;
        }
        let Some(hive) = bee.hive_pos() else {
            return false;
        };
        if bee.wants_to_enter_hive()
            && closer_to_center_than(&hive, &bee.get_entity().pos.load(), 2.0)
            && let Some(full) =
                bee.with_hive(crate::block::entities::beehive::BeehiveBlockEntity::is_full)
        {
            if !full {
                return true;
            }
            *bee.hive_pos
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let world = bee.get_entity().world.load_full();
        bee.with_hive(|hive| hive.add_occupant(&world, bee));
    }
}

// bee: Bee.ValidateHiveGoal
pub struct ValidateHiveGoal {
    cooldown: i64,
    last_validate_tick: i64,
}

impl ValidateHiveGoal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cooldown: rand::random_range(20..=40),
            last_validate_tick: -1,
        }
    }
}

impl Goal for ValidateHiveGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        !is_angry(mob) && game_time(bee) > self.last_validate_tick + self.cooldown
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        if let Some(hive) = bee.hive_pos()
            && bee.get_entity().world.load().is_loaded(&hive)
            && !bee.is_hive_valid()
        {
            bee.drop_hive();
        }
        self.last_validate_tick = game_time(bee);
    }
}

// bee: Bee.ValidateFlowerGoal
pub struct ValidateFlowerGoal {
    cooldown: i64,
    last_validate_tick: i64,
}

impl ValidateFlowerGoal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cooldown: rand::random_range(20..=40),
            last_validate_tick: -1,
        }
    }
}

impl Goal for ValidateFlowerGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        !is_angry(mob) && game_time(bee) > self.last_validate_tick + self.cooldown
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let world = bee.get_entity().world.load_full();
        if let Some(flower) = bee.saved_flower_pos()
            && world.is_loaded(&flower)
            && !attracts_bees(&world, &flower)
        {
            bee.drop_flower();
        }
        self.last_validate_tick = game_time(bee);
    }
}

// bee: Bee.BeePollinateGoal
pub struct PollinateGoal {
    successful_pollinating_ticks: i32,
    last_sound_played_tick: i32,
    hover_pos: Option<Vector3<f64>>,
    pollinating_ticks: i32,
    unreachable_flower_cache: FxHashMap<i64, i64>,
}

impl PollinateGoal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            successful_pollinating_ticks: 0,
            last_sound_played_tick: 0,
            hover_pos: None,
            pollinating_ticks: 0,
            unreachable_flower_cache: FxHashMap::default(),
        }
    }

    const fn has_pollinated_long_enough(&self) -> bool {
        self.successful_pollinating_ticks > MIN_POLLINATION_TICKS
    }

    fn set_wanted_pos(&self, bee: &BeeEntity) {
        if let Some(hover) = self.hover_pos {
            bee.mob_entity.steered().move_control().set_wanted_position(
                hover.x,
                hover.y,
                hover.z,
                POLLINATE_SPEED_MODIFIER,
            );
        }
    }

    fn offset() -> f64 {
        f64::from((rand::random::<f32>() * 2.0 - 1.0) * HOVER_POS_OFFSET)
    }

    // bee: findNearbyFlower, manhattan box search with an unreachable cache
    fn find_nearby_flower(&mut self, bee: &BeeEntity) -> Option<BlockPos> {
        let world = bee.get_entity().world.load_full();
        let origin = bee.get_entity().block_pos.load();
        let now = game_time(bee);
        let mut temp_cache = FxHashMap::default();
        let mut result = None;
        'search: for distance in 0..=FLOWER_SEARCH_RADIUS {
            for x in -distance..=distance {
                for y in -(distance - x.abs())..=(distance - x.abs()) {
                    let rest = distance - x.abs() - y.abs();
                    let zs: &[i32] = if rest == 0 { &[0] } else { &[rest, -rest] };
                    for &z in zs {
                        let pos = origin.offset(Vector3::new(x, y, z));
                        if !attracts_bees(&world, &pos) {
                            continue;
                        }
                        let key = pos.as_long();
                        let until = self
                            .unreachable_flower_cache
                            .get(&key)
                            .copied()
                            .unwrap_or(i64::MIN);
                        if now < until {
                            temp_cache.insert(key, until);
                            continue;
                        }
                        let reachable = {
                            let mut navigator = nav_of(bee);
                            navigator
                                .create_path(&bee.mob_entity.living_entity, bottom_center(&pos), 1)
                                .is_some_and(|path| path.can_reach())
                        };
                        if reachable {
                            result = Some(pos);
                            break 'search;
                        }
                        temp_cache.insert(key, now + 600);
                    }
                }
            }
        }
        self.unreachable_flower_cache = temp_cache;
        result
    }
}

impl Goal for PollinateGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        if is_angry(mob) || bee.cooldown_before_locating_new_flower.load(Relaxed) > 0 {
            return false;
        }
        if bee.has_nectar() {
            return false;
        }
        let world = bee.get_entity().world.load_full();
        if world.is_raining() {
            return false;
        }
        let Some(flower) = self.find_nearby_flower(bee) else {
            bee.cooldown_before_locating_new_flower
                .store(rand::random_range(20..=60), Relaxed);
            return false;
        };
        bee.set_saved_flower_pos(flower);
        let mut navigator = nav_of(bee);
        navigator.move_to_coords(
            f64::from(flower.0.x) + 0.5,
            f64::from(flower.0.y) + 0.5,
            f64::from(flower.0.z) + 0.5,
            1.2,
            &bee.mob_entity.living_entity,
        );
        true
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        if is_angry(mob)
            || !bee.is_pollinating()
            || !bee.has_saved_flower_pos()
            || bee.get_entity().world.load().is_raining()
        {
            return false;
        }
        if self.has_pollinated_long_enough() {
            rand::random::<f32>() < 0.2
        } else {
            true
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        self.successful_pollinating_ticks = 0;
        self.pollinating_ticks = 0;
        self.last_sound_played_tick = 0;
        bee.pollinating.store(true, Relaxed);
        bee.ticks_without_nectar.store(0, Relaxed);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        if self.has_pollinated_long_enough() {
            bee.set_nectar(true);
        }
        bee.pollinating.store(false, Relaxed);
        nav_of(bee).stop();
        bee.cooldown_before_locating_new_flower.store(200, Relaxed);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let Some(flower) = bee.saved_flower_pos() else {
            return;
        };
        self.pollinating_ticks += 1;
        if self.pollinating_ticks > MAX_POLLINATING_TICKS {
            bee.drop_flower();
            bee.pollinating.store(false, Relaxed);
            bee.cooldown_before_locating_new_flower.store(200, Relaxed);
            return;
        }
        let mut flower_pos = bottom_center(&flower);
        flower_pos.y += HOVER_HEIGHT_WITHIN_FLOWER;
        let bee_pos = bee.get_entity().pos.load();
        if flower_pos.squared_distance_to_vec(&bee_pos).sqrt() > 1.0 {
            self.hover_pos = Some(flower_pos);
            self.set_wanted_pos(bee);
            return;
        }
        let hover = *self.hover_pos.get_or_insert(flower_pos);
        let arrived = bee_pos.squared_distance_to_vec(&hover).sqrt() <= ARRIVAL_THRESHOLD;
        let mut should_set_wanted_pos = true;
        if !arrived && self.pollinating_ticks > MAX_POLLINATING_TICKS {
            bee.drop_flower();
            return;
        }
        if arrived {
            if rand::random_range(0..POSITION_CHANGE_CHANCE) == 0 {
                self.hover_pos = Some(Vector3::new(
                    flower_pos.x + Self::offset(),
                    flower_pos.y,
                    flower_pos.z + Self::offset(),
                ));
                nav_of(bee).stop();
            } else {
                should_set_wanted_pos = false;
            }
            bee.mob_entity
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at(mob, flower_pos.x, flower_pos.y, flower_pos.z);
        }
        if should_set_wanted_pos {
            self.set_wanted_pos(bee);
        }
        self.successful_pollinating_ticks += 1;
        if rand::random::<f32>() < 0.05
            && self.successful_pollinating_ticks > self.last_sound_played_tick + 60
        {
            self.last_sound_played_tick = self.successful_pollinating_ticks;
            bee.get_entity().world.load().play_sound_fine(
                Sound::EntityBeePollinate,
                SoundCategory::Neutral,
                &bee_pos,
                1.0,
                1.0,
            );
        }
    }
}

// bee: Bee.BeeLocateHiveGoal
pub struct LocateHiveGoal;

impl LocateHiveGoal {
    // bee: findNearbyHivesWithSpace, bee_home POIs sorted by distance
    fn find_nearby_hives_with_space(bee: &BeeEntity) -> Vec<BlockPos> {
        let world = bee.get_entity().world.load_full();
        let bee_pos = bee.get_entity().block_pos.load();
        let mut hives: Vec<BlockPos> = Vec::new();
        world
            .portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(bee_pos, HIVE_SEARCH_DISTANCE, |pos, poi_type| {
                let (dx, dy, dz) = (
                    i64::from(pos.0.x - bee_pos.0.x),
                    i64::from(pos.0.y - bee_pos.0.y),
                    i64::from(pos.0.z - bee_pos.0.z),
                );
                let in_range = dx * dx + dy * dy + dz * dz
                    <= i64::from(HIVE_SEARCH_DISTANCE) * i64::from(HIVE_SEARCH_DISTANCE);
                if in_range && matches!(poi_type, "minecraft:beehive" | "minecraft:bee_nest") {
                    hives.push(pos);
                }
            });
        hives.retain(|pos| {
            world
                .get_block_entity(pos)
                .and_then(|entity| {
                    entity
                        .as_any()
                        .downcast_ref::<crate::block::entities::beehive::BeehiveBlockEntity>()
                        .map(|hive| !hive.is_full())
                })
                .unwrap_or(false)
        });
        hives.sort_by_key(|pos| {
            let (dx, dy, dz) = (
                i64::from(pos.0.x - bee_pos.0.x),
                i64::from(pos.0.y - bee_pos.0.y),
                i64::from(pos.0.z - bee_pos.0.z),
            );
            dx * dx + dy * dy + dz * dz
        });
        hives
    }
}

impl Goal for LocateHiveGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        !is_angry(mob)
            && bee.cooldown_before_locating_new_hive.load(Relaxed) == 0
            && !bee.has_hive()
            && bee.wants_to_enter_hive()
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        bee.cooldown_before_locating_new_hive.store(200, Relaxed);
        let hives = Self::find_nearby_hives_with_space(bee);
        let Some(first) = hives.first().copied() else {
            return;
        };
        let mut blacklist = bee
            .blacklisted_hives
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = hives.iter().find(|pos| !blacklist.contains(pos)) {
            bee.set_hive_pos(*pos);
            return;
        }
        blacklist.clear();
        bee.set_hive_pos(first);
    }
}

// bee: Bee.BeeGoToHiveGoal
pub struct GoToHiveGoal {
    travelling_ticks: i32,
    last_path: Option<crate::entity::ai::pathfinder::path::Path>,
    ticks_stuck: i32,
}

impl GoToHiveGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            travelling_ticks: 0,
            last_path: None,
            ticks_stuck: 0,
        }
    }

    fn has_reached_target(bee: &BeeEntity, target: &BlockPos) -> bool {
        if bee.closer_than(target, 2) {
            return true;
        }
        let navigator = nav_of(bee);
        navigator
            .get_path()
            .is_some_and(|path| path.get_target() == *target && path.can_reach() && path.is_done())
    }

    // bee: pathfindDirectlyTowards
    fn pathfind_directly_towards(bee: &BeeEntity, target: &BlockPos) -> bool {
        let close_enough = if bee.closer_than(target, 3) { 1 } else { 2 };
        let mut navigator = nav_of(bee);
        navigator.set_max_visited_nodes_multiplier(10.0);
        let path = navigator.create_path(
            &bee.mob_entity.living_entity,
            Vector3::new(
                f64::from(target.0.x),
                f64::from(target.0.y),
                f64::from(target.0.z),
            ),
            close_enough,
        );
        navigator.move_to_path(path, 1.0, &bee.mob_entity.living_entity);
        navigator
            .get_path()
            .is_some_and(crate::entity::ai::pathfinder::path::Path::can_reach)
    }

    fn drop_and_blacklist_hive(bee: &BeeEntity) {
        if let Some(hive) = bee.hive_pos() {
            let mut blacklist = bee
                .blacklisted_hives
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            blacklist.push(hive);
            while blacklist.len() > MAX_BLACKLISTED_TARGETS {
                blacklist.remove(0);
            }
        }
        bee.drop_hive();
    }

    fn can_bee_use(bee: &BeeEntity) -> bool {
        let Some(hive) = bee.hive_pos() else {
            return false;
        };
        let world = bee.get_entity().world.load_full();
        !bee.is_too_far_away(&hive)
            && !bee.mob_entity.has_position_target()
            && bee.wants_to_enter_hive()
            && !Self::has_reached_target(bee, &hive)
            && world
                .get_block(&hive)
                .has_tag(&tag::Block::MINECRAFT_BEEHIVES)
    }
}

impl Goal for GoToHiveGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        bee(mob).is_some_and(|bee| !is_angry(mob) && Self::can_bee_use(bee))
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }

    fn start(&mut self, _mob: &dyn Mob) {
        self.travelling_ticks = 0;
        self.ticks_stuck = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.travelling_ticks = 0;
        self.ticks_stuck = 0;
        let Some(bee) = bee(mob) else { return };
        let mut navigator = nav_of(bee);
        navigator.stop();
        navigator.reset_max_visited_nodes_multiplier();
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let Some(hive) = bee.hive_pos() else { return };
        self.travelling_ticks += 1;
        if self.travelling_ticks > to_goal_ticks(MAX_TRAVELLING_TICKS) {
            Self::drop_and_blacklist_hive(bee);
            return;
        }
        if nav_of(bee).is_in_progress() {
            return;
        }
        if !bee.closer_than(&hive, PATHFIND_TO_HIVE_WHEN_CLOSER_THAN) {
            if bee.is_too_far_away(&hive) {
                bee.drop_hive();
            } else {
                bee.pathfind_randomly_towards(&hive);
            }
            return;
        }
        if !Self::pathfind_directly_towards(bee, &hive) {
            Self::drop_and_blacklist_hive(bee);
            return;
        }
        let current = nav_of(bee)
            .get_path()
            .map(crate::entity::ai::pathfinder::path::Path::copy);
        let same_path = current
            .as_ref()
            .zip(self.last_path.as_ref())
            .is_some_and(|(current, last)| current.same_as_path(last));
        if same_path {
            self.ticks_stuck += 1;
            if self.ticks_stuck > TICKS_BEFORE_HIVE_DROP {
                bee.drop_hive();
                self.ticks_stuck = 0;
            }
        } else {
            self.last_path = current;
        }
    }
}

// bee: Bee.BeeGoToKnownFlowerGoal
pub struct GoToKnownFlowerGoal {
    travelling_ticks: i32,
}

impl GoToKnownFlowerGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            travelling_ticks: 0,
        }
    }
}

impl Goal for GoToKnownFlowerGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        if is_angry(mob) {
            return false;
        }
        bee.saved_flower_pos().is_some_and(|flower| {
            !bee.mob_entity.has_position_target()
                && bee.ticks_without_nectar.load(Relaxed) > TICKS_BEFORE_GOING_TO_KNOWN_FLOWER
                && !bee.closer_than(&flower, 2)
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }

    fn start(&mut self, _mob: &dyn Mob) {
        self.travelling_ticks = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.travelling_ticks = 0;
        let Some(bee) = bee(mob) else { return };
        let mut navigator = nav_of(bee);
        navigator.stop();
        navigator.reset_max_visited_nodes_multiplier();
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let Some(flower) = bee.saved_flower_pos() else {
            return;
        };
        self.travelling_ticks += 1;
        if self.travelling_ticks > to_goal_ticks(MAX_TRAVELLING_TICKS) {
            bee.drop_flower();
        } else if !nav_of(bee).is_in_progress() {
            if bee.is_too_far_away(&flower) {
                bee.drop_flower();
            } else {
                bee.pathfind_randomly_towards(&flower);
            }
        }
    }
}

// bee: Bee.BeeGrowCropGoal
pub struct GrowCropGoal;

impl GrowCropGoal {
    // bee: crop growth by tag branch (CropBlock, StemBlock, SweetBerryBushBlock, CaveVines)
    fn grown_state(
        world: &std::sync::Arc<World>,
        pos: &BlockPos,
    ) -> Option<pumpkin_data::BlockStateId> {
        let (block, state_id) = world.get_block_and_state_id(pos);
        if !block.has_tag(&tag::Block::MINECRAFT_BEE_GROWABLES) {
            return None;
        }
        if block == &Block::WHEAT || block == &Block::CARROTS || block == &Block::POTATOES {
            let mut props = WheatLikeProperties::from_state_id(state_id);
            return (props.age < 7).then(|| {
                props.age += 1;
                props.to_state_id(block)
            });
        }
        if block == &Block::MELON_STEM || block == &Block::PUMPKIN_STEM {
            let mut props = WheatLikeProperties::from_state_id(state_id);
            return (props.age < 7).then(|| {
                props.age += 1;
                props.to_state_id(block)
            });
        }
        if block == &Block::BEETROOTS || block == &Block::SWEET_BERRY_BUSH {
            let max_age = 3;
            let mut props = NetherWartLikeProperties::from_state_id(state_id);
            return (props.age < max_age).then(|| {
                props.age += 1;
                props.to_state_id(block)
            });
        }
        if block == &Block::TORCHFLOWER_CROP {
            let mut props = TorchflowerCropLikeProperties::from_state_id(state_id);
            return (props.age < 1).then(|| {
                props.age += 1;
                props.to_state_id(block)
            });
        }
        if block == &Block::CAVE_VINES || block == &Block::CAVE_VINES_PLANT {
            let behaviour = world.block_registry.get_pumpkin_block(block.id)?;
            let args = crate::block::BonemealArgs {
                world,
                block,
                position: pos,
                state_id,
            };
            if behaviour.is_valid_bonemeal_target(args) {
                behaviour.perform_bonemeal(args);
                return Some(world.get_block_state_id(pos));
            }
        }
        None
    }
}

impl Goal for GrowCropGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        if is_angry(mob) || bee.crops_grown_since_pollination.load(Relaxed) >= MAX_CROPS_GROWABLE {
            return false;
        }
        if rand::random::<f32>() < 0.3 {
            return false;
        }
        bee.has_nectar() && bee.is_hive_valid()
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        if rand::random_range(0..to_goal_ticks(GROW_CHANCE)) != 0 {
            return;
        }
        let world = bee.get_entity().world.load_full();
        let here = bee.get_entity().block_pos.load();
        for i in 1..=2 {
            let below = BlockPos::new(here.0.x, here.0.y - i, here.0.z);
            if let Some(grown) = Self::grown_state(&world, &below) {
                world.sync_world_event(
                    pumpkin_data::world::WorldEvent::ParticlesAndSoundPlantGrowth,
                    below,
                    15,
                );
                world.set_block_state(&below, grown, BlockFlags::NOTIFY_ALL);
                bee.crops_grown_since_pollination.fetch_add(1, Relaxed);
            }
        }
    }
}

// bee: Bee.BeeWanderGoal
pub struct BeeWanderGoal;

impl BeeWanderGoal {
    fn wander_threshold(bee: &BeeEntity) -> i32 {
        let reduction = if !bee.has_hive() && !bee.has_saved_flower_pos() {
            DEFAULT_WANDER_DISTANCE_REDUCTION
        } else {
            RESTRICTED_WANDER_DISTANCE_REDUCTION
        };
        TOO_FAR_DISTANCE - reduction
    }

    fn find_pos(bee: &BeeEntity, mob: &dyn Mob) -> Option<Vector3<f64>> {
        let direction = match bee.hive_pos() {
            Some(hive)
                if bee.is_hive_valid() && !bee.closer_than(&hive, Self::wander_threshold(bee)) =>
            {
                let delta = hive.to_centered_f64() - bee.get_entity().pos.load();
                delta.normalize()
            }
            _ => {
                let entity = bee.get_entity();
                Vector3::rotation_vector(
                    f64::from(entity.pitch.load()),
                    f64::from(entity.yaw.load()),
                )
            }
        };
        hover_random_pos::get_pos(
            mob,
            8,
            7,
            direction.x,
            direction.z,
            std::f64::consts::FRAC_PI_2,
            3,
            1,
        )
        .or_else(|| {
            air_and_water_random_pos::get_pos(
                mob,
                8,
                4,
                -2,
                direction.x,
                direction.z,
                std::f64::consts::FRAC_PI_2,
            )
        })
    }
}

impl Goal for BeeWanderGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(bee) = bee(mob) else { return false };
        nav_of(bee).is_done() && rand::random_range(0..10) == 0
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        bee(mob).is_some_and(|bee| nav_of(bee).is_in_progress())
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(bee) = bee(mob) else { return };
        let Some(target) = Self::find_pos(bee, mob) else {
            return;
        };
        let mut navigator = nav_of(bee);
        let path = navigator.create_path(&bee.mob_entity.living_entity, target, 1);
        navigator.move_to_path(path, 1.0, &bee.mob_entity.living_entity);
    }
}

impl Default for ValidateHiveGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ValidateFlowerGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for PollinateGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for GoToHiveGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for GoToKnownFlowerGoal {
    fn default() -> Self {
        Self::new()
    }
}
