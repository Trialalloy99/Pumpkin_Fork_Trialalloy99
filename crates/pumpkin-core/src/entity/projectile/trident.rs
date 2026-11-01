use crate::entity::projectile::owner::ProjectileOwner;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    entity::{Entity, EntityBase, living::LivingEntity, player::Player},
    server::Server,
};
use pumpkin_data::damage::DamageType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_protocol::IdOr;
use pumpkin_protocol::java::client::play::{CEntityVelocity, CSoundEffect};
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::arrow::ArrowPickup;
use super::{ProjectileHit, calculate_ray_intersection};

pub struct TridentEntity {
    pub entity: Entity,
    pub owner: ProjectileOwner,
    pub item_stack: Arc<Mutex<ItemStack>>,
    pub pickup: ArrowPickup,
    pub in_ground: AtomicBool,
    pub in_ground_time: AtomicU32,
    pub life: AtomicU32,
    pub shake_time: AtomicU8,
    pub has_hit: AtomicBool,
    pub last_block_pos: Arc<std::sync::RwLock<Option<BlockPos>>>,
    // trident: hit something, loyalty may start (ThrownTrident.dealtDamage)
    dealt_damage: AtomicBool,
    return_ticks: AtomicU32,
}

impl TridentEntity {
    const BASE_DAMAGE: f64 = 8.0;
    const AIR_INERTIA: f64 = 0.99;
    const WATER_INERTIA: f64 = 0.99;
    const GRAVITY: f64 = 0.05;
    const DESPAWN_TIME: u32 = 1200;

    pub fn new(entity: Entity, owner: Option<&Entity>) -> Self {
        Self {
            entity,
            owner: ProjectileOwner::new(owner),
            item_stack: Arc::new(Mutex::new(ItemStack::new(1, &Item::TRIDENT))),
            pickup: ArrowPickup::Disallowed,
            in_ground: AtomicBool::new(false),
            in_ground_time: AtomicU32::new(0),
            life: AtomicU32::new(0),
            shake_time: AtomicU8::new(0),
            has_hit: AtomicBool::new(false),
            last_block_pos: Arc::new(std::sync::RwLock::new(None)),
            dealt_damage: AtomicBool::new(false),
            return_ticks: AtomicU32::new(0),
        }
    }

    pub fn new_shot(
        entity: Entity,
        shooter: &Entity,
        item_stack: ItemStack,
        pickup: ArrowPickup,
    ) -> Self {
        let mut owner_pos = shooter.pos.load();
        owner_pos.y = owner_pos.y + f64::from(shooter.entity_dimension.load().eye_height) - 0.1;
        entity.pos.store(owner_pos);
        entity.set_velocity(Vector3::new(0.0, 0.1, 0.0));

        Self {
            entity,
            owner: ProjectileOwner::new(Some(shooter)),
            item_stack: Arc::new(Mutex::new(item_stack)),
            pickup,
            in_ground: AtomicBool::new(false),
            in_ground_time: AtomicU32::new(0),
            life: AtomicU32::new(0),
            shake_time: AtomicU8::new(0),
            has_hit: AtomicBool::new(false),
            last_block_pos: Arc::new(std::sync::RwLock::new(None)),
            dealt_damage: AtomicBool::new(false),
            return_ticks: AtomicU32::new(0),
        }
    }

    fn loyalty(&self) -> i32 {
        self.item_stack
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_enchantment_level(&pumpkin_data::Enchantment::LOYALTY)
    }

    // trident: fly back to the owner, drop if the owner is gone (ThrownTrident.tick loyalty)
    fn tick_loyalty(&self, world: &crate::world::World) -> bool {
        let entity = &self.entity;
        if self.in_ground.load(Ordering::Relaxed) && self.in_ground_time.load(Ordering::Relaxed) > 4
        {
            self.dealt_damage.store(true, Ordering::Relaxed);
        }
        let loyalty = self.loyalty();
        if loyalty <= 0
            || !(self.dealt_damage.load(Ordering::Relaxed)
                || entity.no_physics.load(Ordering::Relaxed))
        {
            return false;
        }
        let Some(owner) = self.owner.get(world) else {
            return false;
        };
        let owner_entity = owner.get_entity();
        if !owner_entity.is_alive()
            || owner
                .get_player()
                .is_some_and(crate::entity::player::Player::is_spectator)
        {
            if self.pickup == ArrowPickup::Allowed {
                let stack = self
                    .item_stack
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                entity.spawn_at_location(stack);
            }
            entity.remove();
            return true;
        }
        let pos = entity.pos.load();
        let owner_eye = owner_entity.get_eye_pos();
        if owner.get_player().is_none()
            && pos.squared_distance_to_vec(&owner_eye).sqrt()
                < f64::from(owner_entity.width()) + 1.0
        {
            entity.remove();
            return true;
        }
        entity.no_physics.store(true, Ordering::Relaxed);
        self.in_ground.store(false, Ordering::Relaxed);
        let to_owner = owner_eye - pos;
        let loyalty = f64::from(loyalty);
        let pos = Vector3::new(pos.x, pos.y + to_owner.y * 0.015 * loyalty, pos.z);
        let accel = 0.05 * loyalty;
        let velocity = entity
            .velocity
            .load()
            .multiply(0.95, 0.95, 0.95)
            .add(&to_owner.normalize().multiply(accel, accel, accel));
        if self.return_ticks.fetch_add(1, Ordering::Relaxed) == 0 {
            world.play_sound_fine(
                Sound::ItemTridentReturn,
                SoundCategory::Neutral,
                &pos,
                10.0,
                1.0,
            );
        }
        // trident: no-physics flight, no collisions or gravity (AbstractArrow.tick isNoPhysics)
        entity.set_pos(pos.add(&velocity));
        let len = velocity.horizontal_length();
        entity.set_rotation(
            (-velocity.x).atan2(-velocity.z) as f32 * 57.295_776,
            velocity.y.atan2(len) as f32 * 57.295_776,
        );
        let inertia = if entity.touching_water.load(Ordering::Relaxed) {
            Self::WATER_INERTIA
        } else {
            Self::AIR_INERTIA
        };
        entity
            .velocity
            .store(velocity.multiply(inertia, inertia, inertia));
        let packet = CEntityVelocity::new(entity.entity_id.into(), velocity);
        world.broadcast_to_chunk(entity.chunk_pos.load(), &packet);
        true
    }

    /// Applies projectile-spawned enchantment effects matching vanilla `Projectile::applyOnProjectileSpawned`.
    pub fn apply_on_projectile_spawned(&self, pickup_item_stack: &ItemStack) {
        super::apply_on_projectile_spawned(self.get_entity(), pickup_item_stack, None, None);
    }

    pub fn set_velocity_from_rotation(
        &self,
        pitch: f32,
        yaw: f32,
        roll: f32,
        speed: f32,
        divergence: f32,
    ) {
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
        let len = velocity.horizontal_length();
        self.entity.set_rotation(
            velocity.x.atan2(velocity.z) as f32 * 57.295_776,
            velocity.y.atan2(len) as f32 * 57.295_776,
        );
    }

    fn should_skip_collision(&self, self_ent: &Entity, other: &Arc<dyn EntityBase>) -> bool {
        let other_ent = other.get_entity();
        // projectile: spectators are not pickable (Projectile.canHitEntity)
        if other.is_spectator() {
            return true;
        }

        // Don't collide with self
        if other_ent.entity_id == self_ent.entity_id || self.dealt_damage.load(Ordering::Relaxed) {
            return true;
        }

        if !self.owner.can_hit(&self_ent.world.load(), other_ent) {
            return true;
        }

        // Skip other projectiles and item entities
        if other_ent.entity_type == &pumpkin_data::entity::EntityType::ARROW
            || other_ent.entity_type == &pumpkin_data::entity::EntityType::TRIDENT
            || other_ent.entity_type == &pumpkin_data::entity::EntityType::ITEM
            || other_ent.entity_type == &pumpkin_data::entity::EntityType::FALLING_BLOCK
        {
            return true;
        }

        false
    }
}

impl EntityBase for TridentEntity {
    fn get_projectile_owner(&self) -> Option<&ProjectileOwner> {
        Some(&self.owner)
    }

    #[expect(clippy::too_many_lines, reason = "mirrors AbstractArrow.tick")]
    fn tick(&self, caller: &dyn EntityBase, _server: &Server) {
        let entity = self.get_entity();
        let world = entity.world.load();
        self.owner.check_left_owner(entity);

        // Handle shake time
        let shake = self.shake_time.load(Ordering::Relaxed);
        if shake > 0 {
            self.shake_time.store(shake - 1, Ordering::Relaxed);
        }

        if self.tick_loyalty(&world) {
            entity.update_fluid_state(caller);
            return;
        }

        if self.in_ground.load(Ordering::Relaxed) {
            let _in_ground_time = self.in_ground_time.fetch_add(1, Ordering::Relaxed);
            // trident: loyal tridents never despawn (ThrownTrident.tickDespawn)
            if self.pickup != ArrowPickup::Allowed || self.loyalty() <= 0 {
                let life = self.life.fetch_add(1, Ordering::Relaxed);
                if life >= Self::DESPAWN_TIME {
                    entity.remove();
                }
            }
            return;
        }

        // Trident is flying
        let start_pos = entity.pos.load();
        let velocity = entity.velocity.load();

        // trident: move with this tick's velocity, drag and gravity for the next (AbstractArrow.tick)
        let in_water = entity.touching_water.load(Ordering::Relaxed);

        // Update rotation based on velocity
        let len = velocity.horizontal_length();
        entity.set_rotation(
            velocity.x.atan2(velocity.z) as f32 * 57.295_776,
            velocity.y.atan2(len) as f32 * 57.295_776,
        );

        // Move trident
        let new_pos = start_pos.add(&velocity);
        entity.set_pos(new_pos);

        // Broadcast velocity update
        let packet = CEntityVelocity::new(entity.entity_id.into(), velocity);
        let chunk_pos = entity.chunk_pos.load();
        world.broadcast_to_chunk(chunk_pos, &packet);

        // Check for collisions using raycasting
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
        let (block_cols, block_positions) =
            world.get_block_collisions(search_box, self.get_entity());
        for (idx, bb) in block_cols.iter().enumerate() {
            if let Some(t) = calculate_ray_intersection(&start_pos, &velocity, bb)
                && t < closest_t
            {
                closest_t = t;

                // Map back to block pos
                for (len, pos) in &block_positions {
                    if idx < *len {
                        let hit_pos = start_pos.add(&velocity.multiply(t, t, t));
                        hit = Some(ProjectileHit::Block {
                            pos: *pos,
                            face: get_hit_face(hit_pos, *pos),
                            hit_pos,
                            normal: velocity.normalize().multiply(-1.0, -1.0, -1.0),
                        });
                        break;
                    }
                }
            }
        }

        // Entity collisions
        let candidates = world.get_all_at_box(&search_box);
        for cand in candidates {
            if self.should_skip_collision(entity, &cand) {
                continue;
            }

            let ebb = cand.get_entity().bounding_box.load().expand(0.3, 0.3, 0.3);
            if let Some(t) = calculate_ray_intersection(&start_pos, &velocity, &ebb)
                && t < closest_t
            {
                closest_t = t;
                let hit_pos = start_pos.add(&velocity.multiply(t, t, t));
                hit = Some(ProjectileHit::Entity {
                    entity: cand.clone(),
                    hit_pos,
                    normal: velocity.normalize().multiply(-1.0, -1.0, -1.0),
                });
            }
        }

        // Handle hit
        if let Some(h) = hit
            && !self.has_hit.swap(true, Ordering::SeqCst)
        {
            caller.on_hit(h);
        }

        if !self.in_ground.load(Ordering::Relaxed) && !entity.is_removed() {
            let mut velocity = entity.velocity.load();
            let inertia = if in_water {
                Self::WATER_INERTIA
            } else {
                Self::AIR_INERTIA
            };
            velocity = velocity.multiply(inertia, inertia, inertia);
            velocity.y -= Self::GRAVITY;
            entity.velocity.store(velocity);
        }
        entity.update_fluid_state(caller);
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }
    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn on_hit(&self, hit: ProjectileHit) {
        let entity = self.get_entity();
        let world = entity.world.load();

        match hit {
            ProjectileHit::Block { pos, hit_pos, .. } => {
                self.in_ground.store(true, Ordering::Relaxed);
                self.shake_time.store(7, Ordering::Relaxed);
                *self
                    .last_block_pos
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pos);

                let block = world.get_block(&pos);
                let state = world.get_block_state(&pos);
                if let Some(server) = world.server.upgrade() {
                    world
                        .block_registry
                        .on_projectile_hit(block, &world, self, &pos, state, &hit_pos, &server);
                }

                // Stop the trident
                entity.velocity.store(Vector3::new(0.0, 0.0, 0.0));
                entity.set_pos(hit_pos);

                // Play sound
                let sound_packet = CSoundEffect::new(
                    IdOr::Id(Sound::ItemTridentHitGround as u16),
                    SoundCategory::Neutral,
                    &hit_pos,
                    1.0,
                    1.0,
                    0,
                );
                let chunk_pos = entity.chunk_pos.load();
                world.broadcast_to_chunk(chunk_pos, &sound_packet);
            }
            ProjectileHit::Entity {
                entity: target,
                hit_pos,
                ..
            } => {
                // trident: 8 damage, impaling vs sensitive mobs, reverse and slow down (ThrownTrident.onHitEntity)
                let mut damage = Self::BASE_DAMAGE;
                let impaling = self
                    .item_stack
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_enchantment_level(&pumpkin_data::Enchantment::IMPALING);
                if impaling > 0
                    && target
                        .get_entity()
                        .entity_type
                        .has_tag(&pumpkin_data::tag::EntityType::MINECRAFT_SENSITIVE_TO_IMPALING)
                {
                    damage += 2.5 * f64::from(impaling);
                }

                self.dealt_damage.store(true, Ordering::Relaxed);
                let owner = self.owner.get(&world);
                target.damage_with_context(
                    target.as_ref(),
                    damage as f32,
                    DamageType::TRIDENT,
                    Some(hit_pos),
                    Some(self),
                    Some(owner.as_deref().unwrap_or(self)),
                );

                let velocity = entity.velocity.load();
                entity
                    .velocity
                    .store(velocity.multiply(-0.5 * 0.02, -0.5 * 0.2, -0.5 * 0.02));
                let yaw = entity.yaw.load();
                entity.set_rotation(yaw + 180.0, entity.pitch.load());

                // Play hit sound
                let sound_packet = CSoundEffect::new(
                    IdOr::Id(Sound::ItemTridentHit as u16),
                    SoundCategory::Neutral,
                    &hit_pos,
                    1.0,
                    1.0,
                    0,
                );
                let chunk_pos = entity.chunk_pos.load();
                world.broadcast_to_chunk(chunk_pos, &sound_packet);
                self.has_hit.store(false, Ordering::Relaxed);
            }
        }
    }

    fn on_player_collision(&self, player: &Arc<Player>) {
        // trident: pick up when grounded or returning to its owner (ThrownTrident.tryPickup)
        let returning = self.entity.no_physics.load(Ordering::Relaxed);
        if !(self.in_ground.load(Ordering::Relaxed) || returning)
            || self.shake_time.load(Ordering::Relaxed) > 0
        {
            return;
        }
        if returning && !self.owner.matches(player.get_entity()) {
            return;
        }

        if player.living_entity.health.load() <= 0.0 {
            return;
        }

        match self.pickup {
            ArrowPickup::Disallowed => return,
            ArrowPickup::CreativeOnly if !player.is_creative() => return,
            _ => {}
        }

        let mut stack = self
            .item_stack
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if player.is_creative() || player.inventory.insert_stack_anywhere(&mut stack) {
            player.increment_stat(
                pumpkin_data::statistic::StatisticCategory::PickedUp,
                stack.item.id as i32,
                1,
            );
            player.living_entity.pickup(&self.entity, 1);
            self.get_entity().remove();
        }
    }
}

/// Get the face of the block that was hit
fn get_hit_face(hit_pos: Vector3<f64>, block_pos: BlockPos) -> pumpkin_data::BlockDirection {
    let local = hit_pos.sub(&block_pos.0.to_f64());
    let eps = 1.0e-4;

    if local.x <= eps {
        pumpkin_data::BlockDirection::West
    } else if local.x >= 1.0 - eps {
        pumpkin_data::BlockDirection::East
    } else if local.y <= eps {
        pumpkin_data::BlockDirection::Down
    } else if local.y >= 1.0 - eps {
        pumpkin_data::BlockDirection::Up
    } else if local.z <= eps {
        pumpkin_data::BlockDirection::North
    } else {
        pumpkin_data::BlockDirection::South
    }
}
