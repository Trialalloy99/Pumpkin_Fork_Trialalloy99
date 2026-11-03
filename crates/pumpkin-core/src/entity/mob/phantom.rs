use crate::entity::{
    Entity, EntityBase,
    ai::{
        control::{Control, LookControlTrait, MoveControlTrait},
        goal::{Controls, Goal, to_goal_ticks},
        target_predicate::TargetPredicate,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::cat::CatEntity,
};
use crate::world::World;
use pumpkin_data::world::WorldEvent;
use pumpkin_data::{
    attributes::Attributes,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{
    approach, approach_degrees, boundingbox::EntityDimensions, degrees_difference_abs,
    position::BlockPos, vector3::Vector3, wrap_degrees,
};
use pumpkin_world::chunk::ChunkHeightmapType;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

// phantom: Phantom.TICKS_PER_FLAP = ceil(24.166098F)
const TICKS_PER_FLAP: i32 = 25;
const MAX_SIZE: i32 = 64;
const XP_REWARD: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttackPhase {
    Circle = 0,
    Swoop = 1,
}

type SharedVec = Arc<Mutex<Vector3<f64>>>;

pub struct PhantomEntity {
    pub mob_entity: MobEntity,
    move_target: SharedVec,
    anchor: Mutex<Option<BlockPos>>,
    phase: AtomicU8,
    size: AtomicI32,
}

impl PhantomEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let move_target: SharedVec = Arc::new(Mutex::new(Vector3::new(0.0, 0.0, 0.0)));
        let phantom = Arc::new(Self {
            mob_entity: MobEntity::new(entity),
            move_target: move_target.clone(),
            anchor: Mutex::new(None),
            phase: AtomicU8::new(0),
            size: AtomicI32::new(0),
        });
        phantom.get_entity().set_has_no_gravity(true);
        phantom.set_size(0);
        *phantom
            .mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(PhantomMoveControl::new(move_target));
        *phantom
            .mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Box::new(PhantomLookControl);
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

    fn anchor(&self) -> Option<BlockPos> {
        *self
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_anchor(&self, anchor: Option<BlockPos>) {
        *self
            .anchor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = anchor;
    }

    fn move_target(&self) -> Vector3<f64> {
        *self
            .move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_move_target(&self, target: Vector3<f64>) {
        *self
            .move_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }

    // phantom: setPhantomSize + updatePhantomSizeInfo
    fn set_size(&self, size: i32) {
        let size = size.clamp(0, MAX_SIZE);
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
            damage.base_value = f64::from(6 + size);
            damage.dirty.store(true, Ordering::Relaxed);
        }
    }

    // phantom: TargetingConditions.test(DEFAULT) on the current target
    fn can_attack(&self, target: &dyn EntityBase) -> bool {
        let world = self.get_entity().world.load();
        TargetPredicate::create_attackable().test(&world, Some(self as &dyn EntityBase), target)
    }

    // phantom: PhantomMoveTargetGoal.touchingTarget
    fn touching_target(&self) -> bool {
        self.get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&self.move_target())
            < 4.0
    }
}

// phantom: PhantomLookControl does nothing
struct PhantomLookControl;

impl Control for PhantomLookControl {}

impl LookControlTrait for PhantomLookControl {
    fn tick(&mut self, _mob: &dyn Mob) {}

    fn look_at_with_range(&mut self, _x: f64, _y: f64, _z: f64, _yaw: f32, _pitch: f32) {}
}

// phantom: Phantom.PhantomMoveControl
struct PhantomMoveControl {
    target: SharedVec,
    speed: f32,
}

impl PhantomMoveControl {
    const fn new(target: SharedVec) -> Self {
        Self { target, speed: 0.1 }
    }
}

impl Control for PhantomMoveControl {}

impl MoveControlTrait for PhantomMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        if entity.horizontal_collision.load(Ordering::Relaxed) {
            entity.yaw.store(entity.yaw.load() + 180.0);
            self.speed = 0.1;
        }
        let target = *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pos = entity.pos.load();
        let mut tdx = target.x - pos.x;
        let tdy = target.y - pos.y;
        let mut tdz = target.z - pos.z;
        let mut sd = tdx.hypot(tdz);
        if sd.abs() <= f64::from(1.0e-5f32) {
            return;
        }
        let y_relative_scale = 1.0 - (tdy * f64::from(0.7f32)).abs() / sd;
        tdx *= y_relative_scale;
        tdz *= y_relative_scale;
        sd = tdx.hypot(tdz);
        let sd2 = (tdx * tdx + tdz * tdz + tdy * tdy).sqrt();
        if sd2 == 0.0 {
            return;
        }
        let previous = entity.yaw.load();
        let angle = tdz.atan2(tdx) as f32;
        let a = wrap_degrees(previous + 90.0);
        let b = wrap_degrees(angle.to_degrees());
        let yaw = approach_degrees(a, b, 4.0) - 90.0;
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);
        if degrees_difference_abs(previous, yaw) < 3.0 {
            self.speed = approach(self.speed, 1.8, 0.005 * (1.8 / self.speed));
        } else {
            self.speed = approach(self.speed, 0.2, 0.025);
        }
        let x_rot = (-((-tdy).atan2(sd).to_degrees())) as f32;
        entity.set_pitch(x_rot);
        let move_angle = (yaw + 90.0).to_radians();
        let speed = f64::from(self.speed);
        let new = Vector3::new(
            speed * f64::from(move_angle.cos()) * (tdx / sd2).abs(),
            speed * f64::from(x_rot.to_radians().sin()) * (tdy / sd2).abs(),
            speed * f64::from(move_angle.sin()) * (tdz / sd2).abs(),
        );
        let movement = entity.velocity.load();
        entity.velocity.store(movement + (new - movement) * 0.2);
        entity.velocity_dirty.store(true, Ordering::Relaxed);
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
    // phantom: xpReward = 5
    fn get_base_experience_reward(&self) -> u32 {
        XP_REWARD
    }
    // phantom: travel -> travelFlying(input, 0.2F)
    fn travel(&self, caller: &dyn EntityBase) -> bool {
        self.mob_entity.living_entity.travel_flying_with(
            caller,
            0.2f32.into(),
            0.2f32.into(),
            0.2f32.into(),
        );
        true
    }
    // phantom: PhantomBodyRotationControl.clientTick
    fn custom_body_rotation(&self) -> bool {
        let entity = self.get_entity();
        entity.head_yaw.store(entity.body_yaw.load());
        entity.body_yaw.store(entity.yaw.load());
        true
    }
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_anchor(Some(self.get_entity().block_pos.load().add(0, 5, 0)));
        self.set_size(0);
        self.finalize_spawn_super(data)
    }
    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        if let Some(p) = self.anchor() {
            nbt.put(
                "anchor_pos",
                pumpkin_nbt::tag::NbtTag::IntArray(vec![p.0.x, p.0.y, p.0.z]),
            );
        }
        nbt.put_int("size", self.size.load(Ordering::Relaxed));
    }
    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some([x, y, z]) = nbt.get_int_array("anchor_pos") {
            self.set_anchor(Some(BlockPos::new(*x, *y, *z)));
        } else if let (Some(x), Some(y), Some(z)) =
            (nbt.get_int("AX"), nbt.get_int("AY"), nbt.get_int("AZ"))
        {
            // fork saves from before the vanilla key names
            self.set_anchor(Some(BlockPos::new(x, y, z)));
        } else {
            self.set_anchor(None);
        }
        self.set_size(
            nbt.get_int("size")
                .or_else(|| nbt.get_int("Size"))
                .unwrap_or(0),
        );
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

// phantom: PhantomAttackStrategyGoal
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
        if phantom.anchor().is_none() {
            return;
        }
        let Some(target) = phantom.mob_entity.get_target() else {
            return;
        };
        let target = target.get_entity().block_pos.load();
        let mut anchor = target.add(0, 20 + rand::random_range(0..20), 0);
        let sea_level = phantom.get_entity().world.load().sea_level;
        if anchor.0.y < sea_level {
            anchor = BlockPos::new(anchor.0.x, sea_level + 1, anchor.0.z);
        }
        phantom.set_anchor(Some(anchor));
    }
}
impl Goal for AttackStrategyGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom.upgrade().is_some_and(|p| {
            p.mob_entity
                .get_target()
                .is_some_and(|t| p.can_attack(t.as_ref()))
        })
    }
    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }
    fn start(&mut self, _mob: &dyn Mob) {
        self.next_sweep = to_goal_ticks(10);
        if let Some(p) = self.phantom.upgrade() {
            p.set_phase(AttackPhase::Circle);
            Self::set_anchor_above_target(&p);
        }
    }
    fn stop(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        if let Some(anchor) = p.anchor() {
            let top = p.get_entity().world.load().get_heightmap_height(
                ChunkHeightmapType::MotionBlocking,
                anchor.0.x,
                anchor.0.z,
            );
            p.set_anchor(Some(BlockPos::new(
                anchor.0.x,
                top + 10 + rand::random_range(0..20),
                anchor.0.z,
            )));
        }
    }
    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        if p.attack_phase() == AttackPhase::Circle {
            self.next_sweep -= 1;
            if self.next_sweep <= 0 {
                p.set_phase(AttackPhase::Swoop);
                Self::set_anchor_above_target(&p);
                self.next_sweep = to_goal_ticks((8 + rand::random_range(0..4)) * 20);
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
}

// phantom: PhantomSweepAttackGoal
struct SweepGoal {
    phantom: Weak<PhantomEntity>,
    scared_of_cat: bool,
    cat_search_tick: i32,
}
impl SweepGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            scared_of_cat: false,
            cat_search_tick: 0,
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
        let Some(p) = self.phantom.upgrade() else {
            return false;
        };
        let Some(target) = p.mob_entity.get_target() else {
            return false;
        };
        if !target.get_entity().is_alive() {
            return false;
        }
        if target.get_entity().is_spectator()
            || target
                .get_player()
                .is_some_and(crate::entity::player::Player::is_creative)
        {
            return false;
        }
        if !self.can_start(mob) {
            return false;
        }
        let e = p.get_entity();
        let tick = e.tick_count.load(Ordering::Relaxed);
        if tick > self.cat_search_tick {
            self.cat_search_tick = tick + 20;
            let area = e.bounding_box.load().expand(16.0, 16.0, 16.0);
            let mut found = false;
            for entity in e.world.load().get_entities_at_box(&area) {
                if entity.get_entity().is_alive()
                    && let Some(cat) = entity.cast_any().downcast_ref::<CatEntity>()
                {
                    cat.hiss();
                    found = true;
                }
            }
            self.scared_of_cat = found;
        }
        !self.scared_of_cat
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
        let te = target.get_entity();
        let pos = te.pos.load();
        let bb = te.bounding_box.load();
        // Entity.getY(0.5)
        p.set_move_target(Vector3::new(
            pos.x,
            pos.y + (bb.max.y - bb.min.y) * 0.5,
            pos.z,
        ));
        let e = p.get_entity();
        if e.bounding_box.load().expand(0.2, 0.2, 0.2).intersects(&bb) {
            p.mob_entity.try_attack(p.as_ref(), target.as_ref());
            p.set_phase(AttackPhase::Circle);
            if !e.is_silent() {
                e.world.load().sync_world_event(
                    WorldEvent::SoundPhantomBite,
                    e.block_pos.load(),
                    0,
                );
            }
        } else if e.horizontal_collision.load(Ordering::Relaxed)
            // LivingEntity.wasHurtRecently: hurtTime > 0
            || p.mob_entity.living_entity.hurt_cooldown.load(Ordering::Relaxed) > 10
        {
            p.set_phase(AttackPhase::Circle);
        }
    }
    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// phantom: PhantomCircleAroundAnchorGoal
struct CircleGoal {
    phantom: Weak<PhantomEntity>,
    angle: f32,
    distance: f32,
    height: f32,
    clockwise: f32,
}
impl CircleGoal {
    const fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            angle: 0.0,
            distance: 5.0,
            height: 0.0,
            clockwise: 1.0,
        }
    }

    fn select_next(&mut self, p: &PhantomEntity) {
        let anchor = p.anchor().unwrap_or_else(|| {
            let here = p.get_entity().block_pos.load();
            p.set_anchor(Some(here));
            here
        });
        self.angle += self.clockwise * 15.0f32.to_radians();
        p.set_move_target(Vector3::new(
            f64::from(anchor.0.x) + f64::from(self.distance * self.angle.cos()),
            f64::from(anchor.0.y) + f64::from(-4.0 + self.height),
            f64::from(anchor.0.z) + f64::from(self.distance * self.angle.sin()),
        ));
    }
}
impl Goal for CircleGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom.upgrade().is_some_and(|p| {
            p.mob_entity.get_target().is_none() || p.attack_phase() == AttackPhase::Circle
        })
    }
    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }
    fn start(&mut self, _mob: &dyn Mob) {
        self.distance = 5.0 + rand::random::<f32>() * 10.0;
        self.height = -4.0 + rand::random::<f32>() * 9.0;
        self.clockwise = if rand::random() { 1.0 } else { -1.0 };
        if let Some(p) = self.phantom.upgrade() {
            self.select_next(&p);
        }
    }
    fn tick(&mut self, _mob: &dyn Mob) {
        let Some(p) = self.phantom.upgrade() else {
            return;
        };
        if rand::random_range(0..to_goal_ticks(350)) == 0 {
            self.height = -4.0 + rand::random::<f32>() * 9.0;
        }
        if rand::random_range(0..to_goal_ticks(250)) == 0 {
            self.distance += 1.0;
            if self.distance > 15.0 {
                self.distance = 5.0;
                self.clockwise = -self.clockwise;
            }
        }
        if rand::random_range(0..to_goal_ticks(450)) == 0 {
            self.angle = rand::random::<f32>() * 2.0 * std::f32::consts::PI;
            self.select_next(&p);
        }
        if p.touching_target() {
            self.select_next(&p);
        }
        let e = p.get_entity();
        let bp = e.block_pos.load();
        let world = e.world.load();
        let y = e.pos.load().y;
        if p.move_target().y < y && !world.get_block_state(&bp.down()).is_air() {
            self.height = self.height.max(1.0);
            self.select_next(&p);
        }
        if p.move_target().y > y && !world.get_block_state(&bp.up()).is_air() {
            self.height = self.height.min(-1.0);
            self.select_next(&p);
        }
    }
    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// phantom: PhantomAttackPlayerTargetGoal
struct FindTargetGoal {
    phantom: Weak<PhantomEntity>,
    next_scan: i32,
    attack_targeting: TargetPredicate,
}
impl FindTargetGoal {
    fn new(phantom: Weak<PhantomEntity>) -> Self {
        Self {
            phantom,
            next_scan: to_goal_ticks(20),
            attack_targeting: TargetPredicate::create_attackable().set_base_max_distance(64.0),
        }
    }
}
impl Goal for FindTargetGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        if self.next_scan > 0 {
            self.next_scan -= 1;
            return false;
        }
        self.next_scan = to_goal_ticks(60);
        let Some(p) = self.phantom.upgrade() else {
            return false;
        };
        let e = p.get_entity();
        let world = e.world.load();
        let me = p.as_ref() as &dyn EntityBase;
        let mut players: Vec<_> = world
            .get_players_at_box(&e.bounding_box.load().expand(16.0, 64.0, 16.0))
            .into_iter()
            .filter(|player| {
                self.attack_targeting
                    .test(&world, Some(me), player.as_ref())
            })
            .collect();
        players.sort_by(|a, b| {
            b.get_entity()
                .pos
                .load()
                .y
                .total_cmp(&a.get_entity().pos.load().y)
        });
        for player in players {
            if p.can_attack(player.as_ref()) {
                p.mob_entity.set_target(Some(player));
                return true;
            }
        }
        false
    }
    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.phantom.upgrade().is_some_and(|p| {
            p.mob_entity
                .get_target()
                .is_some_and(|t| p.can_attack(t.as_ref()))
        })
    }
}
