//! Warden, ported from `net.minecraft.world.entity.monster.warden.Warden`.

use std::sync::{
    Arc, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicBool, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::tag::Taggable;
use pumpkin_data::{
    damage::DamageType,
    effect::StatusEffect,
    entity::{EntityPose, EntityStatus, EntityType},
    potion::Effect,
    sound::Sound,
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::{GameMode, math::vector3::Vector3};

use crate::entity::ai::brain::{
    Brain, BrainState,
    memory::{
        PackedMemories,
        position_tracker::{BlockPosTracker, PositionTracker},
        types,
    },
};
use crate::entity::passive::allay::vibration::VibrationData;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
};
use crate::world::{World, game_event::GameEventListener};

mod ai;
mod anger;
mod vibration;

pub use ai::EMERGE_DURATION;
use anger::{AngerLevel, AngerManagement};

const DARKNESS_DISPLAY_LIMIT: i32 = 200;
const DARKNESS_DURATION: i32 = 260;
const DARKNESS_RADIUS: f64 = 20.0;
const DARKNESS_INTERVAL: i32 = 120;
const ANGERMANAGEMENT_TICK_DELAY: i32 = 20;
const DEFAULT_ANGER: i32 = 35;
const PROJECTILE_ANGER: i32 = 10;
const ON_HURT_ANGER_BOOST: i32 = 20;
const RECENT_PROJECTILE_TICK_THRESHOLD: i64 = 100;
const TOUCH_COOLDOWN_TICKS: i64 = 20;
const PROJECTILE_ANGER_DISTANCE: f64 = 30.0;
const VIBRATION_COOLDOWN_TICKS: i64 = 40;
const DISTURBANCE_LOCATION_EXPIRY_TIME: i64 = 100;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Vanilla `Entity.closerThan(entity, horizontal, vertical)`.
#[must_use]
pub(crate) fn closer_than(a: &Entity, b: &Entity, horizontal: f64, vertical: f64) -> bool {
    let a = a.pos.load();
    let b = b.pos.load();
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    dx * dx + dz * dz < horizontal * horizontal && (a.y - b.y).abs() < vertical
}

pub struct WardenEntity {
    pub mob_entity: MobEntity,
    anger: Mutex<AngerManagement>,
    vibration: Mutex<VibrationData>,
    sonic_cooldown_pending: AtomicBool,
}

impl WardenEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        mob_entity
            .navigator
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_can_float(true);
        Arc::new(Self {
            mob_entity,
            anger: Mutex::new(AngerManagement::default()),
            vibration: Mutex::new(VibrationData::default()),
            sonic_cooldown_pending: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn is_digging_or_emerging(&self) -> bool {
        matches!(
            self.get_entity().pose.load(),
            EntityPose::Digging | EntityPose::Emerging
        )
    }

    /// Vanilla `Warden.canTargetEntity`.
    #[must_use]
    pub fn can_target_entity(&self, entity: &dyn EntityBase) -> bool {
        let base = entity.get_entity();
        entity.get_living_entity().is_some_and(|living| {
            base.world.load().uuid == self.get_entity().world.load().uuid
                && !entity.is_spectator()
                && !entity.get_player().is_some_and(|player| {
                    matches!(
                        player.gamemode.load(),
                        GameMode::Creative | GameMode::Spectator
                    )
                })
                && !self.considers_entity_as_ally(entity)
                && base.entity_type != &EntityType::ARMOR_STAND
                && base.entity_type != &EntityType::WARDEN
                && !base.invulnerable.load(Ordering::Relaxed)
                && !living.is_dead_or_dying()
        })
    }

    /// Vanilla `emerge` of a warden spawned by a shrieker (`EntitySpawnReason.TRIGGERED`).
    pub fn emerge(&self) {
        let entity = self.get_entity();
        entity.set_pose(EntityPose::Emerging);
        self.mob_entity.with_brain(self, |brain| {
            brain.set_with_expiry(types::IS_EMERGING, (), i64::from(ai::EMERGE_DURATION));
        });
        self.play_entity_sound(Sound::EntityWardenAgitated, 5.0, 1.0);
    }

    /// Vanilla `applyDarknessAround`.
    pub fn apply_darkness_around(world: &World, position: Vector3<f64>, radius: f64) {
        for player in world.get_nearby_players(position, radius) {
            if !matches!(
                player.gamemode.load(),
                GameMode::Survival | GameMode::Adventure
            ) {
                continue;
            }
            let current = player.living_entity.get_effect(&StatusEffect::DARKNESS);
            if current.is_some_and(|effect| {
                effect.duration < 0 || effect.duration >= DARKNESS_DISPLAY_LIMIT
            }) {
                continue;
            }
            let darkness = Effect {
                effect_type: &StatusEffect::DARKNESS,
                duration: DARKNESS_DURATION,
                amplifier: 0,
                ambient: false,
                show_particles: false,
                show_icon: false,
                blend: true,
            };
            player.send_effect(&darkness);
            player.living_entity.add_effect(darkness);
        }
    }

    // ── anger ────────────────────────────────────────────────────────────────

    fn active_anger(&self) -> i32 {
        let target = self.mob_entity.get_target();
        lock(&self.anger).active_anger(target.as_deref())
    }

    #[must_use]
    pub fn anger_level(&self) -> AngerLevel {
        AngerLevel::by_anger(self.active_anger())
    }

    /// Vanilla `getEntityAngryAt`.
    #[must_use]
    pub fn entity_angry_at(&self) -> Option<Arc<dyn EntityBase>> {
        if self.anger_level().is_angry() {
            lock(&self.anger).active_entity(|entity| self.can_target_entity(entity))
        } else {
            None
        }
    }

    pub fn clear_anger(&self, entity: &dyn EntityBase) {
        lock(&self.anger).clear_anger(entity);
    }

    fn sync_client_anger_level(&self) {
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::warden::CLIENT_ANGER_LEVEL,
            self.active_anger(),
        );
    }

    fn play_listening_sound(&self) {
        if self.get_entity().pose.load() != EntityPose::Roaring {
            self.play_entity_sound(self.anger_level().listening_sound(), 10.0, 1.0);
        }
    }

    /// Vanilla `increaseAngerAt`, `state` is the warden's own brain.
    pub fn increase_anger_at_state(
        &self,
        state: &mut BrainState,
        entity: Option<&Arc<dyn EntityBase>>,
        amount: i32,
        play_sound: bool,
    ) {
        let Some(entity) = entity else {
            return;
        };
        if self.mob_entity.is_no_ai() || !self.can_target_entity(entity.as_ref()) {
            return;
        }
        self.set_dig_cooldown_state(state);
        let maybe_switch_target = state
            .get(types::ATTACK_TARGET)
            .is_none_or(|target| target.get_player().is_none());
        let new_anger = lock(&self.anger).increase_anger(entity, amount);
        if entity.get_player().is_some()
            && maybe_switch_target
            && AngerLevel::by_anger(new_anger).is_angry()
        {
            state.erase(types::ATTACK_TARGET.id());
        }
        if play_sound {
            self.play_listening_sound();
        }
    }

    pub fn increase_anger_at(
        &self,
        entity: Option<&Arc<dyn EntityBase>>,
        amount: i32,
        play_sound: bool,
    ) {
        self.mob_entity.with_brain(self, |brain| {
            self.increase_anger_at_state(brain, entity, amount, play_sound);
        });
    }

    /// Vanilla `setAttackTarget`.
    pub fn set_attack_target_state(&self, state: &mut BrainState, target: Arc<dyn EntityBase>) {
        state.erase(types::ROAR_TARGET.id());
        state.set(types::ATTACK_TARGET, target);
        state.erase(types::CANT_REACH_WALK_TARGET_SINCE.id());
        state.set_with_expiry(types::SONIC_BOOM_COOLDOWN, (), 200);
    }

    /// Vanilla `WardenAi.setDigCooldown`.
    pub fn set_dig_cooldown_state(&self, state: &mut BrainState) {
        if state.has_memory_value(types::DIG_COOLDOWN.id()) {
            state.set_with_expiry(types::DIG_COOLDOWN, (), ai::DIGGING_COOLDOWN);
        }
    }

    /// Vanilla `WardenAi.setDisturbanceLocation`.
    pub fn set_disturbance_location_state(&self, state: &mut BrainState, pos: BlockPos) {
        if self.entity_angry_at().is_none() && !state.has_memory_value(types::ATTACK_TARGET.id()) {
            self.set_dig_cooldown_state(state);
            state.set_with_expiry(types::SNIFF_COOLDOWN, (), 100);
            state.set_with_expiry(
                types::LOOK_TARGET,
                Arc::new(BlockPosTracker::new(pos)) as Arc<dyn PositionTracker>,
                DISTURBANCE_LOCATION_EXPIRY_TIME,
            );
            state.set_with_expiry(
                types::DISTURBANCE_LOCATION,
                pos,
                DISTURBANCE_LOCATION_EXPIRY_TIME,
            );
            state.erase(types::WALK_TARGET.id());
        }
    }

    /// Vanilla `VibrationUser.onReceiveVibration`.
    fn on_receive_vibration(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        source: Option<&Arc<dyn EntityBase>>,
        projectile_owner: Option<&Arc<dyn EntityBase>>,
        _receiving_distance: f32,
    ) {
        if self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }
        let entity = self.get_entity();
        world.send_entity_status(entity, EntityStatus::TendrilsShiver, ActorEventID::None);
        self.play_entity_sound(Sound::EntityWardenTendrilClicks, 5.0, 1.0);
        self.mob_entity.with_brain(self, |brain| {
            brain.set_with_expiry(types::VIBRATION_COOLDOWN, (), VIBRATION_COOLDOWN_TICKS);
            let mut suspicious_pos = pos;
            if let Some(owner) = projectile_owner {
                if closer_than_sq(entity, owner.get_entity(), PROJECTILE_ANGER_DISTANCE) {
                    if brain.has_memory_value(types::RECENT_PROJECTILE.id()) {
                        if self.can_target_entity(owner.as_ref()) {
                            suspicious_pos = owner.get_entity().block_pos.load();
                        }
                        self.increase_anger_at_state(brain, Some(owner), DEFAULT_ANGER, true);
                    } else {
                        self.increase_anger_at_state(brain, Some(owner), PROJECTILE_ANGER, true);
                    }
                }
                brain.set_with_expiry(
                    types::RECENT_PROJECTILE,
                    (),
                    RECENT_PROJECTILE_TICK_THRESHOLD,
                );
            } else {
                self.increase_anger_at_state(brain, source, DEFAULT_ANGER, true);
            }
            if !self.anger_level().is_angry() {
                let active = lock(&self.anger).active_entity(|e| self.can_target_entity(e));
                let same = |a: &Arc<dyn EntityBase>, b: Option<&Arc<dyn EntityBase>>| {
                    b.is_some_and(|b| a.get_entity().entity_uuid == b.get_entity().entity_uuid)
                };
                if projectile_owner.is_some()
                    || active.is_none()
                    || active.as_ref().is_some_and(|active| same(active, source))
                {
                    self.set_disturbance_location_state(brain, suspicious_pos);
                }
            }
        });
    }
}

fn closer_than_sq(a: &Entity, b: &Entity, distance: f64) -> bool {
    a.pos.load().squared_distance_to_vec(&b.pos.load()) < distance * distance
}

impl Mob for WardenEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityWardenStep, 10.0, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        ai::make_brain(self, packed)
    }

    fn requires_custom_persistence(&self) -> bool {
        true
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        false
    }

    fn get_walk_target_value(&self, _pos: &BlockPos) -> f32 {
        0.0
    }

    fn mob_is_pushable(&self) -> bool {
        !self.is_digging_or_emerging()
    }

    fn for_each_game_event_listener(&self, visit: &mut dyn FnMut(&dyn GameEventListener)) {
        visit(&vibration::WardenVibrationListener(self));
    }

    fn check_spawn_obstruction(&self, world: &World) -> bool {
        let entity = &self.mob_entity.living_entity.entity;
        let bounding_box = entity.bounding_box.load();
        !world.contains_any_liquid(bounding_box)
            && world.get_entities_at_box(&bounding_box).is_empty()
            && world.is_space_empty(bounding_box)
    }

    // warden: dig cooldown from the start (Warden.finalizeSpawn)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<super::spawn::SpawnGroupData>,
    ) -> Option<super::spawn::SpawnGroupData> {
        self.mob_entity.with_brain(self, |brain| {
            brain.set_with_expiry(types::DIG_COOLDOWN, (), ai::DIGGING_COOLDOWN);
        });
        self.finalize_spawn_super(group_data)
    }

    // warden: Warden.customServerAiStep
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let tick_count = entity.tick_count.load(Ordering::Relaxed);
        if self.sonic_cooldown_pending.swap(false, Ordering::Relaxed) {
            self.mob_entity.with_brain(self, |brain| {
                brain.set_with_expiry(types::SONIC_BOOM_COOLDOWN, (), ai::SONIC_BOOM_COOLDOWN);
            });
        }
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, |brain| {
            self.mob_entity
                .set_target(brain.get(types::ATTACK_TARGET).cloned());
        });
        if (tick_count + entity.entity_id) % DARKNESS_INTERVAL == 0 {
            Self::apply_darkness_around(&world, entity.pos.load(), DARKNESS_RADIUS);
        }
        if tick_count % ANGERMANAGEMENT_TICK_DELAY == 0 {
            lock(&self.anger).tick(&world, |entity| self.can_target_entity(entity));
            self.sync_client_anger_level();
        }
        self.mob_entity.with_brain(self, ai::update_activity);
    }

    fn post_tick(&self) {
        self.tick_vibration();
        if self.mob_entity.persistence_required.load(Ordering::Relaxed) {
            self.mob_entity
                .with_brain(self, |brain| self.set_dig_cooldown_state(brain));
        }
    }

    // warden: invulnerable while digging, anger from every hit (Warden.hurtServer)
    fn pre_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        if self.is_digging_or_emerging() {
            return damage_type
                .has_tag(&pumpkin_data::tag::DamageType::MINECRAFT_BYPASSES_INVULNERABILITY);
        }
        if self.mob_entity.is_no_ai() {
            return true;
        }
        let world = self.get_entity().world.load_full();
        let attacker = cause
            .or(source)
            .and_then(|attacker| world.get_entity_by_id(attacker.get_entity().entity_id));
        let direct = source.is_none_or(|source| {
            cause.is_none_or(|cause| source.get_entity().entity_id == cause.get_entity().entity_id)
        });
        self.mob_entity.with_brain(self, |brain| {
            self.increase_anger_at_state(
                brain,
                attacker.as_ref(),
                AngerLevel::Angry.minimum_anger() + ON_HURT_ANGER_BOOST,
                false,
            );
            if !brain.has_memory_value(types::ATTACK_TARGET.id())
                && let Some(attacker) = attacker
                && attacker.get_living_entity().is_some()
                && (direct || closer_than_sq(self.get_entity(), attacker.get_entity(), 5.0))
            {
                self.set_attack_target_state(brain, attacker);
            }
        });
        true
    }

    // warden: Warden.doHurtTarget
    fn pre_attack(&self, _target: &dyn EntityBase) {
        let entity = self.get_entity();
        entity.world.load().send_entity_status(
            entity,
            EntityStatus::StartAttacking,
            ActorEventID::None,
        );
        self.play_entity_sound(Sound::EntityWardenAttackImpact, 10.0, 1.0);
        self.sonic_cooldown_pending.store(true, Ordering::Relaxed);
    }

    // warden: Warden.doPush
    fn on_push_entity(&self, other: &Arc<dyn EntityBase>) {
        if self.mob_entity.is_no_ai() {
            return;
        }
        let touched = self.mob_entity.with_brain(self, |brain| {
            if brain.has_memory_value(types::TOUCH_COOLDOWN.id()) {
                return false;
            }
            brain.set_with_expiry(types::TOUCH_COOLDOWN, (), TOUCH_COOLDOWN_TICKS);
            self.increase_anger_at_state(brain, Some(other), DEFAULT_ANGER, true);
            self.set_disturbance_location_state(brain, other.get_entity().block_pos.load());
            true
        });
        let _ = touched;
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_compound("anger", lock(&self.anger).write_nbt());
        nbt.put_compound("listener", lock(&self.vibration).write_nbt());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        *lock(&self.anger) = nbt
            .get_compound("anger")
            .map(AngerManagement::read_nbt)
            .unwrap_or_default();
        self.sync_client_anger_level();
        *lock(&self.vibration) = nbt
            .get_compound("listener")
            .map(VibrationData::read_nbt)
            .unwrap_or_default();
    }
}
