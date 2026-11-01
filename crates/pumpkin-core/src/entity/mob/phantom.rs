use crate::entity::{
    Entity, EntityBase,
    ai::goal::{Controls, Goal},
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
};
use crate::world::World;
use pumpkin_data::{
    attributes::Attributes,
    entity::EntityType,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{boundingbox::EntityDimensions, position::BlockPos, vector3::Vector3};
use pumpkin_world::chunk::ChunkHeightmapType;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

const TICKS_PER_FLAP: i32 = 25;
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttackPhase {
    Circle = 0,
    Swoop = 1,
}

pub struct PhantomEntity {
    pub mob_entity: MobEntity,
    move_target: Mutex<Vector3<f64>>,
    anchor: Mutex<BlockPos>,
    phase: AtomicU8,
    size: AtomicI32,
}

impl PhantomEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let pos = entity.pos.load();
        let phantom = Arc::new(Self {
            mob_entity: MobEntity::new(entity),
            move_target: Mutex::new(pos),
            anchor: Mutex::new(BlockPos::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32 + 5,
                pos.z.floor() as i32,
            )),
            phase: AtomicU8::new(0),
            size: AtomicI32::new(0),
        });
        phantom.get_entity().set_has_no_gravity(true);
        phantom.set_size(0);
        let weak = Arc::downgrade(&phantom);
        phantom
            .mob_entity
            .goals_selector
            .add_goal(1, Box::new(AttackStrategyGoal::new(weak.clone())));
        phantom
            .mob_entity
            .goals_selector
            .add_goal(2, Box::new(SweepGoal::new(weak.clone())));
        phantom
            .mob_entity
            .goals_selector
            .add_goal(3, Box::new(CircleGoal::new(weak.clone())));
        phantom
            .mob_entity
            .target_selector
            .add_goal(1, Box::new(FindTargetGoal::new(weak)));
        phantom
    }
    fn attack_phase(&self) -> AttackPhase {
        if self.phase.load(Ordering::Relaxed) == 1 {
            AttackPhase::Swoop
        } else {
            AttackPhase::Circle
        }
    }
    fn set_phase(&self, phase: AttackPhase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }
    fn set_size(&self, size: i32) {
        let size = size.clamp(0, 64);
        self.size.store(size, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::phantom::ID_SIZE, size);
        let scale = 1.0 + 0.15 * size as f32;
        entity.set_default_dimensions(EntityDimensions {
            width: entity.entity_type.dimension[0] * scale,
            height: entity.entity_type.dimension[1] * scale,
            eye_height: entity.entity_type.eye_height * scale,
        });
        let mut attrs = self
            .mob_entity
            .living_entity
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(damage) = attrs.get_mut(&Attributes::ATTACK_DAMAGE.id) {
            damage.base_value = 6.0 + f64::from(size);
            damage.dirty.store(true, Ordering::Relaxed);
        }
    }
    fn move_towards_target(&self, speed: f64) {
        let target = *self
            .move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entity = self.get_entity();
        let delta = target - entity.pos.load();
        let length = delta.length();
        if length < 1.0e-5 {
            return;
        }
        let wanted_yaw = (-delta.x.atan2(delta.z).to_degrees()) as f32;
        let yaw = entity.yaw.load();
        let yaw = yaw + ((wanted_yaw - yaw + 540.0).rem_euclid(360.0) - 180.0).clamp(-4.0, 4.0);
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);
        let velocity = entity.velocity.load();
        let horizontal = velocity.x.hypot(velocity.z);
        let wanted = (speed * delta.x.hypot(delta.z) / length).max(0.05);
        let horizontal = horizontal + (wanted - horizontal) * 0.2;
        let radians = f64::from(yaw).to_radians();
        entity.velocity.store(Vector3::new(
            -radians.sin() * horizontal,
            velocity.y + (speed * delta.y / length - velocity.y) * 0.2,
            radians.cos() * horizontal,
        ));
    }
    fn near_target(&self) -> bool {
        let target = *self
            .move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target)
            < 4.0
    }
}

impl CustomSound for PhantomEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPhantomDeath)
    }
    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPhantomHurt)
    }
}
impl Mob for PhantomEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPhantomAmbient)
    }
    fn get_mob_gravity(&self) -> f64 {
        0.0
    }
    fn max_fall_distance(&self) -> i32 {
        0
    }
    fn get_base_experience_reward(&self) -> u32 {
        5 + self.size.load(Ordering::Relaxed) as u32
    }
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_size(0);
        let pos = self.get_entity().block_pos.load();
        *self
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pos.up().up().up().up().up();
        self.finalize_spawn_super(data)
    }
    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let p = *self
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nbt.put_int("AX", p.0.x);
        nbt.put_int("AY", p.0.y);
        nbt.put_int("AZ", p.0.z);
        nbt.put_int("Size", self.size.load(Ordering::Relaxed));
    }
    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let (Some(x), Some(y), Some(z)) =
            (nbt.get_int("AX"), nbt.get_int("AY"), nbt.get_int("AZ"))
        {
            *self
                .anchor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = BlockPos::new(x, y, z);
        }
        self.set_size(nbt.get_int("Size").unwrap_or(0));
    }
    fn mob_is_flapping(&self) -> bool {
        let e = self.get_entity();
        e.entity_id
            .wrapping_mul(3)
            .wrapping_add(e.tick_count.load(Ordering::Relaxed))
            % TICKS_PER_FLAP
            == 0
    }
}

struct AttackStrategyGoal {
    phantom: Weak<PhantomEntity>,
    next_sweep: i32,
}
impl AttackStrategyGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            next_sweep: 0,
        }
    }

    fn set_anchor_above_target(phantom: &PhantomEntity) {
        let Some(target) = phantom.mob_entity.get_target() else {
            return;
        };
        let target = target.get_entity().block_pos.load();
        let sea_level = phantom.get_entity().world.load().sea_level;
        let y = (target.0.y + 20 + rand::random_range(0..20)).max(sea_level + 1);
        *phantom
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            BlockPos::new(target.0.x, y, target.0.z);
    }
}
impl Goal for AttackStrategyGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom
            .upgrade()
            .is_some_and(|p| p.mob_entity.get_target().is_some())
    }
    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }
    fn start(&mut self, _mob: &dyn Mob) {
        self.next_sweep = 10;
        if let Some(p) = self.phantom.upgrade() {
            p.set_phase(AttackPhase::Circle);
            Self::set_anchor_above_target(&p);
        }
    }
    fn stop(&mut self, _mob: &dyn Mob) {
        if let Some(p) = self.phantom.upgrade() {
            let e = p.get_entity();
            let pos = e.block_pos.load();
            let top = e.world.load().get_heightmap_height(
                ChunkHeightmapType::MotionBlocking,
                pos.0.x,
                pos.0.z,
            );
            *p.anchor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                BlockPos::new(pos.0.x, top + 10 + rand::random_range(0..20), pos.0.z);
        }
    }
    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        if p.attack_phase() == AttackPhase::Circle {
            self.next_sweep -= 1;
        }
        if self.next_sweep <= 0 {
            p.set_phase(AttackPhase::Swoop);
            Self::set_anchor_above_target(&p);
            self.next_sweep = (8 + rand::random_range(0..4)) * 20;
            let e = p.get_entity();
            e.world.load().play_sound_fine(
                Sound::EntityPhantomSwoop,
                SoundCategory::Hostile,
                &e.pos.load(),
                10.0,
                0.95 + rand::random::<f32>() * 0.1,
            );
        }
    }
}

struct SweepGoal {
    phantom: Weak<PhantomEntity>,
    cat_check: i32,
}
impl SweepGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            cat_check: 0,
        }
    }
}
impl Goal for SweepGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom.upgrade().is_some_and(|p| {
            p.attack_phase() == AttackPhase::Swoop && p.mob_entity.get_target().is_some()
        })
    }
    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }
    fn stop(&mut self, _mob: &dyn Mob) {
        if let Some(p) = self.phantom.upgrade() {
            p.mob_entity.set_target(None);
            p.set_phase(AttackPhase::Circle);
        }
    }
    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        let Some(target) = p.mob_entity.get_target() else {
            return;
        };
        self.cat_check -= 1;
        if self.cat_check <= 0 {
            self.cat_check = 20;
            let area = p.get_entity().bounding_box.load().expand(16.0, 16.0, 16.0);
            let cats: Vec<_> = p
                .get_entity()
                .world
                .load()
                .get_entities_at_box(&area)
                .iter()
                .filter(|e| {
                    e.get_entity().entity_type == &EntityType::CAT && e.get_entity().is_alive()
                })
                .cloned()
                .collect();
            for cat in &cats {
                let entity = cat.get_entity();
                entity.world.load().play_sound_fine(
                    Sound::EntityCatHiss,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                    1.0,
                    1.0,
                );
            }
            if !cats.is_empty() {
                p.set_phase(AttackPhase::Circle);
                return;
            }
        }
        let te = target.get_entity();
        let pos = te.pos.load();
        *p.move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Vector3::new(pos.x, pos.y + te.get_eye_height() * 0.5, pos.z);
        if p.get_entity()
            .bounding_box
            .load()
            .expand(0.2, 0.2, 0.2)
            .intersects(&te.bounding_box.load())
        {
            p.mob_entity.try_attack(p.as_ref(), target.as_ref());
            p.set_phase(AttackPhase::Circle);
        } else if p.get_entity().horizontal_collision.load(Ordering::Relaxed)
            || p.mob_entity
                .living_entity
                .hurt_cooldown
                .load(Ordering::Relaxed)
                > 0
        {
            p.set_phase(AttackPhase::Circle);
        }
        p.move_towards_target(1.2);
    }
    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

struct CircleGoal {
    phantom: Weak<PhantomEntity>,
    angle: f32,
    distance: f32,
    height: f32,
    direction: f32,
}
impl CircleGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            angle: 0.0,
            distance: 5.0,
            height: 0.0,
            direction: 1.0,
        }
    }
    fn next(&mut self, p: &PhantomEntity) {
        self.angle += self.direction * 15.0f32.to_radians();
        let a = *p
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *p.move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Vector3::new(
            a.0.x as f64 + f64::from(self.distance * self.angle.cos()),
            a.0.y as f64 - 4.0 + f64::from(self.height),
            a.0.z as f64 + f64::from(self.distance * self.angle.sin()),
        );
    }
}
impl Goal for CircleGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom
            .upgrade()
            .is_some_and(|p| p.attack_phase() == AttackPhase::Circle)
    }
    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }
    fn start(&mut self, _mob: &dyn Mob) {
        self.distance = rand::random_range(5.0..15.0);
        self.height = rand::random_range(-4.0..5.0);
        self.direction = if rand::random() { 1.0 } else { -1.0 };
        if let Some(p) = self.phantom.upgrade() {
            self.next(&p);
        }
    }
    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        if rand::random_range(0..350) == 0 {
            self.height = rand::random_range(-4.0..5.0);
        }
        if rand::random_range(0..250) == 0 {
            self.distance += 1.0;
            if self.distance > 15.0 {
                self.distance = 5.0;
                self.direction = -self.direction;
            }
        }
        if rand::random_range(0..450) == 0 {
            self.angle = rand::random_range(0.0..std::f32::consts::TAU);
            self.next(&p);
        }
        if p.near_target() {
            self.next(&p);
        }
        let e = p.get_entity();
        let bp = e.block_pos.load();
        let y = p
            .move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .y;
        let world = e.world.load();
        if y < e.pos.load().y && !world.get_block_state(&bp.down()).is_air() {
            self.height = self.height.max(1.0);
            self.next(&p);
        }
        if y > e.pos.load().y && !world.get_block_state(&bp.up()).is_air() {
            self.height = self.height.min(-1.0);
            self.next(&p);
        }
        p.move_towards_target(0.5);
    }
    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

struct FindTargetGoal {
    phantom: Weak<PhantomEntity>,
    delay: i32,
}
impl FindTargetGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self { phantom, delay: 20 }
    }
}
impl Goal for FindTargetGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.delay -= 1;
        if self.delay > 0 {
            return false;
        }
        self.delay = 60;
        let Some(p) = self.phantom.upgrade() else {
            return false;
        };
        let e = p.get_entity();
        let mut players = e
            .world
            .load()
            .get_players_at_box(&e.bounding_box.load().expand(16.0, 64.0, 16.0));
        players.sort_by(|a, b| {
            b.get_entity()
                .pos
                .load()
                .y
                .total_cmp(&a.get_entity().pos.load().y)
        });
        if let Some(player) = players.into_iter().find(|x| {
            x.get_entity().is_alive() && !x.is_creative() && !x.get_entity().is_spectator()
        }) {
            p.mob_entity.set_target(Some(player));
            return true;
        }
        false
    }
    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom
            .upgrade()
            .and_then(|p| p.mob_entity.get_target())
            .is_some_and(|t| {
                t.get_entity().is_alive()
                    && !t.get_entity().is_spectator()
                    && t.get_player().is_none_or(|p| !p.is_creative())
            })
    }
}
