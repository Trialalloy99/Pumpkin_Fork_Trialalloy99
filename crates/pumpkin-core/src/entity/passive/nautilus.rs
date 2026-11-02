use crossbeam::atomic::AtomicCell;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering::Relaxed},
};
use uuid::Uuid;

use pumpkin_data::{
    BlockState,
    damage::DamageType,
    effect::StatusEffect,
    entity::{EntityStatus, EntityType},
    item::Item,
    item_stack::ItemStack,
    particle::Particle,
    potion::Effect,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::ActorEventID;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::control::{
        smooth_swimming_look_control::SmoothSwimmingLookControl,
        smooth_swimming_move_control::SmoothSwimmingMoveControl,
    },
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal,
        random_swimming::RandomSwimmingGoal, revenge::RevengeGoal, swim::SwimGoal,
        tempt::TemptGoal, try_find_water::TryFindWaterGoal,
    },
    ai::pathfinder::Navigator,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};

pub const NAUTILUS_FOOD: &[&Item] = &[
    &Item::NAUTILUS_SHELL,
    &Item::PUFFERFISH,
    &Item::COD,
    &Item::SALMON,
    &Item::TROPICAL_FISH,
];

pub const NAUTILUS_MAX_AIR: i32 = 300;
pub const NAUTILUS_DASH_COOLDOWN: i32 = 55;

// nautilus: base nautilus mob, taming, saddle, breathing and dash (AbstractNautilus, Nautilus)
pub struct NautilusEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub is_tame: AtomicBool,
    pub owner: AtomicCell<Option<Uuid>>,
    pub is_dashing: AtomicBool,
    pub dash_cooldown: AtomicI32,
    pub is_saddled: AtomicBool,
    pub inventory: Mutex<Vec<ItemStack>>,
}

impl NautilusEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(SmoothSwimmingMoveControl::new(85, 10, 0.011, 0.0, true));
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
            ageable_data: AgeableData::default(),
            is_tame: AtomicBool::new(false),
            owner: AtomicCell::new(None),
            is_dashing: AtomicBool::new(false),
            dash_cooldown: AtomicI32::new(0),
            is_saddled: AtomicBool::new(false),
            inventory: Mutex::new(vec![ItemStack::new(0, &Item::AIR); 9]),
        };

        let mob_arc = Arc::new(nautilus);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            // nautilus: AI goals (NautilusAi)
            goal_selector.add_goal(0, Box::new(TryFindWaterGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.6));
            goal_selector.add_goal(2, BreedGoal::new(0.4));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.3, NAUTILUS_FOOD, false)));
            goal_selector.add_goal(4, Box::new(MeleeAttackGoal::new(0.6, true)));
            goal_selector.add_goal(5, Box::new(RandomSwimmingGoal::new(1.0, 1)));
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

    pub fn is_dashing(&self) -> bool {
        self.is_dashing.load(Relaxed)
    }

    pub fn set_dashing(&self, dashing: bool) {
        self.is_dashing.store(dashing, Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::nautilus::DASH, dashing);
    }

    pub fn is_tame(&self) -> bool {
        self.is_tame.load(Relaxed)
    }

    pub fn set_tame(&self, tame: bool, owner: Option<Uuid>) {
        self.is_tame.store(tame, Relaxed);
        self.owner.store(owner);
    }

    pub fn get_ambient_sound(&self) -> Sound {
        let is_baby = self.mob_entity.living_entity.entity.age.load(Relaxed) < 0;
        let is_water = self.mob_entity.living_entity.entity.is_in_water();
        if is_baby {
            if is_water {
                Sound::EntityBabyNautilusAmbient
            } else {
                Sound::EntityBabyNautilusAmbientLand
            }
        } else if is_water {
            Sound::EntityNautilusAmbient
        } else {
            Sound::EntityNautilusAmbientLand
        }
    }

    pub fn get_dash_sound(&self) -> Sound {
        let is_water = self.mob_entity.living_entity.entity.is_in_water();
        if is_water {
            Sound::EntityNautilusDash
        } else {
            Sound::EntityNautilusDashLand
        }
    }

    pub fn get_dash_ready_sound(&self) -> Sound {
        let is_water = self.mob_entity.living_entity.entity.is_in_water();
        if is_water {
            Sound::EntityNautilusDashReady
        } else {
            Sound::EntityNautilusDashReadyLand
        }
    }

    pub fn get_eat_sound(&self) -> Sound {
        let is_baby = self.mob_entity.living_entity.entity.age.load(Relaxed) < 0;
        if is_baby {
            Sound::EntityBabyNautilusEat
        } else {
            Sound::EntityNautilusEat
        }
    }

    // nautilus: dash jump execution (AbstractNautilus.executeRidersJump)
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
        self.dash_cooldown.store(NAUTILUS_DASH_COOLDOWN, Relaxed);
        let world = entity.world.load();
        world.play_sound(
            self.get_dash_sound(),
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

impl AgeableMob for NautilusEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for NautilusEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        NAUTILUS_FOOD.contains(&item_stack.item)
    }
}

impl CustomSound for NautilusEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Relaxed) < 0;
        let is_water = entity.is_in_water();
        Some(if is_baby {
            if is_water {
                Sound::EntityBabyNautilusHurt
            } else {
                Sound::EntityBabyNautilusHurtLand
            }
        } else if is_water {
            Sound::EntityNautilusHurt
        } else {
            Sound::EntityNautilusHurtLand
        })
    }

    fn death_sound(&self) -> Option<Sound> {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Relaxed) < 0;
        let is_water = entity.is_in_water();
        Some(if is_baby {
            if is_water {
                Sound::EntityBabyNautilusDeath
            } else {
                Sound::EntityBabyNautilusDeathLand
            }
        } else if is_water {
            Sound::EntityNautilusDeath
        } else {
            Sound::EntityNautilusDeathLand
        })
    }
}

impl Mob for NautilusEntity {
    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn get_owner_uuid(&self) -> Option<Uuid> {
        self.owner.load()
    }

    fn mob_get_swim_sound(&self) -> Option<Sound> {
        let is_baby = self.mob_entity.living_entity.entity.age.load(Relaxed) < 0;
        Some(if is_baby {
            Sound::EntityBabyNautilusSwim
        } else {
            Sound::EntityNautilusSwim
        })
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {}

    // nautilus: no gravity, 0.9 drag in water (AbstractNautilus.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(
            living.movement_input.load(),
            f64::from(living.get_speed()),
        );
        living.make_move(caller);
        entity.velocity.store(entity.velocity.load() * 0.9);
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(self.get_ambient_sound())
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("IsTame", self.is_tame.load(Relaxed));
        nbt.put_bool("Saddled", self.is_saddled.load(Relaxed));
        nbt.put_int("DashCooldown", self.dash_cooldown.load(Relaxed));
        if let Some(owner) = self.owner.load() {
            nbt.put_uuid("Owner", owner);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(is_tame) = nbt.get_bool("IsTame") {
            self.is_tame.store(is_tame, Relaxed);
        }
        if let Some(saddled) = nbt.get_bool("Saddled") {
            self.is_saddled.store(saddled, Relaxed);
        }
        if let Some(dash) = nbt.get_int("DashCooldown") {
            self.dash_cooldown.store(dash, Relaxed);
        }
        if let Some(owner) = nbt.get_uuid("Owner") {
            self.owner.store(Some(owner));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        self.mob_entity.living_entity.entity.set_synced_data(
            pumpkin_data::tracked_data::nautilus::DASH,
            self.is_dashing(),
        );
    }

    // nautilus: air supply, breath effect, and dash cooldown tick (AbstractNautilus, Nautilus)
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        if !entity.is_alive() || self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }

        // Air supply handling (Nautilus.handleAirSupply)
        if entity.is_in_water() {
            entity.set_air_supply(NAUTILUS_MAX_AIR);
        } else {
            let air = entity.air_supply.load(Relaxed) - 1;
            entity.set_air_supply(air);
            if air <= -20 {
                entity.set_air_supply(0);
                self.mob_entity
                    .living_entity
                    .damage(self, 2.0, DamageType::DRY_OUT);
            }
        }

        // Breath of the Nautilus effect to passenger player
        let Ok(passengers) = entity.passengers.try_lock() else {
            return;
        };
        if let Some(passenger) = passengers.first()
            && let Some(player) = passenger.cast_any().downcast_ref::<Player>()
        {
            let world = entity.world.load();
            let game_time = world.get_world_age();
            if game_time % 40 == 0 {
                let player_arc = world.get_player_by_uuid(player.gameprofile.id);
                if let Some(p) = player_arc {
                    p.add_effect(Effect {
                        effect_type: &StatusEffect::BREATH_OF_THE_NAUTILUS,
                        duration: 60,
                        amplifier: 0,
                        ambient: true,
                        show_particles: true,
                        show_icon: true,
                        blend: true,
                    });
                }
            }
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

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let mob_entity = &self.mob_entity;
        let entity = &mob_entity.living_entity.entity;

        if !self.is_tame() && self.is_food(item_stack) {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            if rand::random::<u32>().is_multiple_of(3) && self.approve_taming(player) {
                self.set_tame(true, Some(player.gameprofile.id));
                mob_entity
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .stop();
                let world = entity.world.load();
                world.send_entity_status(
                    entity,
                    EntityStatus::TamingSucceeded,
                    ActorEventID::TamingSucceeded,
                );
            } else {
                let world = entity.world.load();
                world.send_entity_status(
                    entity,
                    EntityStatus::TamingFailed,
                    ActorEventID::TamingFailed,
                );
            }
            let world = entity.world.load();
            world.play_sound(
                self.get_eat_sound(),
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            return true;
        }

        if self.is_tame() && !player.get_entity().is_sneaking() {
            if !self.is_saddled.load(Relaxed) && item_stack.item == &Item::SADDLE {
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                self.is_saddled.store(true, Relaxed);
                let world = entity.world.load();
                world.play_sound(
                    Sound::ItemNautilusSaddleEquip,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
                return true;
            }

            let world = player.world();
            if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
                return true;
            }
        }

        self.animal_interact(player, item_stack, self.get_ambient_sound())
    }

    fn is_saddled(&self) -> bool {
        self.is_saddled.load(Relaxed)
    }

    fn can_be_saddled(&self) -> bool {
        self.mob_entity.living_entity.entity.is_alive()
    }

    fn set_saddled(&self, saddled: bool) {
        self.is_saddled.store(saddled, Relaxed);
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
