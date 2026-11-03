use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use super::GoatEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        AnimalPanic, Behavior, CountDownCooldownTicks, DoNothing, FollowTemptation,
        LookAtTargetSink, MoveToTargetSink, Swim, Timed,
        animal_make_love::AnimalMakeLove,
        baby_follow_adult,
        long_jump::{LongJumpMidJump, LongJumpToRandomPos},
        random_stroll, run_one, set_entity_look_target_sometimes, set_walk_target_from_look_target,
    },
    memory::{
        MemoryModuleId, MemoryStatus, PackedMemories,
        position_tracker::{BlockPosTracker, EntityTracker, PositionTracker},
        types,
        walk_target::WalkTarget,
    },
    sensing::SensorType,
};
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::combat::knockback_after_resistance;
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_PREPARING_TO_RAM: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_RAMMING: f32 = 3.0;
const TIME_BETWEEN_LONG_JUMPS: (i32, i32) = (600, 1200);
const MAX_LONG_JUMP_HEIGHT: i32 = 5;
const MAX_LONG_JUMP_WIDTH: i32 = 5;
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 3.571_428_8;
const TIME_BETWEEN_RAMS: (i32, i32) = (600, 6000);
const TIME_BETWEEN_RAMS_SCREAMER: (i32, i32) = (100, 300);
const ADULT_FOLLOW_RANGE: (i32, i32) = (5, 16);
const RAM_PREPARE_TIME: i64 = 20;
const RAM_MIN_DISTANCE: i32 = 4;
const RAM_MAX_DISTANCE: i32 = 7;
const RAM_TIME_OUT_DURATION: i32 = 200;
const PREPARE_RAM_TIME_OUT_DURATION: i32 = 160;
const RAM_SPEED_FORCE_FACTOR: f32 = 1.65;
const ADULT_RAM_KNOCKBACK_FORCE: f64 = 2.5;
const BABY_RAM_KNOCKBACK_FORCE: f64 = 1.0;

const MEMORIES: [MemoryModuleId; 14] = [
    types::LONG_JUMP_COOLDOWN_TICKS.id(),
    types::LONG_JUMP_MID_JUMP.id(),
    types::RAM_COOLDOWN_TICKS.id(),
    types::RAM_TARGET.id(),
    types::TEMPTATION_COOLDOWN_TICKS.id(),
    types::TEMPTING_PLAYER.id(),
    types::BREED_TARGET.id(),
    types::IS_PANICKING.id(),
    types::WALK_TARGET.id(),
    types::LOOK_TARGET.id(),
    types::NEAREST_VISIBLE_ADULT.id(),
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::HURT_BY.id(),
    types::HURT_BY_ENTITY.id(),
];

fn goat_of(mob: &dyn Mob) -> Option<&GoatEntity> {
    mob.cast_any().downcast_ref::<GoatEntity>()
}

// goat: Brain.provider(NEAREST_LIVING_ENTITIES, NEAREST_PLAYERS, NEAREST_ITEMS, NEAREST_ADULT, HURT_BY, FOOD_TEMPTATIONS)
pub(super) fn make_brain(goat: &GoatEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[
            SensorType::NearestLivingEntities,
            SensorType::NearestPlayers,
            SensorType::NearestItems,
            SensorType::NearestAdult,
            SensorType::HurtBy,
            SensorType::FoodTemptations,
        ],
        activities,
    }
    .make_brain(goat, packed)
}

// goat: GoatAi.initMemories
pub(super) fn init_memories(goat: &GoatEntity) {
    let (long_jump, ram) = {
        let mut rng = goat.get_random();
        (
            rng.random_range(TIME_BETWEEN_LONG_JUMPS.0..=TIME_BETWEEN_LONG_JUMPS.1),
            rng.random_range(TIME_BETWEEN_RAMS.0..=TIME_BETWEEN_RAMS.1),
        )
    };
    goat.mob_entity.with_brain(goat, |brain| {
        brain.set(types::LONG_JUMP_COOLDOWN_TICKS, long_jump);
        brain.set(types::RAM_COOLDOWN_TICKS, ram);
    });
}

// goat: GoatAi.updateActivity
pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[Activity::Ram, Activity::LongJump, Activity::Idle]);
}

// goat: RAM_TARGET_CONDITIONS
fn ram_target_conditions() -> TargetPredicate {
    let mut conditions = TargetPredicate::create_attackable();
    conditions.set_predicate(|target, world| {
        let entity = &target.entity;
        entity.entity_type != &EntityType::GOAT
            && (world.level_info.load().game_rules.mob_griefing
                || entity.entity_type != &EntityType::ARMOR_STAND)
            && {
                let bounds = entity.bounding_box.load();
                world
                    .worldborder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains_block(bounds.min.x.floor() as i32, bounds.min.z.floor() as i32)
            }
    });
    conditions
}

fn time_between_rams(goat: &GoatEntity) -> (i32, i32) {
    if goat.is_screaming() {
        TIME_BETWEEN_RAMS_SCREAMER
    } else {
        TIME_BETWEEN_RAMS
    }
}

fn long_jump_sound(mob: &dyn Mob) -> Sound {
    if goat_of(mob).is_some_and(GoatEntity::is_screaming) {
        Sound::EntityGoatScreamingLongJump
    } else {
        Sound::EntityGoatLongJump
    }
}

fn broadcast(tick: &BrainTick<'_>, status: EntityStatus) {
    tick.world
        .send_entity_status(tick.mob.get_entity(), status, ActorEventID::None);
}

fn play(tick: &BrainTick<'_>, sound: Sound, pitch: f32) {
    tick.world.play_sound_fine(
        sound,
        SoundCategory::Neutral,
        &tick.mob.get_entity().pos.load(),
        1.0,
        pitch,
    );
}

// goat: GoatAi.getActivities
#[allow(clippy::too_many_lines)]
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(Timed::new(AnimalPanic::new(
                    SPEED_MULTIPLIER_WHEN_PANICKING,
                ))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
                Box::new(CountDownCooldownTicks::new(
                    types::TEMPTATION_COOLDOWN_TICKS,
                )),
                Box::new(CountDownCooldownTicks::new(types::LONG_JUMP_COOLDOWN_TICKS)),
                Box::new(CountDownCooldownTicks::new(types::RAM_COOLDOWN_TICKS)),
            ],
        ),
        ActivityData::with_pairs_and_conditions(
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
                    0,
                    Box::new(Timed::new(AnimalMakeLove::new(&EntityType::GOAT, |_| {}))),
                ),
                (
                    1,
                    Box::new(Timed::new(FollowTemptation::new(
                        SPEED_MULTIPLIER_WHEN_TEMPTED,
                        2.5,
                        false,
                    ))),
                ),
                (
                    2,
                    Box::new(baby_follow_adult::create(
                        ADULT_FOLLOW_RANGE.0,
                        ADULT_FOLLOW_RANGE.1,
                        SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                        types::NEAREST_VISIBLE_ADULT,
                        false,
                        baby_follow_adult::same_entity,
                    )),
                ),
                (
                    3,
                    Box::new(run_one(vec![
                        (
                            Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                            2,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target::create(
                                SPEED_MULTIPLIER_WHEN_IDLING,
                                3,
                            )),
                            2,
                        ),
                        (Box::new(DoNothing::new(30, 60)), 1),
                    ])),
                ),
            ],
            vec![
                (types::RAM_COOLDOWN_TICKS.id(), MemoryStatus::ValuePresent),
                (
                    types::LONG_JUMP_COOLDOWN_TICKS.id(),
                    MemoryStatus::ValuePresent,
                ),
            ],
        ),
        ActivityData::with_pairs_and_conditions(
            Activity::LongJump,
            vec![
                (
                    0,
                    Box::new(Timed::new(LongJumpMidJump::new(
                        TIME_BETWEEN_LONG_JUMPS,
                        Sound::EntityGoatStep,
                    ))),
                ),
                (
                    1,
                    Box::new(Timed::new(LongJumpToRandomPos::new(
                        TIME_BETWEEN_LONG_JUMPS,
                        MAX_LONG_JUMP_HEIGHT,
                        MAX_LONG_JUMP_WIDTH,
                        MAX_JUMP_VELOCITY_MULTIPLIER,
                        long_jump_sound,
                    ))),
                ),
            ],
            vec![
                (types::TEMPTING_PLAYER.id(), MemoryStatus::ValueAbsent),
                (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
                (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
                (
                    types::LONG_JUMP_COOLDOWN_TICKS.id(),
                    MemoryStatus::ValueAbsent,
                ),
            ],
        ),
        ActivityData::with_pairs_and_conditions(
            Activity::Ram,
            vec![
                (0, Box::new(Timed::new(RamTarget::new()))),
                (1, Box::new(Timed::new(PrepareRamNearestTarget::new()))),
            ],
            vec![
                (types::TEMPTING_PLAYER.id(), MemoryStatus::ValueAbsent),
                (types::BREED_TARGET.id(), MemoryStatus::ValueAbsent),
                (types::RAM_COOLDOWN_TICKS.id(), MemoryStatus::ValueAbsent),
            ],
        ),
    ]
}

// goat: RamTarget
struct RamTarget {
    ram_direction: Vector3<f64>,
    ram_targeting: TargetPredicate,
}

impl RamTarget {
    fn new() -> Self {
        Self {
            ram_direction: Vector3::new(0.0, 0.0, 0.0),
            ram_targeting: ram_target_conditions(),
        }
    }

    // goat: RamTarget.hasRammedHornBreakingBlock
    fn has_rammed_horn_breaking_block(tick: &BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        let velocity = entity.velocity.load();
        let horizontal = Vector3::new(velocity.x, 0.0, velocity.z).normalize();
        let facing = BlockPos::floored_v(entity.pos.load() + horizontal);
        let snaps = |pos: &BlockPos| {
            tick.world
                .get_block(pos)
                .has_tag(&tag::Block::MINECRAFT_SNAPS_GOAT_HORN)
        };
        snaps(&facing) || snaps(&facing.up())
    }

    // goat: RamTarget.finishRam
    fn finish_ram(tick: &mut BrainTick<'_>, goat: &GoatEntity) {
        broadcast(tick, EntityStatus::EndRam);
        let (min, max) = time_between_rams(goat);
        let cooldown = tick.mob.get_random().random_range(min..=max);
        tick.state.set(types::RAM_COOLDOWN_TICKS, cooldown);
        tick.state.erase(types::RAM_TARGET.id());
    }
}

const RAM_TARGET_ENTRY: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::RAM_COOLDOWN_TICKS.id(), MemoryStatus::ValueAbsent),
    (types::RAM_TARGET.id(), MemoryStatus::ValuePresent),
];

impl Behavior for RamTarget {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &RAM_TARGET_ENTRY
    }

    fn min_duration(&self) -> i32 {
        RAM_TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        RAM_TIME_OUT_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::RAM_TARGET.id())
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.has_memory_value(types::RAM_TARGET.id())
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let current = tick.mob.get_entity().block_pos.load();
        let Some(target) = tick.state.get(types::RAM_TARGET).copied() else {
            return;
        };
        self.ram_direction = Vector3::new(
            f64::from(current.0.x) - target.x,
            0.0,
            f64::from(current.0.z) - target.z,
        )
        .normalize();
        tick.state.set(
            types::WALK_TARGET,
            WalkTarget::new(
                Arc::new(BlockPosTracker::from_vec(target)),
                SPEED_MULTIPLIER_WHEN_RAMMING,
                0,
            ),
        );
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(goat) = goat_of(tick.mob) else {
            return;
        };
        let entity = goat.get_entity();
        let bounds = entity.bounding_box.load();
        let ram_target = tick
            .world
            .get_entities_at_box(&bounds)
            .into_iter()
            .find(|candidate| {
                candidate.get_living_entity().is_some()
                    && self
                        .ram_targeting
                        .test(tick.world, Some(tick.mob), candidate.as_ref())
            });
        let sound = if goat.is_screaming() {
            Sound::EntityGoatScreamingRamImpact
        } else {
            Sound::EntityGoatRamImpact
        };
        if let Some(target) = ram_target {
            let damage = goat
                .mob_entity
                .living_entity
                .get_attribute_value(&Attributes::ATTACK_DAMAGE) as f32;
            target.damage(tick.mob, damage, DamageType::MOB_ATTACK_NO_AGGRO);
            let living = &goat.mob_entity.living_entity;
            let speed_level = living
                .get_effect(&StatusEffect::SPEED)
                .map_or(0, |effect| effect.amplifier + 1);
            let slowness_level = living
                .get_effect(&StatusEffect::SLOWNESS)
                .map_or(0, |effect| effect.amplifier + 1);
            let speed_boost = 0.25 * (speed_level - slowness_level) as f32;
            let speed_factor =
                (living.get_speed() * RAM_SPEED_FORCE_FACTOR).clamp(0.2, 3.0) + speed_boost;
            let force = if goat.is_baby_mob() {
                BABY_RAM_KNOCKBACK_FORCE
            } else {
                ADULT_RAM_KNOCKBACK_FORCE
            };
            let resistance = target.get_living_entity().map_or(0.0, |living| {
                living.get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE)
            });
            // goat: applyItemBlocking, a raised shield facing the goat halves the knockback
            let blocked = target.get_living_entity().is_some_and(|living| {
                let to_goat = (entity.pos.load() - target.get_entity().pos.load()).normalize();
                let look = Vector3::rotation_vector(0.0, target.get_entity().yaw.load() as f64);
                living.is_blocking() && Vector3::new(to_goat.x, 0.0, to_goat.z).dot(&look) > 0.0
            });
            let blocking_factor = if blocked { 0.5 } else { 1.0 };
            target.get_entity().apply_knockback(
                knockback_after_resistance(
                    blocking_factor * f64::from(speed_factor) * force,
                    resistance,
                ),
                self.ram_direction.x,
                self.ram_direction.z,
            );
            Self::finish_ram(tick, goat);
            play(tick, sound, 1.0);
        } else if Self::has_rammed_horn_breaking_block(tick) {
            play(tick, sound, 1.0);
            if goat.drop_horn() {
                play(tick, Sound::EntityGoatHornBreak, 1.0);
            }
            Self::finish_ram(tick, goat);
        } else {
            let lost_or_reached = match (
                tick.state.get(types::WALK_TARGET),
                tick.state.get(types::RAM_TARGET),
            ) {
                (Some(walk), Some(ram)) => {
                    walk.target()
                        .current_position()
                        .squared_distance_to_vec(ram)
                        < 0.25 * 0.25
                }
                _ => true,
            };
            if lost_or_reached {
                Self::finish_ram(tick, goat);
            }
        }
    }

    fn debug_name(&self) -> &'static str {
        "RamTarget"
    }
}

struct RamCandidate {
    start_position: BlockPos,
    target_position: BlockPos,
    target: Arc<dyn EntityBase>,
}

// goat: PrepareRamNearestTarget
struct PrepareRamNearestTarget {
    ram_targeting: TargetPredicate,
    reached_ram_position_timestamp: Option<i64>,
    ram_candidate: Option<RamCandidate>,
}

impl PrepareRamNearestTarget {
    fn new() -> Self {
        Self {
            ram_targeting: ram_target_conditions(),
            reached_ram_position_timestamp: None,
            ram_candidate: None,
        }
    }

    // goat: isWalkableBlock
    fn is_walkable_block(tick: &BrainTick<'_>, pos: &BlockPos) -> bool {
        tick.world.get_block_state(&pos.down()).is_solid_render()
            && tick.world.get_block_state(pos).collision_shapes.is_empty()
            && !crate::world::natural_spawner::is_block_dangerous(
                tick.mob.get_entity().entity_type,
                tick.world.get_block_state(pos),
            )
    }

    // goat: calculateRammingStartPosition
    fn calculate_ramming_start_position(
        tick: &BrainTick<'_>,
        target: &dyn EntityBase,
    ) -> Option<BlockPos> {
        let target_pos = target.get_entity().block_pos.load();
        if !Self::is_walkable_block(tick, &target_pos) {
            return None;
        }
        let mut possible = Vec::new();
        for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
            let mut furthest = target_pos;
            for _ in 0..RAM_MAX_DISTANCE {
                let next = furthest.add(dx, 0, dz);
                if Self::is_walkable_block(tick, &next) {
                    furthest = next;
                } else {
                    break;
                }
            }
            let manhattan = (furthest.0.x - target_pos.0.x).abs()
                + (furthest.0.y - target_pos.0.y).abs()
                + (furthest.0.z - target_pos.0.z).abs();
            if manhattan >= RAM_MIN_DISTANCE {
                possible.push(furthest);
            }
        }
        let own = tick.mob.get_entity().block_pos.load();
        possible.sort_by_key(|pos| {
            let (x, y, z) = (
                i64::from(own.0.x - pos.0.x),
                i64::from(own.0.y - pos.0.y),
                i64::from(own.0.z - pos.0.z),
            );
            x * x + y * y + z * z
        });
        let steered = tick.mob.get_mob_entity().steered();
        possible.into_iter().find(|pos| {
            steered
                .navigator()
                .create_path(&steered.mob_entity().living_entity, pos.0.to_f64(), 0)
                .is_some_and(|path| path.can_reach())
        })
    }

    // goat: chooseRamPosition
    fn choose_ram_position(&mut self, tick: &BrainTick<'_>, target: Arc<dyn EntityBase>) {
        self.reached_ram_position_timestamp = None;
        self.ram_candidate =
            Self::calculate_ramming_start_position(tick, target.as_ref()).map(|start_position| {
                RamCandidate {
                    start_position,
                    target_position: target.get_entity().block_pos.load(),
                    target,
                }
            });
    }

    // goat: getEdgeOfBlock
    fn edge_of_block(start: &BlockPos, target: &BlockPos) -> Vector3<f64> {
        let sign = |delta: i32| f64::from(delta.signum());
        Vector3::new(
            f64::from(target.0.x) + 0.5 + 0.5 * sign(target.0.x - start.0.x),
            f64::from(target.0.y),
            f64::from(target.0.z) + 0.5 + 0.5 * sign(target.0.z - start.0.z),
        )
    }
}

const PREPARE_RAM_ENTRY: [(MemoryModuleId, MemoryStatus); 4] = [
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::RAM_COOLDOWN_TICKS.id(), MemoryStatus::ValueAbsent),
    (
        types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
        MemoryStatus::ValuePresent,
    ),
    (types::RAM_TARGET.id(), MemoryStatus::ValueAbsent),
];

impl Behavior for PrepareRamNearestTarget {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &PREPARE_RAM_ENTRY
    }

    fn min_duration(&self) -> i32 {
        PREPARE_RAM_TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        PREPARE_RAM_TIME_OUT_DURATION
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let closest = tick
            .state
            .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
            .and_then(|visible| {
                visible.find_closest(&tick.visibility(), |entity| {
                    self.ram_targeting
                        .test(tick.world, Some(tick.mob), entity.as_ref())
                })
            });
        if let Some(target) = closest {
            self.choose_ram_position(tick, target);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        if !tick.state.has_memory_value(types::RAM_TARGET.id()) {
            broadcast(tick, EntityStatus::EndRam);
            let cooldown =
                goat_of(tick.mob).map_or(TIME_BETWEEN_RAMS.0, |goat| time_between_rams(goat).0);
            tick.state.set(types::RAM_COOLDOWN_TICKS, cooldown);
        }
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        self.ram_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.target.get_entity().is_alive())
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        let Some(candidate) = &self.ram_candidate else {
            return;
        };
        let start = candidate.start_position;
        let target_position = candidate.target_position;
        let target = candidate.target.clone();
        tick.state.set(
            types::WALK_TARGET,
            WalkTarget::from_vec(
                start.to_centered_f64() - Vector3::new(0.0, 0.5, 0.0),
                SPEED_MULTIPLIER_WHEN_PREPARING_TO_RAM,
                0,
            ),
        );
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(EntityTracker::new(target.clone(), true)) as Arc<dyn PositionTracker>,
        );
        if target.get_entity().block_pos.load() != target_position {
            broadcast(tick, EntityStatus::EndRam);
            tick.mob.get_mob_entity().steered().navigator().stop();
            self.choose_ram_position(tick, target);
        } else if tick.mob.get_entity().block_pos.load() == start {
            broadcast(tick, EntityStatus::StartRam);
            let reached = *self.reached_ram_position_timestamp.get_or_insert(tick.time);
            if tick.time - reached >= RAM_PREPARE_TIME {
                tick.state.set(
                    types::RAM_TARGET,
                    Self::edge_of_block(&start, &target_position),
                );
                let sound = if goat_of(tick.mob).is_some_and(GoatEntity::is_screaming) {
                    Sound::EntityGoatScreamingPrepareRam
                } else {
                    Sound::EntityGoatPrepareRam
                };
                let baby = if tick.mob.is_baby_mob() { 1.5 } else { 1.0 };
                let jitter = {
                    let mut rng = tick.mob.get_random();
                    (rng.random::<f32>() - rng.random::<f32>()) * 0.2
                };
                play(tick, sound, jitter + baby);
                self.ram_candidate = None;
            }
        }
    }

    fn debug_name(&self) -> &'static str {
        "PrepareRamNearestTarget"
    }
}

#[allow(dead_code)]
const _: Ordering = Ordering::Relaxed;
