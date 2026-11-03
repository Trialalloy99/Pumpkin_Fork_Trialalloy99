use std::sync::atomic::Ordering;

use pumpkin_data::Block;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityPose;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use rand::seq::SliceRandom;

use super::super::BrainTick;
use super::super::memory::position_tracker::{BlockPosTracker, PositionTracker};
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::long_jump_util;
use super::timed::Behavior;
use crate::entity::mob::Mob;
use crate::world::natural_spawner::is_block_dangerous;

// long jump: LongJumpToRandomPos constants
const FIND_JUMP_TRIES: i32 = 20;
const PREPARE_JUMP_DURATION: i64 = 40;
const MIN_PATHFIND_DISTANCE_TO_VALID_JUMP: i32 = 8;
const TIME_OUT_DURATION: i32 = 200;
const ALLOWED_ANGLES: [i32; 4] = [65, 70, 75, 80];
// long jump: LongJumpMidJump.TIME_OUT_DURATION
const MID_JUMP_TIME_OUT_DURATION: i32 = 100;

const START_CONDITIONS: [(MemoryModuleId, MemoryStatus); 3] = [
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (
        types::LONG_JUMP_COOLDOWN_TICKS.id(),
        MemoryStatus::ValueAbsent,
    ),
    (types::LONG_JUMP_MID_JUMP.id(), MemoryStatus::ValueAbsent),
];

const MID_JUMP_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::LOOK_TARGET.id(), MemoryStatus::Registered),
    (types::LONG_JUMP_MID_JUMP.id(), MemoryStatus::ValuePresent),
];

fn sample(tick: &BrainTick<'_>, range: (i32, i32)) -> i32 {
    tick.mob.get_random().random_range(range.0..=range.1)
}

fn play(tick: &BrainTick<'_>, sound: Sound, volume: f32) {
    let entity = tick.mob.get_entity();
    tick.world.play_sound_fine(
        sound,
        SoundCategory::Neutral,
        &entity.pos.load(),
        volume,
        1.0,
    );
}

/// Vanilla `LongJumpToRandomPos`.
pub struct LongJumpToRandomPos {
    time_between_long_jumps: (i32, i32),
    max_long_jump_height: i32,
    max_long_jump_width: i32,
    max_jump_velocity_multiplier: f32,
    jump_sound: fn(&dyn Mob) -> Sound,
    jump_candidates: Vec<(BlockPos, i32)>,
    initial_position: Option<Vector3<f64>>,
    chosen_jump: Option<Vector3<f64>>,
    find_jump_tries: i32,
    prepare_jump_start: i64,
}

impl LongJumpToRandomPos {
    #[must_use]
    pub const fn new(
        time_between_long_jumps: (i32, i32),
        max_long_jump_height: i32,
        max_long_jump_width: i32,
        max_jump_velocity_multiplier: f32,
        jump_sound: fn(&dyn Mob) -> Sound,
    ) -> Self {
        Self {
            time_between_long_jumps,
            max_long_jump_height,
            max_long_jump_width,
            max_jump_velocity_multiplier,
            jump_sound,
            jump_candidates: Vec::new(),
            initial_position: None,
            chosen_jump: None,
            find_jump_tries: 0,
            prepare_jump_start: 0,
        }
    }

    // long jump: defaultAcceptableLandingSpot
    fn acceptable_landing_spot(tick: &BrainTick<'_>, target: &BlockPos) -> bool {
        let entity = tick.mob.get_entity();
        let landing = tick.world.get_block_state(target);
        tick.world.get_block_state(&target.down()).is_solid_render()
            && !is_block_dangerous(entity.entity_type, landing)
    }

    // long jump: WeightedRandom.getRandomItem, removes the pick
    fn take_candidate(&mut self, tick: &BrainTick<'_>) -> Option<(BlockPos, i32)> {
        let total: i32 = self.jump_candidates.iter().map(|(_, weight)| *weight).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = tick.mob.get_random().random_range(0..total);
        let index = self.jump_candidates.iter().position(|(_, weight)| {
            roll -= *weight;
            roll < 0
        })?;
        Some(self.jump_candidates.remove(index))
    }

    // long jump: calculateOptimalJumpVector
    fn calculate_optimal_jump_vector(
        &self,
        tick: &BrainTick<'_>,
        target: Vector3<f64>,
    ) -> Option<Vector3<f64>> {
        let mut angles = ALLOWED_ANGLES;
        angles.shuffle(&mut tick.mob.get_random());
        let living = &tick.mob.get_mob_entity().living_entity;
        let max_velocity = (living.get_attribute_value(&Attributes::JUMP_STRENGTH)
            * f64::from(self.max_jump_velocity_multiplier)) as f32;
        angles.into_iter().find_map(|angle| {
            long_jump_util::calculate_jump_vector_for_angle(
                tick.mob,
                target,
                max_velocity,
                angle,
                true,
            )
        })
    }

    // long jump: pickCandidate
    fn pick_candidate(&mut self, tick: &mut BrainTick<'_>) {
        while !self.jump_candidates.is_empty() {
            let Some((target, _)) = self.take_candidate(tick) else {
                continue;
            };
            let body = tick.mob.get_entity().block_pos.load();
            if body.0.x == target.0.x && body.0.z == target.0.z {
                continue;
            }
            if !Self::acceptable_landing_spot(tick, &target) {
                continue;
            }
            let Some(jump) = self.calculate_optimal_jump_vector(tick, target.to_centered_f64())
            else {
                continue;
            };
            tick.state.set(
                types::LOOK_TARGET,
                std::sync::Arc::new(BlockPosTracker::new(target))
                    as std::sync::Arc<dyn PositionTracker>,
            );
            let steered = tick.mob.get_mob_entity().steered();
            let path = steered.navigator().create_path(
                &steered.mob_entity().living_entity,
                target.0.to_f64(),
                0,
            );
            if path.as_ref().is_none_or(|path| {
                !path.can_reach()
                    || path.calculate_length() > MIN_PATHFIND_DISTANCE_TO_VALID_JUMP as f32
            }) {
                self.chosen_jump = Some(jump);
                self.prepare_jump_start = tick.time;
                return;
            }
        }
    }
}

impl Behavior for LongJumpToRandomPos {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &START_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        TIME_OUT_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        let can_start = entity.on_ground.load(Ordering::Relaxed)
            && !entity.is_in_water()
            && !entity.touching_lava.load(Ordering::Relaxed)
            && tick.world.get_block(&entity.block_pos.load()) != &Block::HONEY_BLOCK;
        if !can_start {
            let cooldown = sample(tick, self.time_between_long_jumps) / 2;
            tick.state.set(types::LONG_JUMP_COOLDOWN_TICKS, cooldown);
        }
        can_start
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        let valid = self
            .initial_position
            .is_some_and(|initial| initial == entity.pos.load())
            && self.find_jump_tries > 0
            && !entity.is_in_water()
            && (self.chosen_jump.is_some() || !self.jump_candidates.is_empty());
        if !valid && !tick.state.has_memory_value(types::LONG_JUMP_MID_JUMP.id()) {
            let cooldown = sample(tick, self.time_between_long_jumps) / 2;
            tick.state.set(types::LONG_JUMP_COOLDOWN_TICKS, cooldown);
            tick.state.erase(types::LOOK_TARGET.id());
        }
        valid
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        self.chosen_jump = None;
        self.find_jump_tries = FIND_JUMP_TRIES;
        let entity = tick.mob.get_entity();
        self.initial_position = Some(entity.pos.load());
        let origin = entity.block_pos.load();
        let (width, height) = (self.max_long_jump_width, self.max_long_jump_height);
        self.jump_candidates = BlockPos::between_closed_coords(
            origin.0.x - width,
            origin.0.y - height,
            origin.0.z - width,
            origin.0.x + width,
            origin.0.y + height,
            origin.0.z + width,
        )
        .filter(|pos| *pos != origin)
        .map(|pos| {
            let dx = f64::from(origin.0.x - pos.0.x);
            let dy = f64::from(origin.0.y - pos.0.y);
            let dz = f64::from(origin.0.z - pos.0.z);
            (pos, (dx * dx + dy * dy + dz * dz).ceil() as i32)
        })
        .collect();
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(jump) = self.chosen_jump {
            if tick.time - self.prepare_jump_start >= PREPARE_JUMP_DURATION {
                let entity = tick.mob.get_entity();
                let living = &tick.mob.get_mob_entity().living_entity;
                entity.yaw.store(entity.body_yaw.load());
                living.discard_friction.store(true, Ordering::Relaxed);
                let original_length = jump.length();
                let boost = living
                    .get_effect(&StatusEffect::JUMP_BOOST)
                    .map_or(0.0, |effect| {
                        f64::from(0.1f32 * (effect.amplifier + 1) as f32)
                    });
                let scale = (original_length + boost) / original_length;
                entity.velocity.store(jump * scale);
                entity.velocity_dirty.store(true, Ordering::Relaxed);
                tick.state.set(types::LONG_JUMP_MID_JUMP, true);
                play(tick, (self.jump_sound)(tick.mob), 1.0);
            }
        } else {
            self.find_jump_tries -= 1;
            self.pick_candidate(tick);
        }
    }

    fn debug_name(&self) -> &'static str {
        "LongJumpToRandomPos"
    }
}

/// Vanilla `LongJumpMidJump`.
pub struct LongJumpMidJump {
    time_between_long_jumps: (i32, i32),
    landing_sound: Sound,
}

impl LongJumpMidJump {
    #[must_use]
    pub const fn new(time_between_long_jumps: (i32, i32), landing_sound: Sound) -> Self {
        Self {
            time_between_long_jumps,
            landing_sound,
        }
    }
}

impl Behavior for LongJumpMidJump {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &MID_JUMP_CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        MID_JUMP_TIME_OUT_DURATION
    }

    fn max_duration(&self) -> i32 {
        MID_JUMP_TIME_OUT_DURATION
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        !tick.mob.get_entity().on_ground.load(Ordering::Relaxed)
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob
            .get_mob_entity()
            .living_entity
            .discard_friction
            .store(true, Ordering::Relaxed);
        tick.mob.get_entity().set_pose(EntityPose::LongJumping);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.on_ground.load(Ordering::Relaxed) {
            let velocity = entity.velocity.load();
            entity.velocity.store(Vector3::new(
                velocity.x * f64::from(0.1f32),
                velocity.y,
                velocity.z * f64::from(0.1f32),
            ));
            play(tick, self.landing_sound, 2.0);
        }
        tick.mob
            .get_mob_entity()
            .living_entity
            .discard_friction
            .store(false, Ordering::Relaxed);
        entity.set_pose(EntityPose::Standing);
        tick.state.erase(types::LONG_JUMP_MID_JUMP.id());
        let cooldown = sample(tick, self.time_between_long_jumps);
        tick.state.set(types::LONG_JUMP_COOLDOWN_TICKS, cooldown);
    }

    fn debug_name(&self) -> &'static str {
        "LongJumpMidJump"
    }
}
