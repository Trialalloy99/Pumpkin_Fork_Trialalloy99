use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering::Relaxed},
};

use pumpkin_data::structures::StructureSet;
use pumpkin_data::{
    damage::DamageType,
    effect::StatusEffect,
    entity::EntityType,
    item::Item,
    item_stack::ItemStack,
    particle::Particle,
    potion::Effect,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::generation::generator::structure_finder::{
    StructureSearch, find_nearest_structure,
};

use crate::entity::ai::goal::to_goal_ticks;
use crate::entity::ai::util::default_random_pos;
use rand::RngExt;

use crate::entity::ai::behavior::fleeing::{self, Flee};
use crate::entity::{
    Entity, EntityBase,
    ai::control::{
        smooth_swimming_look_control::SmoothSwimmingLookControl,
        smooth_swimming_move_control::SmoothSwimmingMoveControl,
    },
    ai::goal::{
        Controls, Goal, escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, revenge::RevengeGoal,
        swim::SwimGoal, try_find_water::TryFindWaterGoal, wander_around::WanderAroundGoal,
    },
    ai::pathfinder::Navigator,
    ai::target_match::TargetMatch,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity},
    player::Player,
};

const TEMPT_ITEMS: &[&Item] = &[&Item::COD, &Item::SALMON];
const DOLPHIN_FLEES: &[Flee] = &[Flee::new(9, TargetMatch::GUARDIAN_LIKE, 8.0, 1.0, 1.0)];

pub const MAX_AIR_SUPPLY: i32 = 4800;
pub const MAX_MOISTNESS: i32 = 2400;

// dolphin: moistness, air breathing, jumping, and grace parity (Dolphin)
pub struct DolphinEntity {
    pub mob_entity: MobEntity,
    pub got_fish: AtomicBool,
    pub moistness_level: AtomicI32,
    // dolphin: Dolphin.treasurePos
    pub treasure_pos: crossbeam::atomic::AtomicCell<Option<BlockPos>>,
}

impl DolphinEntity {
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

        let dolphin = Self {
            mob_entity,
            got_fish: AtomicBool::new(false),
            moistness_level: AtomicI32::new(MAX_MOISTNESS),
            treasure_pos: crossbeam::atomic::AtomicCell::new(None),
        };
        let mob_arc = Arc::new(dolphin);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // dolphin: breathe air goal (Dolphin.registerGoals, BreathAirGoal)
            goal_selector.add_goal(0, Box::new(TryFindWaterGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            // dolphin: swim to treasure goal (Dolphin.DolphinSwimToTreasureGoal)
            goal_selector.add_goal(1, Box::new(DolphinSwimToTreasureGoal::default()));
            // dolphin: swim with player granting Dolphin's Grace (Dolphin.DolphinSwimWithPlayerGoal)
            goal_selector.add_goal(2, Box::new(DolphinSwimWithPlayerGoal::new(4.0)));
            goal_selector.add_goal(3, EscapeDangerGoal::new(1.6));
            goal_selector.add_goal(4, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(4, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(
                5,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            // dolphin: jump out of water goal (Dolphin.DolphinJumpGoal)
            goal_selector.add_goal(5, Box::new(DolphinJumpGoal::new(10)));
            goal_selector.add_goal(6, Box::new(MeleeAttackGoal::new(1.2, true)));
            fleeing::apply(goal_selector, DOLPHIN_FLEES);
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .ignoring(|entity_type| TargetMatch::GUARDIAN_LIKE.matches(entity_type))
                        .alerting_others(),
                ),
            );
        };

        mob_arc
    }

    #[must_use]
    pub fn got_fish(&self) -> bool {
        self.got_fish.load(Relaxed)
    }

    pub fn set_got_fish(&self, val: bool) {
        self.got_fish.store(val, Relaxed);
        self.get_entity()
            .set_synced_data(pumpkin_data::tracked_data::dolphin::GOT_FISH, val);
    }

    pub fn get_moistness(&self) -> i32 {
        self.moistness_level.load(Relaxed)
    }

    pub fn set_moistness(&self, moistness: i32) {
        self.moistness_level.store(moistness, Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::dolphin::MOISTNESS_LEVEL,
            moistness,
        );
    }
}

impl CustomSound for DolphinEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityDolphinHurt)
    }

    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityDolphinDeath)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Neutral
    }
}

impl Mob for DolphinEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        if self.get_entity().is_in_water() {
            Some(Sound::EntityDolphinAmbientWater)
        } else {
            Some(Sound::EntityDolphinAmbient)
        }
    }

    fn mob_init_data_tracker(&self) {
        self.set_got_fish(self.got_fish());
        self.set_moistness(self.get_moistness());
    }

    // dolphin: moistness and air breathing tick (Dolphin.tick)
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        if !entity.is_alive() || self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }

        if self.mob_entity.is_no_ai() {
            entity.set_air_supply(MAX_AIR_SUPPLY);
            return;
        }

        let in_water = entity.is_in_water();
        if in_water {
            self.set_moistness(MAX_MOISTNESS);
            // In water: air depletes
            let air = entity.air_supply.load(Relaxed) - 1;
            entity.set_air_supply(air);
            if air <= -20 {
                entity.set_air_supply(0);
                self.mob_entity
                    .living_entity
                    .damage(self, 1.0, DamageType::DROWN);
            }
        } else {
            // Out of water: air replenishes, moistness depletes
            entity.set_air_supply(MAX_AIR_SUPPLY);
            let moist = self.get_moistness() - 1;
            self.set_moistness(moist);
            if moist <= 0 && moist % 20 == 0 {
                self.mob_entity
                    .living_entity
                    .damage(self, 1.0, DamageType::DRY_OUT);
            }

            // Flopping on ground (Dolphin.tick)
            if entity.on_ground.load(Relaxed) {
                let mut rng = self.get_random();
                let hop_x = f64::from((rng.random::<f32>() * 2.0 - 1.0) * 0.2);
                let hop_z = f64::from((rng.random::<f32>() * 2.0 - 1.0) * 0.2);
                let mut vel = entity.velocity.load();
                vel.x += hop_x;
                vel.y = 0.5;
                vel.z += hop_z;
                entity.velocity.store(vel);
                entity.on_ground.store(false, Relaxed);
                entity.velocity_dirty.store(true, Relaxed);
            }
        }
    }

    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(
            living.movement_input.load(),
            f64::from(living.get_speed()),
        );
        living.make_move(caller);
        let mut velocity = entity.velocity.load() * 0.9;
        if self
            .mob_entity
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            velocity.y -= 0.005;
        }
        entity.velocity.store(velocity);
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("GotFish", self.got_fish());
        nbt.put_int("Moistness", self.get_moistness());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(got_fish) = nbt.get_bool("GotFish") {
            self.set_got_fish(got_fish);
        }
        if let Some(moistness) = nbt.get_int("Moistness") {
            self.set_moistness(moistness);
        }
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();
        if TEMPT_ITEMS.iter().any(|i| i.id == item.id) {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            self.set_got_fish(true);
            self.mob_entity.living_entity.heal(2.0);

            let entity = self.get_entity();
            let world = entity.world.load();
            let pos = entity.pos.load();
            world.spawn_particle(
                pos + Vector3::new(0.0, f64::from(entity.height()), 0.0),
                Vector3::new(0.5, 0.5, 0.5),
                1.0,
                7,
                Particle::HappyVillager,
            );
            world.play_sound(Sound::EntityDolphinEat, SoundCategory::Neutral, &pos);
            return true;
        }

        false
    }
}

// dolphin: DolphinSwimWithPlayerGoal grants Dolphin's Grace (Dolphin.DolphinSwimWithPlayerGoal)
struct DolphinSwimWithPlayerGoal {
    speed_modifier: f64,
}

impl DolphinSwimWithPlayerGoal {
    const fn new(speed_modifier: f64) -> Self {
        Self { speed_modifier }
    }

    fn dolphin(mob: &dyn Mob) -> Option<&DolphinEntity> {
        mob.cast_any().downcast_ref::<DolphinEntity>()
    }
}

impl Goal for DolphinSwimWithPlayerGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(dolphin) = Self::dolphin(mob) else {
            return false;
        };
        let entity = dolphin.get_entity();
        if !entity.is_in_water() {
            return false;
        }
        let pos = entity.pos.load();
        let world = entity.world.load();
        world
            .get_players_at_box(&entity.bounding_box.load().expand(16.0, 16.0, 16.0))
            .into_iter()
            .any(|player| {
                player
                    .living_entity
                    .entity
                    .pos
                    .load()
                    .squared_distance_to_vec(&pos)
                    < 256.0
            })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(dolphin) = Self::dolphin(mob) else {
            return;
        };
        let entity = dolphin.get_entity();
        let pos = entity.pos.load();
        let world = entity.world.load();
        let players =
            world.get_players_at_box(&entity.bounding_box.load().expand(16.0, 16.0, 16.0));
        for player in players {
            let player_pos = player.living_entity.entity.pos.load();
            let dist_sq = player_pos.squared_distance_to_vec(&pos);
            if dist_sq < 256.0 {
                // Grant Dolphin's Grace (100 ticks = 5 seconds)
                player.add_effect(Effect {
                    effect_type: &StatusEffect::DOLPHINS_GRACE,
                    duration: 100,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                });
                if dist_sq > 6.25 {
                    dolphin.mob_entity.steered().navigator().move_to_entity(
                        &player.living_entity,
                        self.speed_modifier,
                        &dolphin.mob_entity.living_entity,
                    );
                }
                break;
            }
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// dolphin: DolphinJumpGoal leaps out of water (Dolphin.DolphinJumpGoal)
struct DolphinJumpGoal {
    interval: i32,
}

impl DolphinJumpGoal {
    const fn new(interval: i32) -> Self {
        Self { interval }
    }

    fn dolphin(mob: &dyn Mob) -> Option<&DolphinEntity> {
        mob.cast_any().downcast_ref::<DolphinEntity>()
    }
}

impl Goal for DolphinJumpGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(dolphin) = Self::dolphin(mob) else {
            return false;
        };
        if dolphin.get_random().random_range(0..self.interval) != 0 {
            return false;
        }
        let entity = dolphin.get_entity();
        entity.is_in_water()
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(dolphin) = Self::dolphin(mob) else {
            return;
        };
        let entity = dolphin.get_entity();
        let yaw_rad = f64::from(entity.yaw.load()).to_radians();
        let fwd_x = -yaw_rad.sin() * 0.6;
        let fwd_z = yaw_rad.cos() * 0.6;
        let mut vel = entity.velocity.load();
        vel.x += fwd_x;
        vel.y = 0.7;
        vel.z += fwd_z;
        entity.velocity.store(vel);
        entity.velocity_dirty.store(true, Relaxed);
        dolphin.play_entity_sound(Sound::EntityDolphinJump, 1.0, 1.0);
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(dolphin) = Self::dolphin(mob) else {
            return false;
        };
        let entity = dolphin.get_entity();
        !entity.on_ground.load(Relaxed) && entity.velocity.load().y.abs() > 0.03
    }
}

// dolphin: Dolphin.DolphinSwimToTreasureGoal, leads to the nearest ocean ruin or shipwreck
#[derive(Default)]
struct DolphinSwimToTreasureGoal {
    stuck: bool,
}

impl DolphinSwimToTreasureGoal {
    fn reached(dolphin: &DolphinEntity, treasure: BlockPos) -> bool {
        let pos = dolphin.get_entity().pos.load();
        let center = Vector3::new(
            f64::from(treasure.0.x) + 0.5,
            pos.y.floor() + 0.5,
            f64::from(treasure.0.z) + 0.5,
        );
        center.squared_distance_to_vec(&pos) < 16.0
    }

    // dolphin: StructureTags.DOLPHIN_LOCATED (ocean ruins, shipwrecks), radius 50
    fn find_treasure(dolphin: &DolphinEntity) -> Option<BlockPos> {
        let entity = dolphin.get_entity();
        let world = entity.world.load();
        let level = &world.level;
        let generator = level.world_gen();
        let global_cache = generator.global_structure_cache()?;
        let ruins = StructureSet::get("ocean_ruins")?;
        let wrecks = StructureSet::get("shipwrecks")?;
        match find_nearest_structure(
            entity.block_pos.load(),
            &[&ruins.placement, &wrecks.placement],
            50,
            level.seed.0 as i64,
            global_cache,
        ) {
            StructureSearch::Found(pos) => Some(pos),
            StructureSearch::NotFound | StructureSearch::Pending => None,
        }
    }

    fn signal(dolphin: &DolphinEntity) {
        let entity = dolphin.get_entity();
        entity.world.load().send_entity_status(
            entity,
            pumpkin_data::entity::EntityStatus::DolphinLookingForTreasure,
            pumpkin_protocol::bedrock::server::actor_event::ActorEventID::None,
        );
    }
}

impl Goal for DolphinSwimToTreasureGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(dolphin) = mob.cast_any().downcast_ref::<DolphinEntity>() else {
            return false;
        };
        dolphin.got_fish() && dolphin.get_entity().air_supply.load(Relaxed) >= 100
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(dolphin) = mob.cast_any().downcast_ref::<DolphinEntity>() else {
            return false;
        };
        dolphin.treasure_pos.load().is_some_and(|treasure| {
            !Self::reached(dolphin, treasure)
                && !self.stuck
                && dolphin.get_entity().air_supply.load(Relaxed) >= 100
        })
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(dolphin) = mob.cast_any().downcast_ref::<DolphinEntity>() else {
            return;
        };
        self.stuck = false;
        dolphin.mob_entity.steered().navigator().stop();
        if let Some(treasure) = Self::find_treasure(dolphin) {
            dolphin.treasure_pos.store(Some(treasure));
            Self::signal(dolphin);
        } else {
            self.stuck = true;
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let Some(dolphin) = mob.cast_any().downcast_ref::<DolphinEntity>() else {
            return;
        };
        if dolphin
            .treasure_pos
            .load()
            .is_none_or(|treasure| Self::reached(dolphin, treasure))
            || self.stuck
        {
            dolphin.set_got_fish(false);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(dolphin) = mob.cast_any().downcast_ref::<DolphinEntity>() else {
            return;
        };
        let Some(treasure) = dolphin.treasure_pos.load() else {
            return;
        };
        let entity = dolphin.get_entity();
        let pos = entity.pos.load();
        let close_to_next = {
            let steered = dolphin.mob_entity.steered();
            let navigator = steered.navigator();
            navigator.is_done()
                || navigator.get_target_pos().is_none_or(|target| {
                    target.to_centered_f64().squared_distance_to_vec(&pos) < 144.0
                })
        };
        if !close_to_next {
            return;
        }
        let target = treasure.to_centered_f64();
        let mut next =
            default_random_pos::get_pos_towards(mob, 16, 1, target, std::f64::consts::PI / 8.0)
                .or_else(|| {
                    default_random_pos::get_pos_towards(
                        mob,
                        8,
                        4,
                        target,
                        std::f64::consts::PI / 2.0,
                    )
                });
        if let Some(candidate) = next {
            let block = BlockPos::floored_v(candidate);
            let world = entity.world.load();
            // dolphin: next step must be water (FluidTags.WATER)
            let in_water = world.get_block(&block) == &pumpkin_data::Block::WATER
                || world.get_block_state(&block).is_waterlogged();
            if !in_water {
                next = default_random_pos::get_pos_towards(
                    mob,
                    8,
                    5,
                    target,
                    std::f64::consts::PI / 2.0,
                );
            }
        }
        let Some(next) = next else {
            self.stuck = true;
            return;
        };
        dolphin
            .mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at(mob, next.x, next.y, next.z);
        dolphin.mob_entity.steered().navigator().move_to_coords(
            next.x,
            next.y,
            next.z,
            1.3,
            &dolphin.mob_entity.living_entity,
        );
        if rand::random_range(0..to_goal_ticks(80)) == 0 {
            Self::signal(dolphin);
        }
    }

    fn can_stop(&self) -> bool {
        false
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}
