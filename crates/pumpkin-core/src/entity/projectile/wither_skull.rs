use crate::entity::projectile::owner::ProjectileOwner;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use pumpkin_data::{
    Block, damage::DamageType, effect::StatusEffect, fluid::FluidState, potion::Effect,
    tracked_data,
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::{Difficulty, math::position::BlockPos, math::vector3::Vector3};

use crate::{
    entity::{
        Entity, EntityBase,
        boss::wither::WitherEntity,
        projectile::{ProjectileHit, ThrownItemEntity},
    },
    server::Server,
    world::{
        ExplosionInteraction, World,
        explosion::{Explosion, ExplosionDamageCalculator},
    },
};

const INITIAL_ACCELERATION_POWER: f64 = 0.1;
const AIR_INERTIA: f64 = 0.95;
const DANGEROUS_INERTIA: f64 = 0.73;
const WATER_INERTIA: f64 = 0.8;
const SKULL_DAMAGE: f32 = 8.0;
const NO_OWNER_DAMAGE: f32 = 5.0;
const OWNER_HEAL_ON_KILL: f32 = 5.0;
const EXPLOSION_POWER: f32 = 1.0;
const DANGEROUS_BLOCK_RESISTANCE: f32 = 0.8;

// wither skull: WitherSkull.getBlockExplosionResistance
struct WitherSkullExplosion {
    dangerous: bool,
}

impl ExplosionDamageCalculator for WitherSkullExplosion {
    fn get_block_explosion_resistance(
        &self,
        _explosion: &Explosion,
        _world: &World,
        _pos: &BlockPos,
        block: &Block,
        fluid: &FluidState,
    ) -> Option<f32> {
        if block.default_state.is_air() && fluid.is_empty {
            return None;
        }
        let resistance = fluid.blast_resistance.max(block.blast_resistance);
        Some(if self.dangerous && WitherEntity::can_destroy(block) {
            resistance.min(DANGEROUS_BLOCK_RESISTANCE)
        } else {
            resistance
        })
    }
}

// wither skull: WitherSkull extends AbstractHurtingProjectile
pub struct WitherSkullEntity {
    pub thrown: ThrownItemEntity,
    pub dangerous: AtomicBool,
    acceleration_power: AtomicU64,
}

impl WitherSkullEntity {
    #[must_use]
    pub const fn new(entity: Entity) -> Self {
        let thrown = ThrownItemEntity {
            entity,
            owner: ProjectileOwner::none(),
            collides_with_projectiles: false,
            has_hit: AtomicBool::new(false),
            gravity: 0.0,
        };

        Self {
            thrown,
            dangerous: AtomicBool::new(false),
            acceleration_power: AtomicU64::new(INITIAL_ACCELERATION_POWER.to_bits()),
        }
    }

    // wither skull: AbstractHurtingProjectile(type, mob, direction) keeps the spawn position
    #[must_use]
    pub fn new_shot(
        entity: Entity,
        shooter: &Entity,
        dangerous: bool,
        direction: Vector3<f64>,
    ) -> Self {
        let power = INITIAL_ACCELERATION_POWER;
        entity
            .velocity
            .store(direction.normalize().multiply(power, power, power));
        entity.set_rotation(shooter.yaw.load(), shooter.pitch.load());
        let skull = Self {
            thrown: ThrownItemEntity {
                entity,
                owner: ProjectileOwner::new(Some(shooter)),
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: 0.0,
            },
            dangerous: AtomicBool::new(dangerous),
            acceleration_power: AtomicU64::new(power.to_bits()),
        };
        skull.set_dangerous(dangerous);
        skull
    }

    #[must_use]
    pub fn is_dangerous(&self) -> bool {
        self.dangerous.load(Ordering::Relaxed)
    }

    pub fn set_dangerous(&self, dangerous: bool) {
        self.dangerous.store(dangerous, Ordering::Relaxed);
        self.thrown
            .entity
            .set_synced_data(tracked_data::wither_skull::DATA_DANGEROUS, dangerous);
    }

    fn acceleration_power(&self) -> f64 {
        f64::from_bits(self.acceleration_power.load(Ordering::Relaxed))
    }

    // wither skull: getInertia
    fn inertia(&self) -> f64 {
        if self.thrown.entity.touching_water.load(Ordering::Relaxed) {
            WATER_INERTIA
        } else if self.is_dangerous() {
            DANGEROUS_INERTIA
        } else {
            AIR_INERTIA
        }
    }

    // wither skull: onHitEntity (damage, owner heal, wither effect)
    fn hit_entity(&self, world: &Arc<World>, target: &Arc<dyn EntityBase>) {
        let skull_entity = self.get_entity();
        let owner = self.thrown.owner.get(world);
        let living_owner = owner
            .as_ref()
            .filter(|owner| owner.get_living_entity().is_some());
        let was_hurt = living_owner.map_or_else(
            || {
                target.damage_with_context(
                    target.as_ref(),
                    NO_OWNER_DAMAGE,
                    DamageType::MAGIC,
                    Some(skull_entity.pos.load()),
                    Some(self),
                    None,
                )
            },
            |owner| {
                let hurt = target.damage_with_context(
                    target.as_ref(),
                    SKULL_DAMAGE,
                    DamageType::WITHER_SKULL,
                    Some(skull_entity.pos.load()),
                    Some(self),
                    Some(owner.as_ref()),
                );
                if hurt
                    && !target.get_entity().is_alive()
                    && let Some(living) = owner.get_living_entity()
                {
                    living.heal(OWNER_HEAL_ON_KILL);
                }
                hurt
            },
        );

        if was_hurt && let Some(living) = target.get_living_entity() {
            let seconds = match world.level_info.load().difficulty {
                Difficulty::Normal => 10,
                Difficulty::Hard => 40,
                Difficulty::Easy | Difficulty::Peaceful => 0,
            };
            if seconds > 0 {
                let effect = Effect {
                    effect_type: &StatusEffect::WITHER,
                    duration: 20 * seconds,
                    amplifier: 1,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: true,
                };
                if let Some(player) = target.get_player() {
                    player.add_effect(effect);
                } else {
                    living.add_effect(effect);
                }
            }
        }
    }
}

impl EntityBase for WitherSkullEntity {
    fn get_projectile_owner(&self) -> Option<&ProjectileOwner> {
        Some(&self.thrown.owner)
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("dangerous", self.is_dangerous());
        nbt.put_double("acceleration_power", self.acceleration_power());
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        self.set_dangerous(nbt.get_bool("dangerous").unwrap_or(false));
        if let Some(power) = nbt.get_double("acceleration_power") {
            self.acceleration_power
                .store(power.to_bits(), Ordering::Relaxed);
        }
    }

    fn init_data_tracker(&self) {
        let entity = self.get_entity();
        entity.set_synced_data(
            tracked_data::wither_skull::DATA_DANGEROUS,
            self.is_dangerous(),
        );
    }

    // wither skull: AbstractHurtingProjectile.tick
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
        let inertia = self.inertia();
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

    // wither skull: onHit (explosion, then discard)
    fn on_hit(&self, hit: ProjectileHit) {
        if self.thrown.has_hit.swap(true, Ordering::SeqCst) {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if let ProjectileHit::Entity { entity: target, .. } = &hit {
            self.hit_entity(&world, target);
        }

        let calculator: Arc<dyn ExplosionDamageCalculator> = Arc::new(WitherSkullExplosion {
            dangerous: self.is_dangerous(),
        });
        let position = entity.pos.load();
        if let Some(source) = world.get_entity_by_id(entity.entity_id) {
            world.explode_from_entity_with_calculator(
                source,
                position,
                EXPLOSION_POWER,
                ExplosionInteraction::Mob,
                Some(calculator),
            );
        } else {
            world.explode_with_calculator(
                position,
                EXPLOSION_POWER,
                ExplosionInteraction::Mob,
                Some(calculator),
            );
        }
        entity.remove();
    }
}
