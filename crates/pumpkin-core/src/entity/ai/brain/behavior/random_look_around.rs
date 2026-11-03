use pumpkin_util::math::{vector3::Vector3, wrap_degrees};
use rand::RngExt;

use super::super::BrainTick;
use super::super::memory::position_tracker::{BlockPosTracker, PositionTracker};
use super::super::memory::{MemoryModuleId, MemoryStatus, types};
use super::timed::Behavior;
use std::sync::Arc;

const ENTRY_CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
    (types::LOOK_TARGET.id(), MemoryStatus::ValueAbsent),
    (types::GAZE_COOLDOWN_TICKS.id(), MemoryStatus::ValueAbsent),
];

// brain: RandomLookAround
pub struct RandomLookAround {
    min_interval: i32,
    max_interval: i32,
    max_yaw: f32,
    min_pitch: f32,
    pitch_range: f32,
}

impl RandomLookAround {
    /// `interval` is the inclusive `UniformInt` range of the gaze cooldown.
    #[must_use]
    pub fn new(
        min_interval: i32,
        max_interval: i32,
        max_yaw: f32,
        min_pitch: f32,
        max_pitch: f32,
    ) -> Self {
        debug_assert!(min_pitch <= max_pitch);
        Self {
            min_interval,
            max_interval,
            max_yaw,
            min_pitch,
            pitch_range: max_pitch - min_pitch,
        }
    }
}

impl Behavior for RandomLookAround {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &ENTRY_CONDITIONS
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        let (pitch, rotation, cooldown) = {
            let mut rng = tick.mob.get_random();
            let pitch =
                (rng.random::<f32>() * self.pitch_range + self.min_pitch).clamp(-90.0, 90.0);
            let rotation = wrap_degrees(
                entity.yaw.load() + 2.0 * rng.random::<f32>() * self.max_yaw - self.max_yaw,
            );
            let cooldown = rng.random_range(self.min_interval..=self.max_interval);
            (pitch, rotation, cooldown)
        };
        let look = Vector3::rotation_vector(pitch, rotation);
        let target = entity.get_eye_pos()
            + Vector3::new(f64::from(look.x), f64::from(look.y), f64::from(look.z));
        tick.state.set(
            types::LOOK_TARGET,
            Arc::new(BlockPosTracker::from_vec(target)) as Arc<dyn PositionTracker>,
        );
        tick.state.set(types::GAZE_COOLDOWN_TICKS, cooldown);
    }

    fn debug_name(&self) -> &'static str {
        "RandomLookAround"
    }
}
