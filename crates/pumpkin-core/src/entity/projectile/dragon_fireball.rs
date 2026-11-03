use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::entity::area_effect_cloud::AreaEffectCloudEntity;
use crate::entity::projectile::owner::ProjectileOwner;
use crate::entity::projectile::{ProjectileHit, ThrownItemEntity};
use crate::entity::{Entity, EntityBase};
use crate::server::Server;

const INITIAL_ACCELERATION_POWER: f64 = 0.1;
const AIR_INERTIA: f64 = 0.95;
const WATER_INERTIA: f64 = 0.8;

// dragon fireball: DragonFireball extends AbstractHurtingProjectile
pub struct DragonFireballEntity {
    thrown: ThrownItemEntity,
    acceleration_power: AtomicU64,
}

impl DragonFireballEntity {
    #[must_use]
    pub const fn new(entity: Entity) -> Self {
        Self {
            thrown: ThrownItemEntity {
                entity,
                owner: ProjectileOwner::none(),
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: 0.0,
            },
            acceleration_power: AtomicU64::new(INITIAL_ACCELERATION_POWER.to_bits()),
        }
    }

    // dragon fireball: AbstractHurtingProjectile(type, mob, direction) keeps the spawn position
    #[must_use]
    pub fn new_shot(entity: Entity, owner: &Entity, direction: Vector3<f64>) -> Self {
        let power = INITIAL_ACCELERATION_POWER;
        entity
            .velocity
            .store(direction.normalize().multiply(power, power, power));
        entity.set_rotation(owner.yaw.load(), owner.pitch.load());
        Self {
            thrown: ThrownItemEntity {
                entity,
                owner: ProjectileOwner::new(Some(owner)),
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: 0.0,
            },
            acceleration_power: AtomicU64::new(power.to_bits()),
        }
    }

    fn acceleration_power(&self) -> f64 {
        f64::from_bits(self.acceleration_power.load(Ordering::Relaxed))
    }

    // dragon fireball: breath cloud where it lands (DragonFireball.onHit)
    fn land(&self, hit: &ProjectileHit) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if let ProjectileHit::Entity { entity: target, .. } = hit
            && self
                .thrown
                .owner
                .uuid()
                .is_some_and(|owner| owner == target.get_entity().entity_uuid)
        {
            return;
        }
        let pos = entity.pos.load();
        let search = entity.bounding_box.load().expand(4.0, 2.0, 4.0);
        let mut cloud_pos = pos;
        let targets = world
            .get_entities_at_box(&search)
            .into_iter()
            .chain(
                world
                    .get_players_at_box(&search)
                    .into_iter()
                    .map(|p| p as std::sync::Arc<dyn EntityBase>),
            )
            .filter(|target| target.get_living_entity().is_some());
        for target in targets {
            let target_pos = target.get_entity().pos.load();
            if pos.squared_distance_to_vec(&target_pos) < 16.0 {
                cloud_pos = target_pos;
                break;
            }
        }
        let duration = 600;
        let radius = 3.0f32;
        let cloud_entity = Entity::new(
            world.clone(),
            cloud_pos,
            &pumpkin_data::entity::EntityType::AREA_EFFECT_CLOUD,
        );
        let cloud = AreaEffectCloudEntity::create_dragon_breath(
            cloud_entity,
            self.thrown.owner.uuid(),
            duration,
            radius,
            (7.0 - radius) / duration as f32,
            1,
            1,
        );
        world.sync_world_event(
            WorldEvent::ParticlesDragonFireballSplash,
            BlockPos(Vector3::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            )),
            if entity.silent.load(Ordering::Relaxed) {
                -1
            } else {
                1
            },
        );
        world.spawn_entity(cloud);
        entity.remove();
    }
}

impl EntityBase for DragonFireballEntity {
    fn get_projectile_owner(&self) -> Option<&ProjectileOwner> {
        Some(&self.thrown.owner)
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_double("acceleration_power", self.acceleration_power());
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(power) = nbt.get_double("acceleration_power") {
            self.acceleration_power
                .store(power.to_bits(), Ordering::Relaxed);
        }
    }

    // dragon fireball: AbstractHurtingProjectile.tick
    fn tick(&self, caller: &dyn EntityBase, _server: &Server) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        self.thrown.owner.check_left_owner(entity);
        entity.update_last_pos();
        entity.update_fluid_state(caller);

        if self
            .thrown
            .owner
            .get(&world)
            .is_some_and(|owner| owner.get_entity().is_removed())
        {
            entity.remove();
            return;
        }

        // applyInertia
        let movement = entity.velocity.load();
        let inertia = if entity.touching_water.load(Ordering::Relaxed) {
            WATER_INERTIA
        } else {
            AIR_INERTIA
        };
        let accelerated = movement + movement.normalize() * self.acceleration_power();
        let movement = accelerated.multiply(inertia, inertia, inertia);
        entity.velocity.store(movement);

        let start_pos = entity.pos.load();
        let hit = self.thrown.find_hit(caller, &world, start_pos, movement);
        let new_pos = hit
            .as_ref()
            .map_or(start_pos + movement, ProjectileHit::hit_pos);
        super::rotate_towards_movement(entity);
        entity.set_pos(new_pos);
        entity.velocity_dirty.store(true, Ordering::Relaxed);

        if let Some(hit) = hit {
            if let ProjectileHit::Entity { entity: target, .. } = &hit
                && crate::entity::projectile_deflection::try_deflect(caller, target.as_ref())
            {
                return;
            }
            self.on_hit(hit);
        }
    }

    fn get_entity(&self) -> &Entity {
        self.thrown.get_entity()
    }

    fn get_living_entity(&self) -> Option<&crate::entity::living::LivingEntity> {
        None
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn on_hit(&self, hit: ProjectileHit) {
        if !self.thrown.has_hit.swap(true, Ordering::SeqCst) {
            self.land(&hit);
            if !self.get_entity().is_removed() {
                self.thrown.has_hit.store(false, Ordering::SeqCst);
            }
        }
    }
}
