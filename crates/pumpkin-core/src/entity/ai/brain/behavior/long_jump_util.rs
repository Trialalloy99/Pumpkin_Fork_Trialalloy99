use pumpkin_data::entity::EntityPose;
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::vector3::Vector3;

use crate::entity::mob::Mob;

// brain: LongJumpUtil.calculateJumpVectorForAngle
#[must_use]
pub fn calculate_jump_vector_for_angle(
    body: &dyn Mob,
    target_pos: Vector3<f64>,
    max_jump_velocity: f32,
    angle: i32,
    check_collision: bool,
) -> Option<Vector3<f64>> {
    let entity = body.get_entity();
    let mob_pos = entity.pos.load();
    let direction_vector_plane =
        Vector3::new(target_pos.x - mob_pos.x, 0.0, target_pos.z - mob_pos.z)
            .normalize()
            .multiply(0.5, 0.5, 0.5);
    let target_position = target_pos.sub(&direction_vector_plane);
    let direction_vector = target_position.sub(&mob_pos);
    let angrad = f64::from(angle as f32 * std::f32::consts::PI / 180.0);
    let xz_ang = direction_vector.z.atan2(direction_vector.x);
    let r2 = direction_vector.x * direction_vector.x + direction_vector.z * direction_vector.z;
    let r = r2.sqrt();
    let y = direction_vector.y;
    let g = body.get_gravity();
    let sin2ang = f64::from((2.0 * angrad as f32).sin());
    let cosangsqr = angrad.cos().powi(2);
    let sinangrad = angrad.sin();
    let cosangrad = angrad.cos();
    let sinxz_ang = xz_ang.sin();
    let cosxz_ang = xz_ang.cos();
    let v0sqr = r2 * g / (r * sin2ang - 2.0 * y * cosangsqr);
    if v0sqr < 0.0 {
        return None;
    }
    let v0 = v0sqr.sqrt();
    if v0 > f64::from(max_jump_velocity) {
        return None;
    }
    let v0r = v0 * cosangrad;
    let v0y = v0 * sinangrad;
    if check_collision {
        let samples = (r / v0r).ceil() as i32 * 2;
        let mut ri = 0.0;
        let mut previous_pos: Option<Vector3<f64>> = None;
        let dimensions = entity.get_dimensions(EntityPose::LongJumping);
        for _ in 0..samples - 1 {
            ri += r / f64::from(samples);
            let yi =
                sinangrad / cosangrad * ri - ri.powi(2) * g / (2.0 * v0sqr * cosangrad.powi(2));
            let sample_pos = Vector3::new(
                mob_pos.x + ri * cosxz_ang,
                mob_pos.y + yi,
                mob_pos.z + ri * sinxz_ang,
            );
            if let Some(previous) = previous_pos
                && !is_clear_transition(body, &dimensions, previous, sample_pos)
            {
                return None;
            }
            previous_pos = Some(sample_pos);
        }
    }
    Some(
        Vector3::new(v0r * cosxz_ang, v0y, v0r * sinxz_ang).multiply(
            f64::from(0.95_f32),
            f64::from(0.95_f32),
            f64::from(0.95_f32),
        ),
    )
}

// brain: LongJumpUtil.isClearTransition
fn is_clear_transition(
    body: &dyn Mob,
    dimensions: &EntityDimensions,
    position1: Vector3<f64>,
    position2: Vector3<f64>,
) -> bool {
    let direction = position2.sub(&position1);
    let min_dimension = f64::from(dimensions.width.min(dimensions.height));
    let checks = (direction.length() / min_dimension).ceil() as i32;
    let normalized = direction.normalize();
    let step = min_dimension * f64::from(0.9_f32);
    let mut next = position1;
    for i in 0..checks {
        next = if i == checks - 1 {
            position2
        } else {
            next.add(&normalized.multiply(step, step, step))
        };
        let bounds = BoundingBox::new_from_pos(next.x, next.y, next.z, dimensions);
        if !body.get_entity().world.load().is_space_empty(bounds) {
            return false;
        }
    }
    true
}
