use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityStatus;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::creaking_ai;
use crate::block::entities::creaking_heart::CreakingHeartBlockEntity;
use crate::entity::ai::brain::Brain;
use crate::entity::ai::brain::memory::{PackedMemories, types};
use crate::entity::ai::control::look_control::LookControl;
use crate::entity::ai::control::move_control::MoveControl;
use crate::entity::ai::control::{Control, LookControlTrait, MoveControlTrait};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::custom_sound::CustomSound;
use crate::entity::projectile::is_projectile;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
    player::Player,
};
use crate::plugin::api::events::entity::entity_target::TargetReason;

pub const ATTACK_ANIMATION_DURATION: i32 = 15;
pub const ACTIVATION_RANGE_SQ: f64 = 144.0;
pub const INVULNERABILITY_ANIMATION_DURATION: i32 = 8;
pub const TWITCH_DEATH_DURATION: i32 = 45;
pub const MAX_PLAYER_STUCK_COUNTER: i32 = 4;
const GAZE_CONE_SIZE: f64 = 0.5;
const MAX_GAZE_DISTANCE: f64 = 128.0;
const TEARDOWN_WOOD_PARTICLES: i32 = 100;
const TEARDOWN_SPREAD: f32 = 0.3;

pub struct CreakingEntity {
    pub mob_entity: MobEntity,
    can_move: AtomicBool,
    is_active: AtomicBool,
    is_tearing_down: AtomicBool,
    home_pos: AtomicCell<Option<BlockPos>>,
    attack_animation_remaining_ticks: AtomicI32,
    invulnerability_animation_remaining_ticks: AtomicI32,
    eyes_glowing: AtomicBool,
    next_flicker_time: AtomicI32,
    player_stuck_counter: AtomicI32,
    death_time: AtomicI32,
}

impl CreakingEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        // creaking: CreakingMoveControl, CreakingLookControl freeze while watched
        *mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(CreakingMoveControl::default());
        *mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(CreakingLookControl::default());
        mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_float(true);
        Arc::new(Self {
            mob_entity,
            can_move: AtomicBool::new(true),
            is_active: AtomicBool::new(false),
            is_tearing_down: AtomicBool::new(false),
            home_pos: AtomicCell::new(None),
            attack_animation_remaining_ticks: AtomicI32::new(0),
            invulnerability_animation_remaining_ticks: AtomicI32::new(0),
            eyes_glowing: AtomicBool::new(false),
            next_flicker_time: AtomicI32::new(0),
            player_stuck_counter: AtomicI32::new(0),
            death_time: AtomicI32::new(0),
        })
    }

    // creaking: setTransient, bound to a heart with its own pathfinding maluses
    pub fn set_transient(&self, pos: BlockPos) {
        self.set_home_pos(Some(pos));
        let mut navigator = self
            .mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator.set_pathfinding_malus(PathType::DamageOther, 8.0);
        navigator.set_pathfinding_malus(PathType::PowderSnow, 8.0);
        navigator.set_pathfinding_malus(PathType::Lava, 8.0);
        navigator.set_pathfinding_malus(PathType::DamageFire, 0.0);
        navigator.set_pathfinding_malus(PathType::DangerFire, 0.0);
    }

    pub fn is_heart_bound(&self) -> bool {
        self.get_home_pos().is_some()
    }

    pub fn can_move(&self) -> bool {
        self.can_move.load(Ordering::Relaxed)
    }

    pub fn set_can_move(&self, can_move: bool) {
        self.can_move.store(can_move, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::creaking::CAN_MOVE, can_move);
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::Relaxed)
    }

    pub fn set_is_active(&self, active: bool) {
        self.is_active.store(active, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::creaking::IS_ACTIVE, active);
    }

    pub fn is_tearing_down(&self) -> bool {
        self.is_tearing_down.load(Ordering::Relaxed)
    }

    pub fn set_tearing_down(&self) {
        self.is_tearing_down.store(true, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::creaking::IS_TEARING_DOWN, true);
    }

    pub fn get_home_pos(&self) -> Option<BlockPos> {
        self.home_pos.load()
    }

    pub fn set_home_pos(&self, pos: Option<BlockPos>) {
        self.home_pos.store(pos);
        let entity = &self.mob_entity.living_entity.entity;
        // creaking: fireImmune and canUsePortal while heart bound
        entity.fire_immune.store(pos.is_some(), Ordering::Relaxed);
        entity
            .cannot_use_portal
            .store(pos.is_some(), Ordering::Relaxed);
        entity.set_synced_data(pumpkin_data::tracked_data::creaking::HOME_POS, pos);
    }

    pub fn stop_in_place(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        entity.velocity.store(Vector3::default());
        self.mob_entity.steered().navigator().stop();
    }

    // creaking: activate
    pub fn activate(&self, player: &Arc<Player>) {
        self.mob_entity.with_brain(self, |brain| {
            brain.set(types::ATTACK_TARGET, player.clone() as Arc<dyn EntityBase>);
        });
        self.emit_action_event();
        self.play_sound(Sound::EntityCreakingActivate);
        self.set_is_active(true);
    }

    // creaking: deactivate
    pub fn deactivate(&self) {
        self.mob_entity
            .with_brain(self, |brain| brain.erase(types::ATTACK_TARGET.id()));
        self.emit_action_event();
        self.play_sound(Sound::EntityCreakingDeactivate);
        self.set_is_active(false);
    }

    fn emit_action_event(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        entity
            .world
            .load()
            .emit_game_event(GameEvent::EntityAction.name(), entity.pos.load());
    }

    pub fn has_glowing_eyes(&self) -> bool {
        self.eyes_glowing.load(Ordering::Relaxed)
    }

    pub fn check_eye_blink(&self) {
        use rand::RngExt;
        let death_time = self.death_time.load(Ordering::Relaxed);
        if death_time > self.next_flicker_time.load(Ordering::Relaxed) {
            let glowing = self.eyes_glowing.load(Ordering::Relaxed);
            let (min, max) = if glowing {
                (2, 8)
            } else {
                (death_time / 4, death_time / 2)
            };
            let added = self.get_random().random_range(min..=max.max(min));
            self.next_flicker_time
                .store(death_time + added, Ordering::Relaxed);
            self.eyes_glowing.store(!glowing, Ordering::Relaxed);
        }
    }

    fn nearest_players(&self) -> Vec<Arc<Player>> {
        self.mob_entity.with_brain(self, |brain| {
            brain
                .get(types::NEAREST_PLAYERS)
                .cloned()
                .unwrap_or_default()
        })
    }

    // creaking: playerIsStuckInYou
    pub fn player_is_stuck_in_you(&self) -> bool {
        let players = self.nearest_players();
        if players.is_empty() {
            self.player_stuck_counter.store(0, Ordering::Relaxed);
            return false;
        }
        let own_box = self.mob_entity.living_entity.entity.bounding_box.load();
        for player in &players {
            let eyes = player.get_entity().get_eye_pos();
            if eyes.x >= own_box.min.x
                && eyes.x < own_box.max.x
                && eyes.y >= own_box.min.y
                && eyes.y < own_box.max.y
                && eyes.z >= own_box.min.z
                && eyes.z < own_box.max.z
            {
                let count = self.player_stuck_counter.fetch_add(1, Ordering::Relaxed) + 1;
                return count > MAX_PLAYER_STUCK_COUNTER;
            }
        }
        self.player_stuck_counter.store(0, Ordering::Relaxed);
        false
    }

    // living: isLookingAtMe(player, 0.5, false, true, eye, y + 0.5 * scale, mid)
    pub fn is_looking_at_me(&self, player: &Player) -> bool {
        let entity = &self.mob_entity.living_entity.entity;
        let pos = entity.pos.load();
        let eye_y = entity.get_eye_y();
        let player_entity = player.get_entity();
        let player_eyes = player_entity.get_eye_pos();
        let look = Vector3::rotation_vector(
            f64::from(player_entity.pitch.load()),
            f64::from(player_entity.yaw.load()),
        )
        .normalize();
        let scale = self
            .mob_entity
            .living_entity
            .get_attribute_value(&Attributes::SCALE);
        let world = entity.world.load();
        [
            eye_y,
            scale.mul_add(0.5, pos.y),
            f64::midpoint(eye_y, pos.y),
        ]
        .into_iter()
        .any(|gaze_height| {
            let target = Vector3::new(pos.x, gaze_height, pos.z);
            let dir = (target - player_eyes).normalize();
            look.dot(&dir) > 1.0 - GAZE_CONE_SIZE
                && (target - player_eyes).length() <= MAX_GAZE_DISTANCE
                && world
                    .clip(&pumpkin_world::clip::ClipContext::new(
                        player_eyes,
                        target,
                        pumpkin_world::clip::ClipBlock::Visual,
                        pumpkin_world::clip::ClipFluid::None,
                    ))
                    .is_miss
        })
    }

    // living: PLAYER_NOT_WEARING_DISGUISE_ITEM
    fn is_player_disguised(player: &Player) -> bool {
        player
            .living_entity
            .entity_equipment
            .try_lock()
            .is_ok_and(|equipment| {
                equipment
                    .equipment
                    .get(&EquipmentSlot::HEAD)
                    .is_some_and(|helmet| {
                        !helmet.is_empty()
                            && helmet
                                .item
                                .has_tag(&tag::Item::MINECRAFT_GAZE_DISGUISE_EQUIPMENT)
                    })
            })
    }

    // creaking: checkCanMove
    pub fn check_can_move(&self) -> bool {
        let players = self.nearest_players();
        let active = self.is_active();
        if players.is_empty() {
            if active {
                self.deactivate();
            }
            return true;
        }
        let pos = self.mob_entity.living_entity.entity.pos.load();
        let mut has_potential_target = false;
        for player in players {
            if self.can_attack(player.as_ref()) {
                has_potential_target = true;
                if (!active || !Self::is_player_disguised(&player))
                    && self.is_looking_at_me(&player)
                {
                    if active {
                        return false;
                    }
                    if pos.squared_distance_to_vec(&player.get_entity().pos.load())
                        < ACTIVATION_RANGE_SQ
                    {
                        self.activate(&player);
                        return false;
                    }
                }
            }
        }
        if !has_potential_target && active {
            self.deactivate();
        }
        true
    }

    // creaking: tearDown
    pub fn tear_down(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load();
        let bounds = entity.bounding_box.load();
        let center = Vector3::new(
            f64::midpoint(bounds.min.x, bounds.max.x),
            f64::midpoint(bounds.min.y, bounds.max.y),
            f64::midpoint(bounds.min.z, bounds.max.z),
        );
        world.spawn_particle(
            center,
            Vector3::new(TEARDOWN_SPREAD, TEARDOWN_SPREAD, TEARDOWN_SPREAD),
            0.0,
            TEARDOWN_WOOD_PARTICLES,
            Particle::BlockCrumble,
        );
        self.play_sound(Sound::EntityCreakingDeath);
        entity.remove();
    }

    // creaking: creakingDeathEffects, called by the heart when it takes the creaking down
    pub fn creaking_death_effects(&self) {
        self.set_tearing_down();
        self.play_sound(Sound::EntityCreakingTwitch);
    }

    fn play_sound(&self, sound: Sound) {
        let entity = &self.mob_entity.living_entity.entity;
        entity.world.load().play_sound_fine(
            sound,
            SoundCategory::Hostile,
            &entity.pos.load(),
            1.0,
            1.0,
        );
    }

    fn has_heart_protection(&self, home_pos: &BlockPos) -> bool {
        let entity = &self.mob_entity.living_entity.entity;
        entity
            .world
            .load()
            .get_block_entity(home_pos)
            .is_some_and(|block_entity| {
                block_entity
                    .as_any()
                    .downcast_ref::<CreakingHeartBlockEntity>()
                    .is_some_and(|heart| heart.is_protector(entity.entity_uuid))
            })
    }

    // creaking: getTargetFromBrain
    fn sync_target_from_brain(&self) {
        let target = self
            .mob_entity
            .with_brain(self, |brain| brain.get(types::ATTACK_TARGET).cloned());
        self.apply_mob_target(target);
    }
}

// creaking: CreakingMoveControl
#[derive(Default)]
struct CreakingMoveControl {
    inner: MoveControl,
}

impl Control for CreakingMoveControl {}

impl MoveControlTrait for CreakingMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if mob
            .cast_any()
            .downcast_ref::<CreakingEntity>()
            .is_none_or(CreakingEntity::can_move)
        {
            self.inner.tick(mob);
        }
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.inner.set_wanted_position(x, y, z, speed_modifier);
    }

    fn strafe(&mut self, forward: f32, right: f32) {
        self.inner.strafe(forward, right);
    }

    fn has_wanted(&self) -> bool {
        self.inner.has_wanted()
    }

    fn speed_modifier(&self) -> f64 {
        self.inner.speed_modifier()
    }
}

// creaking: CreakingLookControl
#[derive(Default)]
struct CreakingLookControl {
    inner: LookControl,
}

impl Control for CreakingLookControl {}

impl LookControlTrait for CreakingLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if mob
            .cast_any()
            .downcast_ref::<CreakingEntity>()
            .is_none_or(CreakingEntity::can_move)
        {
            LookControlTrait::tick(&mut self.inner, mob);
        }
    }

    fn looking_at(&self) -> Option<Vector3<f64>> {
        self.inner.looking_at()
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        max_yaw_change: f32,
        max_pitch_change: f32,
    ) {
        self.inner
            .look_at_with_range(x, y, z, max_yaw_change, max_pitch_change);
    }
}

impl CustomSound for CreakingEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityCreakingDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        self.is_heart_bound().then_some(Sound::EntityCreakingSway)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl Mob for CreakingEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityCreakingStep, 0.15, 1.0);
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    // creaking: no ambient sound while active
    fn get_ambient_sound(&self) -> Option<Sound> {
        (!self.is_active()).then_some(Sound::EntityCreakingAmbient)
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        creaking_ai::make_brain(self, packed)
    }

    // creaking: customServerAiStep (creakingBrain, CreakingAi.updateActivity)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        let can_move = self.can_move();
        self.mob_entity
            .with_brain(self, |brain| creaking_ai::update_activity(brain, can_move));
        self.sync_target_from_brain();
    }

    // creaking: getTarget reads the brain
    fn set_mob_target(&self, _target: Option<Arc<dyn EntityBase>>, _reason: TargetReason) -> bool {
        true
    }

    // creaking: CreakingPathNavigation.tick only moves while it can
    fn ticks_navigation(&self) -> bool {
        self.can_move()
    }

    // creaking: CreakingJumpControl
    fn jump_control_tick(&self, _jump: bool) -> bool {
        if self.can_move() {
            return false;
        }
        self.mob_entity
            .living_entity
            .jumping
            .store(false, Ordering::SeqCst);
        true
    }

    // creaking: isPushable only while it can move
    fn mob_is_pushable(&self) -> bool {
        self.can_move()
    }

    // creaking: knockback only while it can move
    fn knockback_override(
        &self,
        _power: f64,
        _dx: f64,
        _dz: f64,
        _damage: f32,
        _attacker: Option<&dyn EntityBase>,
    ) -> bool {
        !self.can_move()
    }

    // creaking: getWalkTargetValue
    fn get_walk_target_value(&self, _pos: &BlockPos) -> f32 {
        0.0
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        if let Some(pos) = self.get_home_pos() {
            let mut sub = NbtCompound::new();
            sub.put_int("x", pos.0.x);
            sub.put_int("y", pos.0.y);
            sub.put_int("z", pos.0.z);
            nbt.put_compound("home_pos", sub);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(sub) = nbt.get_compound("home_pos")
            && let (Some(x), Some(y), Some(z)) =
                (sub.get_int("x"), sub.get_int("y"), sub.get_int("z"))
        {
            self.set_transient(BlockPos::new(x, y, z));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // creaking: aiStep counters and canMove, tick() heart protection, tickDeath teardown
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        if self.is_heart_bound() && self.is_tearing_down() {
            let death_time = self.death_time.fetch_add(1, Ordering::Relaxed) + 1;
            if death_time > TWITCH_DEATH_DURATION && !entity.is_removed() {
                self.tear_down();
            }
            return;
        }
        if !entity.is_alive() {
            return;
        }
        for counter in [
            &self.invulnerability_animation_remaining_ticks,
            &self.attack_animation_remaining_ticks,
        ] {
            if counter.load(Ordering::Relaxed) > 0 {
                counter.fetch_sub(1, Ordering::Relaxed);
            }
        }

        let can_move = self.can_move();
        let now_can_move = self.check_can_move();
        if now_can_move != can_move {
            self.emit_action_event();
            if now_can_move {
                self.play_sound(Sound::EntityCreakingUnfreeze);
            } else {
                self.stop_in_place();
                self.play_sound(Sound::EntityCreakingFreeze);
            }
        }
        self.set_can_move(now_can_move);

        if let Some(home_pos) = self.get_home_pos()
            && !self.has_heart_protection(&home_pos)
        {
            self.mob_entity.living_entity.set_health(0.0);
        }
    }

    // creaking: doHurtTarget, attack animation and sound for the clients
    fn pre_attack(&self, _target: &dyn EntityBase) {
        self.attack_animation_remaining_ticks
            .store(ATTACK_ANIMATION_DURATION, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.world.load().send_entity_status(
            entity,
            EntityStatus::StartAttacking,
            ActorEventID::None,
        );
    }

    // creaking: hurtServer, a heart bound creaking only reacts and never takes damage
    fn damage_override(
        &self,
        _amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> Option<bool> {
        let home_pos = self.get_home_pos()?;
        if damage_type.has_tag(&tag::DamageType::MINECRAFT_BYPASSES_INVULNERABILITY) {
            return None;
        }
        let entity = self.get_entity();
        if entity.is_invulnerable_to(&damage_type, cause)
            || self
                .invulnerability_animation_remaining_ticks
                .load(Ordering::Relaxed)
                > 0
            || self.mob_entity.living_entity.is_dead_or_dying()
        {
            return Some(false);
        }
        let responsible_player = cause.or(source).and_then(EntityBase::get_player);
        let direct_is_living_or_projectile = source.is_some_and(|direct| {
            direct.get_living_entity().is_some() || is_projectile(direct.get_entity().entity_type)
        });
        if !direct_is_living_or_projectile && responsible_player.is_none() {
            return Some(false);
        }
        self.invulnerability_animation_remaining_ticks
            .store(INVULNERABILITY_ANIMATION_DURATION, Ordering::Relaxed);
        let world = entity.world.load();
        world.send_entity_status(entity, EntityStatus::Shake, ActorEventID::None);
        self.emit_action_event();
        if self.has_heart_protection(&home_pos) {
            if responsible_player.is_some()
                && let Some(heart) = world.get_block_entity(&home_pos)
                && let Some(heart) = heart.as_any().downcast_ref::<CreakingHeartBlockEntity>()
            {
                heart.creaking_hurt(&world);
            }
            self.play_sound(Sound::EntityCreakingSway);
        }
        Some(true)
    }
}
