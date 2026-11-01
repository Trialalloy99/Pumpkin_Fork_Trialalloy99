use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering::Relaxed},
};

use pumpkin_data::{
    BlockState,
    entity::EntityType,
    particle::Particle,
    sound::{Sound, SoundCategory},
    zombie_nautilus_variant::ZombieNautilusVariant,
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};

use crate::entity::{
    Entity, EntityBase,
    ai::control::{
        smooth_swimming_look_control::SmoothSwimmingLookControl,
        smooth_swimming_move_control::SmoothSwimmingMoveControl,
    },
    ai::goal::{
        escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, revenge::RevengeGoal,
        swim::SwimGoal, try_find_water::TryFindWaterGoal, wander_around::WanderAroundGoal,
    },
    ai::pathfinder::Navigator,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
};

pub const ZOMBIE_NAUTILUS_DASH_COOLDOWN: i32 = 55;

// zombie_nautilus: 26.x undead nautilus mob and drowned mount (ZombieNautilus)
pub struct ZombieNautilusEntity {
    pub mob_entity: MobEntity,
    pub variant: AtomicU8,
    pub is_dashing: AtomicBool,
    pub dash_cooldown: AtomicI32,
}

impl ZombieNautilusEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(SmoothSwimmingMoveControl::new(85, 10, 0.02, 0.1, true));
        *mob_entity
            .look_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(SmoothSwimmingLookControl::new(10));
        *mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Navigator::water_bound(true);

        let nautilus = Self {
            mob_entity,
            variant: AtomicU8::new(ZombieNautilusVariant::Temperate.id()),
            is_dashing: AtomicBool::new(false),
            dash_cooldown: AtomicI32::new(0),
        };

        let mob_arc = Arc::new(nautilus);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            goal_selector.add_goal(0, Box::new(TryFindWaterGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.6));
            goal_selector.add_goal(4, Box::new(MeleeAttackGoal::new(1.1, true)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).alerting_others()));
        };

        mob_arc
    }

    pub fn get_variant(&self) -> ZombieNautilusVariant {
        ZombieNautilusVariant::from_id(self.variant.load(Relaxed))
            .unwrap_or(ZombieNautilusVariant::Temperate)
    }

    pub fn set_variant(&self, variant: ZombieNautilusVariant) {
        self.variant.store(variant.id(), Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::zombie_nautilus::DATA_VARIANT_ID,
            VarInt(i32::from(variant.id())),
        );
    }

    pub fn is_dashing(&self) -> bool {
        self.is_dashing.load(Relaxed)
    }

    pub fn set_dashing(&self, dashing: bool) {
        self.is_dashing.store(dashing, Relaxed);
        self.get_entity()
            .set_synced_data(pumpkin_data::tracked_data::zombie_nautilus::DASH, dashing);
    }

    pub fn get_ambient_sound(&self) -> Sound {
        let is_water = self.get_entity().is_in_water();
        if is_water {
            Sound::EntityZombieNautilusAmbient
        } else {
            Sound::EntityZombieNautilusAmbientLand
        }
    }

    pub fn get_dash_sound(&self) -> Sound {
        let is_water = self.get_entity().is_in_water();
        if is_water {
            Sound::EntityZombieNautilusDash
        } else {
            Sound::EntityZombieNautilusDashLand
        }
    }

    pub fn get_dash_ready_sound(&self) -> Sound {
        let is_water = self.get_entity().is_in_water();
        if is_water {
            Sound::EntityZombieNautilusDashReady
        } else {
            Sound::EntityZombieNautilusDashReadyLand
        }
    }

    pub fn execute_dash(&self) {
        if self.dash_cooldown.load(Relaxed) > 0 {
            return;
        }
        let entity = self.get_entity();
        let yaw_rad = f64::from(entity.yaw.load()).to_radians();
        let fwd_x = -yaw_rad.sin() * 1.2;
        let fwd_z = yaw_rad.cos() * 1.2;
        let mut vel = entity.velocity.load();
        vel.x += fwd_x;
        vel.y = 0.4;
        vel.z += fwd_z;
        entity.velocity.store(vel);
        entity.velocity_dirty.store(true, Relaxed);

        self.set_dashing(true);
        self.dash_cooldown
            .store(ZOMBIE_NAUTILUS_DASH_COOLDOWN, Relaxed);
        let world = entity.world.load();
        world.play_sound(
            self.get_dash_sound(),
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

impl CustomSound for ZombieNautilusEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        let is_water = self.get_entity().is_in_water();
        Some(if is_water {
            Sound::EntityZombieNautilusHurt
        } else {
            Sound::EntityZombieNautilusHurtLand
        })
    }

    fn death_sound(&self) -> Option<Sound> {
        let is_water = self.get_entity().is_in_water();
        Some(if is_water {
            Sound::EntityZombieNautilusDeath
        } else {
            Sound::EntityZombieNautilusDeathLand
        })
    }
}

impl Mob for ZombieNautilusEntity {
    fn mob_get_swim_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieNautilusSwim)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {}

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(self.get_ambient_sound())
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_string("variant", self.get_variant().to_name().to_string());
        nbt.put_int("DashCooldown", self.dash_cooldown.load(Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(variant_name) = nbt.get_string("variant")
            && let Some(variant) = ZombieNautilusVariant::from_name(variant_name)
        {
            self.set_variant(variant);
        }
        if let Some(dash) = nbt.get_int("DashCooldown") {
            self.dash_cooldown.store(dash, Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        self.set_variant(self.get_variant());
        self.set_dashing(self.is_dashing());
    }

    // zombie_nautilus: variant selection from biome and jockey setup (ZombieNautilus.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<crate::world::World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let entity = self.get_entity();
        let pos = entity.block_pos.load();
        let biome = world.get_biome(&pos);
        let variant = ZombieNautilusVariant::select_for_biome(biome.registry_id);
        self.set_variant(variant);
        self.mob_entity.finalize_spawn_base();
        group_data
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        if !entity.is_alive() || self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }

        if self.is_dashing() && self.dash_cooldown.load(Relaxed) < 35 {
            self.set_dashing(false);
        }

        let cooldown = self.dash_cooldown.load(Relaxed);
        if cooldown > 0 {
            let next = cooldown - 1;
            self.dash_cooldown.store(next, Relaxed);
            if next == 0 {
                let world = entity.world.load();
                world.play_sound(
                    self.get_dash_ready_sound(),
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
            }
        }

        if entity.is_in_water() {
            let velo = entity.velocity.load();
            let speed = velo.length();
            let prob = (speed * 2.0).clamp(0.15, 1.0);
            if rand::random::<f64>() < prob {
                let world = entity.world.load();
                let pos = entity.pos.load();
                world.spawn_particle(
                    pos + Vector3::new(0.0, 0.25, 0.0),
                    Vector3::new(0.4, 0.4, 0.4),
                    0.5,
                    2,
                    Particle::Bubble,
                );
            }
        }
    }

    fn can_be_leashed(&self) -> bool {
        self.get_controlling_passenger().is_none()
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        self.get_controlling_passenger().is_none() && self.get_entity().custom_name.load().is_none()
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        let entity = self.get_entity();
        let passengers = entity.passengers.lock().ok()?;
        passengers.first().cloned()
    }

    fn on_player_start_jump(&self, _jump_scale: i32) {
        self.execute_dash();
    }
}
