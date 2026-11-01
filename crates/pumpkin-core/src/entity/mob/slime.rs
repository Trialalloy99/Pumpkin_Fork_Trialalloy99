use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::entity::EntityType;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::Difficulty;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::custom_sound::CustomSound;
use crate::entity::{
    Entity, EntityBase,
    ai::control::{Control, MoveControlTrait, move_control::MoveControl},
    ai::goal::Goal,
    mob::{Mob, MobEntity, equipment::RegionalDifficulty, spawn::SpawnGroupData},
};
use crate::world::World;
use pumpkin_util::random::RandomImpl;
use rand::RngExt;

const SLIME_HUNTS: &[Hunt] = &[
    Hunt::new(1, TargetMatch::Type(&EntityType::PLAYER), true)
        .matching(hunting::within_four_blocks_height),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
];

pub struct SlimeEntity {
    entity: Arc<MobEntity>,
    jump_delay: AtomicI32,
    target_yaw: AtomicCell<f32>,
    is_aggressive: AtomicBool,
    was_on_ground: AtomicBool,
    pub squish: AtomicCell<f32>,
    pub target_squish: AtomicCell<f32>,
    pub o_squish: AtomicCell<f32>,
    speed_modifier: AtomicCell<Option<f64>>,
    has_split: AtomicBool,
}

impl SlimeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let slime = Self {
            entity: Arc::new(mob_entity),
            jump_delay: AtomicI32::new(0),
            target_yaw: AtomicCell::new(0.0),
            is_aggressive: AtomicBool::new(false),
            was_on_ground: AtomicBool::new(false),
            squish: AtomicCell::new(0.0),
            target_squish: AtomicCell::new(0.0),
            o_squish: AtomicCell::new(0.0),
            speed_modifier: AtomicCell::new(None),
            has_split: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(slime);

        {
            let mut move_control = mob_arc
                .entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *move_control = Box::new(SlimeMoveControl::new(Arc::downgrade(&mob_arc)));

            let goal_selector = &mob_arc.entity.goals_selector;
            let target_selector = &mob_arc.entity.target_selector;

            goal_selector.add_goal(1, Box::new(SlimeFloatGoal::new(Arc::downgrade(&mob_arc))));
            goal_selector.add_goal(2, Box::new(SlimeAttackGoal::new(Arc::downgrade(&mob_arc))));
            goal_selector.add_goal(
                3,
                Box::new(SlimeRandomDirectionGoal::new(Arc::downgrade(&mob_arc))),
            );
            goal_selector.add_goal(
                5,
                Box::new(SlimeKeepOnJumpingGoal::new(Arc::downgrade(&mob_arc))),
            );

            hunting::apply(target_selector, &mob_arc.entity, SLIME_HUNTS);
        };

        mob_arc.set_size(1, true);

        mob_arc
    }

    pub fn set_size(&self, size: i32, update_health: bool) {
        let actual_size = size.clamp(1, 127);
        let entity = &self.entity.living_entity.entity;
        entity.data.store(actual_size, Ordering::Relaxed);

        // Update attributes
        {
            let mut attributes = self
                .entity
                .living_entity
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(health) = attributes.get_mut(&Attributes::MAX_HEALTH.id) {
                health.base_value = (actual_size * actual_size) as f64;
                health.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
                speed.base_value = (0.2 + 0.1 * actual_size as f32) as f64;
                speed.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(damage) = attributes.get_mut(&Attributes::ATTACK_DAMAGE.id) {
                damage.base_value = actual_size as f64;
                damage.dirty.store(true, Ordering::Relaxed);
            }
        }

        if update_health {
            let max_health = self
                .entity
                .living_entity
                .get_attribute_value(&Attributes::MAX_HEALTH) as f32;
            self.entity.living_entity.health.store(max_health);
        }

        // Refresh dimensions
        let scaled_dimensions = EntityDimensions {
            width: entity.entity_type.dimension[0] * actual_size as f32,
            height: entity.entity_type.dimension[1] * actual_size as f32,
            eye_height: entity.entity_type.eye_height * actual_size as f32,
        };
        entity.set_default_dimensions(scaled_dimensions);

        entity.set_synced_data(pumpkin_data::tracked_data::slime::ID_SIZE, actual_size);
        if let Some(meta) = self.mob_metadata() {
            entity.send_bedrock_actor_data(&meta.to_bedrock_actor_data());
        }
    }

    pub fn get_size(&self) -> i32 {
        self.entity
            .living_entity
            .entity
            .data
            .load(Ordering::Relaxed)
    }

    pub fn is_tiny(&self) -> bool {
        self.get_size() <= 1
    }

    pub fn check_slime_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        if world.level_info.load().difficulty == Difficulty::Peaceful {
            return false;
        }

        let mut rng = rand::rng();
        let biome = world.get_biome(pos);
        if biome.has_tag(&pumpkin_data::tag::WorldgenBiome::MINECRAFT_ALLOWS_SURFACE_SLIME_SPAWNS)
            && pos.0.y > 50
            && pos.0.y < 70
            && rng.random::<f32>() < world.surface_slime_spawn_chance(pos)
            && world.get_max_local_raw_brightness(pos) <= rng.random_range(0..8)
        {
            return MobEntity::check_mob_spawn_rules(world, pos);
        }

        // Slime Chunk Spawning
        let chunk_pos = pos.chunk_position();
        let world_seed = world.level.seed.0;
        let slime_seed = pumpkin_util::random::seed_slime_chunk(
            chunk_pos.x,
            chunk_pos.y,
            world_seed,
            987_234_911,
        );
        let mut slime_rand = pumpkin_util::random::legacy_rand::LegacyRand::from_seed(slime_seed);

        if rng.random_range(0..10) == 0 && slime_rand.next_bounded_i32(10) == 0 && pos.0.y < 40 {
            return MobEntity::check_mob_spawn_rules(world, pos);
        }

        false
    }

    fn get_jump_delay() -> i32 {
        rand::random_range(10..30)
    }

    fn do_play_jump_sound(&self) -> bool {
        self.get_size() > 0
    }

    fn get_jump_sound(&self) -> Sound {
        if self.entity.living_entity.entity.entity_type == &EntityType::MAGMA_CUBE {
            return Sound::EntityMagmaCubeJump;
        }
        if self.is_tiny() {
            Sound::EntitySlimeJumpSmall
        } else {
            Sound::EntitySlimeJump
        }
    }

    fn get_squish_sound(&self) -> Sound {
        if self.entity.living_entity.entity.entity_type == &EntityType::MAGMA_CUBE {
            return if self.is_tiny() {
                Sound::EntityMagmaCubeSquishSmall
            } else {
                Sound::EntityMagmaCubeSquish
            };
        }
        if self.is_tiny() {
            Sound::EntitySlimeSquishSmall
        } else {
            Sound::EntitySlimeSquish
        }
    }

    fn get_sound_volume(&self) -> f32 {
        0.4 * self.get_size() as f32
    }

    fn get_sound_pitch(&self) -> f32 {
        let pitch_adjuster = if self.is_tiny() { 1.4 } else { 0.8 };
        (rand::random_range(0.0..1.0) - rand::random_range(0.0..1.0)) * 0.2 + 1.0 * pitch_adjuster
    }
}

impl CustomSound for SlimeEntity {
    fn death_sound(&self) -> Option<Sound> {
        let size = self.get_size();
        Some(if size == 1 {
            Sound::EntitySlimeDeathSmall
        } else {
            Sound::EntitySlimeDeath
        })
    }
    fn hurt_sound(&self) -> Option<Sound> {
        let size = self.get_size();
        Some(if size == 1 {
            Sound::EntitySlimeHurtSmall
        } else {
            Sound::EntitySlimeHurt
        })
    }
}

impl Mob for SlimeEntity {
    fn as_custom_sound(&self) -> Option<&dyn crate::entity::custom_sound::CustomSound> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int("Size", self.get_size() - 1);
        nbt.put_bool("wasOnGround", self.was_on_ground.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_size(nbt.get_int("Size").unwrap_or(0) + 1, false);
        self.was_on_ground.store(
            nbt.get_bool("wasOnGround").unwrap_or(false),
            Ordering::Relaxed,
        );
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity
    }

    fn mob_metadata(&self) -> Option<crate::entity::mob::MobMetadata> {
        Some(crate::entity::mob::MobMetadata::new().slime_size(self.get_size()))
    }

    // slime: random spawn size (Slime.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let difficulty = RegionalDifficulty::at(world, self.get_entity().pos.load());
        let mut size_scale = rand::random_range(0..3);
        if rand::random::<f32>() < 0.5 * difficulty.special_multiplier {
            size_scale += 1;
        }
        self.set_size(1 << size_scale, true);
        self.finalize_spawn_super(group_data)
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.get_size() as u32
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.o_squish.store(self.squish.load());
        self.squish
            .store(self.squish.load() + (self.target_squish.load() - self.squish.load()) * 0.5);

        let on_ground = self
            .entity
            .living_entity
            .entity
            .on_ground
            .load(Ordering::Relaxed);
        let was_on_ground = self.was_on_ground.load(Ordering::Relaxed);

        if on_ground && !was_on_ground {
            let world = self.entity.living_entity.entity.world.load();
            let entity = &self.entity.living_entity.entity;
            let pos = entity.pos.load();
            let width = entity.entity_dimension.load().width;
            let radius = width;
            let particle = if entity.entity_type == &EntityType::MAGMA_CUBE {
                pumpkin_data::particle::Particle::Flame
            } else {
                pumpkin_data::particle::Particle::ItemSlime
            };
            for _ in 0..(width * 32.0) as i32 {
                let direction = rand::random_range(0.0..std::f64::consts::TAU);
                let distance = rand::random_range(0.5..1.0) * f64::from(radius);
                world.spawn_particle(
                    Vector3::new(
                        pos.x + direction.sin() * distance,
                        pos.y,
                        pos.z + direction.cos() * distance,
                    ),
                    Vector3::new(0.0, 0.0, 0.0),
                    0.0,
                    1,
                    particle,
                );
            }
            world.play_sound_fine(
                self.get_squish_sound(),
                SoundCategory::Hostile,
                &pos,
                self.get_sound_volume(),
                ((rand::random_range(0.0..1.0) - rand::random_range(0.0..1.0)) * 0.2 + 1.0) / 0.8,
            );

            self.target_squish.store(-0.5);
        } else if !on_ground && was_on_ground {
            self.target_squish.store(1.0);
        }

        self.was_on_ground.store(on_ground, Ordering::Relaxed);
        let damping = if self.entity.living_entity.entity.entity_type == &EntityType::MAGMA_CUBE {
            0.9
        } else {
            0.6
        };
        self.target_squish
            .store(self.target_squish.load() * damping);

        self.is_aggressive.store(false, Ordering::Relaxed);
        self.speed_modifier.store(None);
    }

    fn mob_player_collision(&self, player: &Arc<crate::entity::player::Player>) {
        if !self.is_tiny() {
            // dealDamage
            let damaged = self.entity.try_attack(self, &**player);
            if damaged {
                self.on_attack(&**player);
            }
        }
    }

    fn mob_entity_collision(&self, entity: &dyn EntityBase) {
        if !self.is_tiny() && entity.get_entity().entity_type == &EntityType::IRON_GOLEM {
            let damaged = self.entity.try_attack(self, entity);
            if damaged {
                self.on_attack(entity);
            }
        }
    }

    fn post_tick(&self) {
        if self.entity.living_entity.dead.load(Ordering::Relaxed)
            && self.get_size() > 1
            && self
                .has_split
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let size = self.get_size();
            let world = self.entity.living_entity.entity.world.load();
            let pos = self.entity.living_entity.entity.pos.load();
            let half_size = size / 2;
            let count = 2 + rand::random_range(0..3);

            let width = self
                .entity
                .living_entity
                .entity
                .entity_dimension
                .load()
                .width;
            let xz_offset = width / 2.0;

            for i in 0..count {
                let xd = ((i % 2) as f32 - 0.5) * xz_offset;
                let zd = ((i / 2) as f32 - 0.5) * xz_offset;

                let new_pos = pumpkin_util::math::vector3::Vector3::new(
                    pos.x + xd as f64,
                    pos.y + 0.5,
                    pos.z + zd as f64,
                );
                let slime_like = crate::entity::r#type::from_type(
                    self.entity.living_entity.entity.entity_type,
                    new_pos,
                    &world,
                    uuid::Uuid::new_v4(),
                );
                if let Some(slime) = slime_like.cast_any().downcast_ref::<Self>() {
                    slime.set_size(half_size, true);
                } else if let Some(cube) = slime_like
                    .cast_any()
                    .downcast_ref::<crate::entity::mob::magma_cube::MagmaCubeEntity>(
                ) {
                    cube.slime.set_size(half_size, true);
                }
                let child = slime_like.get_entity();
                child.yaw.store(rand::random_range(0.0..360.0));
                child
                    .custom_name
                    .store(self.entity.living_entity.entity.custom_name.load_full());
                child.invulnerable.store(
                    self.entity
                        .living_entity
                        .entity
                        .invulnerable
                        .load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                world.spawn_entity_non_save(slime_like);
            }
        }
    }
}

pub struct SlimeMoveControl {
    slime: Weak<SlimeEntity>,
}

impl SlimeMoveControl {
    #[must_use]
    pub const fn new(slime: Weak<SlimeEntity>) -> Self {
        Self { slime }
    }
}

impl Control for SlimeMoveControl {}

impl MoveControlTrait for SlimeMoveControl {
    fn set_wanted_position(&mut self, _x: f64, _y: f64, _z: f64, speed_modifier: f64) {
        if let Some(slime) = self.slime.upgrade() {
            slime.speed_modifier.store(Some(speed_modifier));
        }
    }

    fn strafe(&mut self, _forward: f32, _right: f32) {
        if let Some(slime) = self.slime.upgrade() {
            slime.speed_modifier.store(None);
        }
    }

    fn has_wanted(&self) -> bool {
        self.slime
            .upgrade()
            .is_some_and(|slime| slime.speed_modifier.load().is_some())
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(slime) = self.slime.upgrade() else {
            return;
        };
        let mob_entity = mob.get_mob_entity();
        let living_entity = &mob_entity.living_entity;
        let entity = &living_entity.entity;

        let current_yaw = entity.yaw.load();
        let new_yaw =
            MoveControl::rotlerp(current_yaw, slime.target_yaw.load(), MoveControl::MAX_TURN);
        entity.yaw.store(new_yaw);
        entity.head_yaw.store(new_yaw);
        entity.body_yaw.store(new_yaw);

        let Some(speed_modifier) = slime.speed_modifier.take() else {
            let input = living_entity.movement_input.load();
            living_entity
                .movement_input
                .store(Vector3::new(input.x, input.y, 0.0));
            return;
        };
        mob_entity.set_speed(
            (speed_modifier * living_entity.get_attribute_value(&Attributes::MOVEMENT_SPEED))
                as f32,
        );
        if entity.on_ground.load(Ordering::Relaxed) {
            let delay = slime.jump_delay.fetch_sub(1, Ordering::Relaxed);
            if delay <= 0 {
                let mut next_delay = SlimeEntity::get_jump_delay();
                if entity.entity_type == &EntityType::MAGMA_CUBE {
                    next_delay *= 4;
                }
                if slime.is_aggressive.load(Ordering::Relaxed) {
                    next_delay /= 3;
                }
                slime.jump_delay.store(next_delay, Ordering::Relaxed);
                mob_entity
                    .jump_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .jump();
                if slime.do_play_jump_sound() {
                    let world = entity.world.load();
                    world.play_sound_fine(
                        slime.get_jump_sound(),
                        SoundCategory::Hostile,
                        &entity.pos.load(),
                        slime.get_sound_volume(),
                        slime.get_sound_pitch(),
                    );
                }
            } else {
                mob_entity.set_speed(0.0);
                let input = living_entity.movement_input.load();
                living_entity
                    .movement_input
                    .store(Vector3::new(0.0, input.y, 0.0));
            }
        }
    }
}

pub struct SlimeFloatGoal {
    slime: Weak<SlimeEntity>,
}

impl SlimeFloatGoal {
    pub const fn new(slime: Weak<SlimeEntity>) -> Self {
        Self { slime }
    }
}

impl Goal for SlimeFloatGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.slime.upgrade().is_some_and(|slime| {
            let entity = &slime.entity.living_entity.entity;
            entity.touching_water.load(Ordering::Relaxed)
                || entity.touching_lava.load(Ordering::Relaxed)
        })
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(slime) = self.slime.upgrade() else {
            return;
        };
        if rand::random_range(0.0..1.0) < 0.8 {
            slime
                .entity
                .jump_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .jump();
        }
        slime.speed_modifier.store(Some(1.2));
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> crate::entity::ai::goal::Controls {
        crate::entity::ai::goal::Controls::JUMP | crate::entity::ai::goal::Controls::MOVE
    }
}

pub struct SlimeAttackGoal {
    slime: Weak<SlimeEntity>,
    grow_tired_timer: i32,
}

impl SlimeAttackGoal {
    pub const fn new(slime: Weak<SlimeEntity>) -> Self {
        Self {
            slime,
            grow_tired_timer: 0,
        }
    }
}

impl Goal for SlimeAttackGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.slime
            .upgrade()
            .is_some_and(|slime| slime.entity.get_target().is_some())
    }

    fn start(&mut self, _mob: &dyn Mob) {
        self.grow_tired_timer = 300;
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.grow_tired_timer > 0
            && self
                .slime
                .upgrade()
                .is_some_and(|slime| slime.entity.get_target().is_some())
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(slime) = self.slime.upgrade() else {
            return;
        };
        self.grow_tired_timer -= 1;
        if let Some(target) = slime.entity.get_target() {
            let pos = target.get_entity().pos.load();
            let my_pos = slime.entity.living_entity.entity.pos.load();
            let dx = pos.x - my_pos.x;
            let dz = pos.z - my_pos.z;
            let yaw = dx.atan2(dz).to_degrees() as f32;
            slime.target_yaw.store(yaw);
        }
        slime.is_aggressive.store(true, Ordering::Relaxed);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> crate::entity::ai::goal::Controls {
        crate::entity::ai::goal::Controls::LOOK
    }
}

pub struct SlimeRandomDirectionGoal {
    slime: Weak<SlimeEntity>,
    chosen_degrees: f32,
    next_randomize_time: i32,
}

impl SlimeRandomDirectionGoal {
    pub const fn new(slime: Weak<SlimeEntity>) -> Self {
        Self {
            slime,
            chosen_degrees: 0.0,
            next_randomize_time: 0,
        }
    }
}

impl Goal for SlimeRandomDirectionGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.slime.upgrade().is_some_and(|slime| {
            let entity = &slime.entity.living_entity.entity;
            slime.entity.get_target().is_none()
                && (entity.on_ground.load(Ordering::Relaxed)
                    || entity.touching_water.load(Ordering::Relaxed)
                    || entity.touching_lava.load(Ordering::Relaxed))
        })
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(slime) = self.slime.upgrade() else {
            return;
        };
        self.next_randomize_time -= 1;
        if self.next_randomize_time <= 0 {
            self.next_randomize_time = rand::random_range(40..100);
            self.chosen_degrees = rand::random_range(0.0..360.0);
        }
        slime.target_yaw.store(self.chosen_degrees);
        slime.is_aggressive.store(false, Ordering::Relaxed);
    }

    fn controls(&self) -> crate::entity::ai::goal::Controls {
        crate::entity::ai::goal::Controls::LOOK
    }
}

pub struct SlimeKeepOnJumpingGoal {
    slime: Weak<SlimeEntity>,
}

impl SlimeKeepOnJumpingGoal {
    #[must_use]
    pub const fn new(slime: Weak<SlimeEntity>) -> Self {
        Self { slime }
    }
}

impl Goal for SlimeKeepOnJumpingGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.slime
            .upgrade()
            .is_some_and(|slime| !slime.entity.living_entity.entity.has_vehicle())
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        if let Some(slime) = self.slime.upgrade() {
            slime.speed_modifier.store(Some(1.0));
        }
    }

    fn controls(&self) -> crate::entity::ai::goal::Controls {
        crate::entity::ai::goal::Controls::JUMP | crate::entity::ai::goal::Controls::MOVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;

    #[tokio::test]
    async fn zero_speed_requests_are_consumed_as_movement_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let slime = SlimeEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::SLIME,
        ));
        let living = &slime.entity.living_entity;
        living.entity.on_ground.store(true, Ordering::Relaxed);
        slime.jump_delay.store(10, Ordering::Relaxed);
        slime.entity.set_speed(0.5);
        let (speed, delay, pending) = {
            let mut control = slime
                .entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.set_wanted_position(0.0, 64.0, 8.0, 0.0);
            control.tick(&*slime);
            (
                living.get_speed(),
                slime.jump_delay.load(Ordering::Relaxed),
                control.has_wanted(),
            )
        };
        slime.entity.set_speed(0.5);
        let waiting_speed = {
            let mut control = slime
                .entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.tick(&*slime);
            living.get_speed()
        };
        world.level.shutdown().await;
        assert_eq!((speed, delay, pending), (0.0, 9, false));
        assert_eq!(waiting_speed, 0.5);
        assert_eq!(living.movement_input.load().z, 0.0);
        Ok(())
    }
}
