use crate::entity::EntityBase;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy)]
pub enum ProjectileDeflectionType {
    None,
    Simple,
    // breeze: PROJECTILE_DEFLECTION (BREEZE_DEFLECT sound, then REVERSE)
    SimpleWithSound(pumpkin_data::sound::Sound),
    Redirected,
    TransferVelocityDirection,
}

impl ProjectileDeflectionType {
    pub fn deflect(&self, projectile: &dyn EntityBase, hit_entity: Option<&dyn EntityBase>) {
        match self {
            Self::None => {}
            Self::Simple | Self::SimpleWithSound(_) => {
                if let Self::SimpleWithSound(sound) = self {
                    let entity = hit_entity.map_or(projectile.get_entity(), EntityBase::get_entity);
                    entity.world.load().play_sound(
                        *sound,
                        pumpkin_data::sound::SoundCategory::Hostile,
                        &entity.pos.load(),
                    );
                }
                let vel = rand::random::<f32>().mul_add(20.0, 170.0);

                let current_velocity = projectile
                    .get_entity()
                    .velocity
                    .load()
                    .multiply(-0.5, -0.5, -0.5);

                let entity = projectile.get_entity();
                entity.velocity.store(current_velocity);

                let yaw = entity.yaw.load() + vel;
                let pitch = entity.pitch.load();
                entity.set_rotation(yaw, pitch);
                // TODO: Add entity.lastYaw += vel
                entity.velocity_dirty.store(true, Ordering::Relaxed);
            }
            Self::Redirected => {
                if let Some(hit_entity) = hit_entity {
                    let rotation_vector = hit_entity.get_entity().rotation();

                    let entity = projectile.get_entity();
                    entity.velocity.store(rotation_vector.to_f64());
                    entity.velocity_dirty.store(true, Ordering::Relaxed);
                }
            }
            Self::TransferVelocityDirection => {
                if let Some(hit_entity) = hit_entity {
                    let hit_velocity = hit_entity.get_entity().velocity.load().normalize();

                    let entity = projectile.get_entity();
                    entity.velocity.store(hit_velocity);
                    entity.velocity_dirty.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Vanilla `Projectile.hitTargetOrDeflectSelf` for entity hits: `true` when the target deflected the projectile.
pub fn try_deflect(projectile: &dyn EntityBase, target: &dyn EntityBase) -> bool {
    let deflection = target.projectile_deflection(projectile);
    if matches!(deflection, ProjectileDeflectionType::None) {
        return false;
    }
    let projectile_entity = projectile.get_entity();
    let target_id = target.get_entity().entity_id;
    if projectile_entity.last_deflected_by.load(Ordering::Relaxed) != target_id {
        deflection.deflect(projectile, Some(target));
        projectile_entity
            .last_deflected_by
            .store(target_id, Ordering::Relaxed);
    }
    true
}
