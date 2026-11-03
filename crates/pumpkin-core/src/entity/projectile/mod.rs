use super::{Entity, EntityBase, living::LivingEntity};
use crate::world::World;
use crate::world::game_event::GameEventContext;
use pumpkin_data::BlockDirection;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_protocol::java::client::play::CEntityVelocity;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use std::{
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};
pub mod arrow;
pub mod dragon_fireball;
pub mod egg;
pub mod ender_pearl;
pub mod evoker_fangs;
pub mod experience_bottle;
pub mod eye_of_ender;
pub mod fireball;
pub mod firework_rocket;
pub mod fishing_bobber;
pub mod lingering_potion;
pub mod llama_spit;
pub mod owner;
pub mod shulker_bullet;
pub mod small_fireball;
pub mod snowball;
pub mod splash_potion;
pub mod trident;
pub mod wind_charge;
pub mod wither_skull;

use pumpkin_data::item_stack::ItemStack;

#[must_use]
pub fn is_projectile(entity_type: &EntityType) -> bool {
    *entity_type == EntityType::ARROW
        || *entity_type == EntityType::TRIDENT
        || *entity_type == EntityType::EGG
        || *entity_type == EntityType::EXPERIENCE_BOTTLE
        || *entity_type == EntityType::SNOWBALL
        || *entity_type == EntityType::FIREWORK_ROCKET
        || *entity_type == EntityType::WIND_CHARGE
        || *entity_type == EntityType::SPLASH_POTION
        || *entity_type == EntityType::LINGERING_POTION
        || *entity_type == EntityType::ENDER_PEARL
        || *entity_type == EntityType::SHULKER_BULLET
        || *entity_type == EntityType::FIREBALL
        || *entity_type == EntityType::SMALL_FIREBALL
        || *entity_type == EntityType::FISHING_BOBBER
        || *entity_type == EntityType::WITHER_SKULL
        || *entity_type == EntityType::LLAMA_SPIT
}

/// Vanilla `ProjectileUtil.rotateTowardsMovement(projectile, 0.2)`.
pub(crate) fn rotate_towards_movement(entity: &Entity) {
    fn lerp_rotation(current: f32, target: f32) -> f32 {
        let mut current = current;
        while target - current < -180.0 {
            current -= 360.0;
        }
        while target - current >= 180.0 {
            current += 360.0;
        }
        0.2f32.mul_add(target - current, current)
    }
    let movement = entity.velocity.load();
    let horizontal = movement.horizontal_length();
    entity.pitch.store(lerp_rotation(
        entity.pitch.load(),
        movement.y.atan2(horizontal).to_degrees() as f32,
    ));
    entity.yaw.store(lerp_rotation(
        entity.yaw.load(),
        movement.x.atan2(movement.z).to_degrees() as f32,
    ));
}

/// Helper to apply projectile spawned enchantment effects matching vanilla `Projectile::applyOnProjectileSpawned`.
pub fn apply_on_projectile_spawned(
    projectile_entity: &Entity,
    pickup_item_stack: &ItemStack,
    weapon: Option<&ItemStack>,
    arrow: Option<&arrow::ArrowEntity>,
) {
    crate::enchantment::EnchantmentHelper::on_projectile_spawned(
        pickup_item_stack,
        projectile_entity,
        arrow,
    );
    if let Some(weapon) = weapon
        && weapon.item_count > 0
        && weapon.item.id != pickup_item_stack.item.id
    {
        crate::enchantment::EnchantmentHelper::on_projectile_spawned(
            weapon,
            projectile_entity,
            arrow,
        );
    }
}

pub struct ThrownItemEntity {
    pub entity: Entity,
    pub owner: owner::ProjectileOwner,
    pub collides_with_projectiles: bool,
    pub has_hit: AtomicBool,
    pub gravity: f64,
}

impl ThrownItemEntity {
    pub fn new(entity: Entity, owner: &Entity, gravity: f64) -> Self {
        let mut owner_pos = owner.pos.load();
        owner_pos.y += owner.get_eye_height() - 0.1;
        entity.pos.store(owner_pos);
        Self {
            entity,
            owner: owner::ProjectileOwner::new(Some(owner)),
            collides_with_projectiles: false,
            has_hit: AtomicBool::new(false),
            gravity,
        }
    }

    pub fn set_velocity_from(&self, pitch: f32, yaw: f32, roll: f32, speed: f32, divergence: f32) {
        let yaw_rad = yaw.to_radians();
        let pitch_rad = pitch.to_radians();
        let roll_rad = (pitch + roll).to_radians();

        let x = -yaw_rad.sin() * pitch_rad.cos();
        let y = -roll_rad.sin();
        let z = yaw_rad.cos() * pitch_rad.cos();

        self.set_velocity(
            f64::from(x),
            f64::from(y),
            f64::from(z),
            f64::from(speed),
            f64::from(divergence),
        );
    }

    /// Vanilla `Projectile.shootFromRotation`: the owner's movement is added to the throw,
    /// vertical movement only while the owner is airborne.
    pub fn set_velocity_from_owner(
        &self,
        owner: &dyn EntityBase,
        pitch: f32,
        yaw: f32,
        roll: f32,
        speed: f32,
        divergence: f32,
    ) {
        self.set_velocity_from(pitch, yaw, roll, speed, divergence);
        let movement = Entity::known_movement(owner);
        let on_ground = owner.get_entity().on_ground.load(Ordering::Relaxed);
        let entity = self.get_entity();
        entity.velocity.store(
            entity.velocity.load()
                + Vector3::new(
                    movement.x,
                    if on_ground { 0.0 } else { movement.y },
                    movement.z,
                ),
        );
    }

    pub fn set_velocity(&self, x: f64, y: f64, z: f64, power: f64, uncertainty: f64) {
        fn next_triangular(mode: f64, deviation: f64) -> f64 {
            deviation.mul_add(rand::random::<f64>() - rand::random::<f64>(), mode)
        }
        let velocity = Vector3::new(x, y, z)
            .normalize()
            .add_raw(
                next_triangular(0.0, 0.017_227_5 * uncertainty),
                next_triangular(0.0, 0.017_227_5 * uncertainty),
                next_triangular(0.0, 0.017_227_5 * uncertainty),
            )
            .multiply(power, power, power);

        self.entity.velocity.store(velocity);
        // projectile: PROJECTILE_SHOOT with the owner as source (Projectile.tick, first tick)
        let world = self.entity.world.load();
        let owner = self.owner.get(&world);
        world.emit_game_event_with_context(
            GameEvent::ProjectileShoot.name(),
            self.entity.pos.load(),
            &GameEventContext {
                source_entity: owner.as_deref(),
            },
        );
        let len = velocity.horizontal_length();
        self.entity.set_rotation(
            velocity.x.atan2(velocity.z) as f32 * 57.295_776,
            velocity.y.atan2(len) as f32 * 57.295_776,
        );
    }
}

impl ThrownItemEntity {
    /// Process a tick for projectile movement and collisions
    pub fn process_tick(&self, caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let world = entity.world.load();

        self.owner.check_left_owner(entity);
        entity.update_last_pos();

        // Apply gravity and inertia
        let mut velocity = entity.velocity.load();
        velocity.y -= self.get_gravity();

        let inertia = Self::inertia(entity);
        velocity = velocity.multiply(inertia, inertia, inertia);

        // Store velocity
        entity.velocity.store(velocity);

        let start_pos = entity.pos.load();
        let delta = velocity;

        // Update position
        let new_pos = start_pos.add(&delta);
        entity.set_pos(new_pos);
        // throwable: water state for next tick's drag (ThrowableProjectile.tick super.tick)
        entity.update_fluid_state(caller);

        // Send updated velocity to clients
        let packet = CEntityVelocity::new(entity.entity_id.into(), velocity);
        let chunk_pos = entity.chunk_pos.load();
        world.broadcast_to_chunk(chunk_pos, &packet);

        let hit = self.find_hit(caller, &world, start_pos, delta);

        // Handle hit or continue
        if let Some(h) = hit {
            // projectile: Projectile.hitTargetOrDeflectSelf
            if let ProjectileHit::Entity { entity: target, .. } = &h
                && crate::entity::projectile_deflection::try_deflect(caller, target.as_ref())
            {
                return;
            }
            // Ensure hit is only processed once per projectile
            if self.has_hit.swap(true, Ordering::SeqCst) {
                return;
            }

            if let ProjectileHit::Block { pos, hit_pos, .. } = &h {
                let block = world.get_block(pos);
                let state = world.get_block_state(pos);
                if let Some(server) = world.server.upgrade() {
                    world
                        .block_registry
                        .on_projectile_hit(block, &world, caller, pos, state, hit_pos, &server);
                }
            }

            // projectile: hit position is where it entered the target, so knockback pushes away (Projectile.onHit)
            if let ProjectileHit::Entity { hit_pos, .. } = &h {
                entity.set_pos(*hit_pos);
            }

            // projectile: Projectile.onHit game event PROJECTILE_LAND, block center or hit point
            let land_pos = match &h {
                ProjectileHit::Block { pos, .. } => pos.to_centered_f64(),
                ProjectileHit::Entity { .. } => h.hit_pos(),
            };
            // Just trigger hit effects and remove
            caller.on_hit(h);
            world.emit_game_event_with_context(
                GameEvent::ProjectileLand.name(),
                land_pos,
                &GameEventContext {
                    source_entity: Some(caller),
                },
            );
            entity.remove();
        }
    }

    /// Closest block or entity along `start_pos + delta` (Vanilla `ProjectileUtil.getHitResultOnMoveVector`).
    pub(crate) fn find_hit(
        &self,
        caller: &dyn EntityBase,
        world: &Arc<World>,
        start_pos: Vector3<f64>,
        delta: Vector3<f64>,
    ) -> Option<ProjectileHit> {
        let entity = self.get_entity();
        let new_pos = start_pos.add(&delta);

        // Calculate search box for collisions
        let search_box = BoundingBox::new(
            Vector3::new(
                start_pos.x.min(new_pos.x),
                start_pos.y.min(new_pos.y),
                start_pos.z.min(new_pos.z),
            ),
            Vector3::new(
                start_pos.x.max(new_pos.x),
                start_pos.y.max(new_pos.y),
                start_pos.z.max(new_pos.z),
            ),
        )
        .expand(0.3, 0.3, 0.3);

        let mut closest_t = 1.0f64;
        let mut hit = None;

        // Block collisions
        let (block_cols, block_positions) = world.get_block_collisions(search_box, caller);
        for (idx, bb) in block_cols.iter().enumerate() {
            if let Some(t) = calculate_ray_intersection(&start_pos, &delta, bb)
                && t < closest_t
            {
                closest_t = t;
                // Map back to block pos
                for (len, pos) in &block_positions {
                    if idx < *len {
                        let hit_pos = start_pos.add(&delta.multiply(t, t, t));
                        hit = Some(ProjectileHit::Block {
                            pos: *pos,
                            face: get_hit_face(hit_pos, *pos),
                            hit_pos,
                            normal: delta.normalize().multiply(-1.0, -1.0, -1.0),
                        });
                        break;
                    }
                }
            }
        }

        // Entity collisions
        let candidates = world.get_all_at_box(&search_box);
        for cand in candidates {
            if self.should_skip_collision(world, entity, &cand) {
                continue;
            }

            let ebb = cand.get_entity().bounding_box.load().expand(0.3, 0.3, 0.3);
            if let Some(t) = entity_ray_intersection(&start_pos, &delta, &ebb, cand.as_ref())
                && t < closest_t
            {
                closest_t = t;
                let hit_pos = start_pos.add(&delta.multiply(t, t, t));
                hit = Some(ProjectileHit::Entity {
                    entity: cand.clone(),
                    hit_pos,
                    normal: delta.normalize().multiply(-1.0, -1.0, -1.0),
                });
            }
        }

        hit
    }

    /// Vanilla `getInertia` / `getLiquidInertia`; wind charges keep all their speed (`AbstractWindCharge`).
    fn inertia(entity: &Entity) -> f64 {
        if entity.entity_type == &EntityType::WIND_CHARGE
            || entity.entity_type == &EntityType::BREEZE_WIND_CHARGE
        {
            1.0
        } else if entity.touching_water.load(Ordering::Relaxed) {
            0.8
        } else {
            0.99
        }
    }

    /// Returns if collision should be skipped (e.g. owner or projectile vs projectile)
    fn should_skip_collision(
        &self,
        world: &World,
        self_ent: &Entity,
        other: &Arc<dyn EntityBase>,
    ) -> bool {
        let other_ent = other.get_entity();
        // projectile: spectators are not pickable (Projectile.canHitEntity)
        if other.is_spectator() || !other.is_pickable() {
            return true;
        }
        // dragon: only its parts can be hit, never the body box, and never by its own projectiles
        if other
            .cast_any()
            .is::<crate::entity::boss::ender_dragon::EnderDragonEntity>()
        {
            return true;
        }
        if let Some(parent) = other.parent_entity_uuid()
            && self.owner.uuid() == Some(parent)
        {
            return true;
        }
        if other_ent.entity_id == self_ent.entity_id {
            return true;
        }

        if !self.owner.can_hit(world, other_ent) {
            return true;
        }

        // Projectiles should pass through lingering clouds
        if *other_ent.entity_type == EntityType::AREA_EFFECT_CLOUD {
            return true;
        }

        // Projectile vs projectile logic
        if !self.collides_with_projectiles && is_projectile(other_ent.entity_type) {
            return true;
        }

        false
    }

    const fn get_entity(&self) -> &Entity {
        &self.entity
    }

    #[allow(dead_code, clippy::unused_self)]
    const fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }
    const fn get_gravity(&self) -> f64 {
        self.gravity
    }
}

/// Vanilla `ProjectileUtil.getEntityHitResult`: a ray starting inside the box only hits entities
/// that `canBePickedFromInside`.
pub(crate) fn entity_ray_intersection(
    start: &Vector3<f64>,
    dir: &Vector3<f64>,
    bb: &BoundingBox,
    target: &dyn EntityBase,
) -> Option<f64> {
    let inside = start.x >= bb.min.x
        && start.x < bb.max.x
        && start.y >= bb.min.y
        && start.y < bb.max.y
        && start.z >= bb.min.z
        && start.z < bb.max.z;
    if inside && !target.can_be_picked_from_inside() {
        return None;
    }
    calculate_ray_intersection(start, dir, bb)
}

/// Ray intersection algorithm for AABBs, returning a t value
pub(crate) fn calculate_ray_intersection(
    start: &Vector3<f64>,
    dir: &Vector3<f64>,
    bb: &BoundingBox,
) -> Option<f64> {
    let mut t_min = 0.0f64;
    let mut t_max = 1.0f64;

    let b_min = [bb.min.x, bb.min.y, bb.min.z];
    let b_max = [bb.max.x, bb.max.y, bb.max.z];
    let s = [start.x, start.y, start.z];
    let d = [dir.x, dir.y, dir.z];

    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if s[i] < b_min[i] || s[i] > b_max[i] {
                return None;
            }
        } else {
            let t1 = (b_min[i] - s[i]) / d[i];
            let t2 = (b_max[i] - s[i]) / d[i];
            t_min = t_min.max(t1.min(t2));
            t_max = t_max.min(t1.max(t2));
        }
    }

    (t_min <= t_max && (0.0..=1.0).contains(&t_min)).then_some(t_min)
}

/// Get the face of the block that was hit
fn get_hit_face(hit_pos: Vector3<f64>, block_pos: BlockPos) -> BlockDirection {
    let local = hit_pos.sub(&block_pos.0.to_f64());
    let eps = 1.0e-4;

    if local.x <= eps {
        BlockDirection::West
    } else if local.x >= 1.0 - eps {
        BlockDirection::East
    } else if local.y <= eps {
        BlockDirection::Down
    } else if local.y >= 1.0 - eps {
        BlockDirection::Up
    } else if local.z <= eps {
        BlockDirection::North
    } else {
        BlockDirection::South
    }
}

pub enum ProjectileHit {
    Block {
        pos: BlockPos,
        face: BlockDirection,
        hit_pos: Vector3<f64>,
        normal: Vector3<f64>,
    },
    Entity {
        entity: Arc<dyn EntityBase>,
        hit_pos: Vector3<f64>,
        normal: Vector3<f64>,
    },
}

impl ProjectileHit {
    /// Returns the exact impact coordinates regardless of what was hit.
    #[must_use]
    pub const fn hit_pos(&self) -> Vector3<f64> {
        match self {
            Self::Block { hit_pos, .. } | Self::Entity { hit_pos, .. } => *hit_pos,
        }
    }

    /// Returns the surface normal of the impact.
    #[must_use]
    pub const fn normal(&self) -> Vector3<f64> {
        match self {
            Self::Block { normal, .. } | Self::Entity { normal, .. } => *normal,
        }
    }

    /// Safely returns the face hit if it was a block, otherwise None.
    #[must_use]
    pub const fn face(&self) -> Option<BlockDirection> {
        match self {
            Self::Block { face, .. } => Some(*face),
            Self::Entity { .. } => None,
        }
    }
}
