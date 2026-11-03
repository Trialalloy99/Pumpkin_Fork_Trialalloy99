//! Ported from `WardenAi` and `net.minecraft.world.entity.ai.behavior.warden`.

use std::sync::Arc;

use pumpkin_data::{
    attributes::Attributes,
    damage::DamageType,
    entity::{EntityPose, EntityStatus},
    environment_attribute::Activity,
    particle::Particle,
    sound::Sound,
};
use pumpkin_protocol::{bedrock::server::actor_event::ActorEventID, codec::var_int::VarInt};
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use super::{WardenEntity, closer_than};
use crate::entity::EntityBase;
use crate::entity::RemovalReason;
use crate::entity::ai::brain::{
    Brain, BrainProvider, BrainTick,
    activity::ActivityData,
    behavior::{
        Behavior, DoNothing, LookAtTargetSink, MoveToTargetSink, OneShot, Swim, Timed,
        go_to_target_location, melee_attack, random_stroll, run_one_with_conditions,
        set_walk_target_from_attack_target_if_target_out_of_reach,
        stop_attacking_if_target_invalid, utils,
    },
    memory::{
        MemoryModuleId, MemoryStatus, PackedMemories,
        position_tracker::{BlockPosTracker, PositionTracker},
        types,
    },
    sensing::SensorType,
};
use crate::entity::mob::Mob;

const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.5;
const SPEED_MULTIPLIER_WHEN_INVESTIGATING: f32 = 0.7;
const SPEED_MULTIPLIER_WHEN_FIGHTING: f32 = 1.2;
const MELEE_ATTACK_COOLDOWN: i64 = 18;
pub(super) const DIGGING_DURATION: i32 = 100;
pub const EMERGE_DURATION: i32 = 134;
pub(super) const ROAR_DURATION: i32 = 84;
const SNIFFING_DURATION: i32 = 84;
pub(super) const DIGGING_COOLDOWN: i64 = 1200;
const ROAR_SOUND_DELAY: i64 = 25;
const SONIC_BOOM_TICKS_BEFORE_SOUND: i64 = 34;
const SONIC_BOOM_DURATION: i32 = 60;
pub(super) const SONIC_BOOM_COOLDOWN: i64 = 40;

const MEMORIES: [MemoryModuleId; 20] = [
    types::NEAREST_VISIBLE_NEMESIS.id(),
    types::RECENT_PROJECTILE.id(),
    types::TOUCH_COOLDOWN.id(),
    types::VIBRATION_COOLDOWN.id(),
    types::NEAREST_ATTACKABLE.id(),
    types::ROAR_TARGET.id(),
    types::DISTURBANCE_LOCATION.id(),
    types::IS_SNIFFING.id(),
    types::IS_EMERGING.id(),
    types::ROAR_SOUND_DELAY.id(),
    types::DIG_COOLDOWN.id(),
    types::ROAR_SOUND_COOLDOWN.id(),
    types::SNIFF_COOLDOWN.id(),
    types::SONIC_BOOM_COOLDOWN.id(),
    types::SONIC_BOOM_SOUND_COOLDOWN.id(),
    types::SONIC_BOOM_SOUND_DELAY.id(),
    types::ATTACK_COOLING_DOWN.id(),
    types::CANT_REACH_WALK_TARGET_SINCE.id(),
    types::LOOK_TARGET.id(),
    types::WALK_TARGET.id(),
];

pub(super) fn make_brain(warden: &WardenEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[SensorType::NearestPlayers, SensorType::WardenEntity],
        activities,
    }
    .make_brain(warden, packed)
}

fn warden<'a>(tick: &BrainTick<'a>) -> Option<&'a WardenEntity> {
    tick.mob.cast_any().downcast_ref::<WardenEntity>()
}

pub(super) fn update_activity(brain: &mut Brain) {
    brain.set_active_activity_to_first_valid(&[
        Activity::Emerge,
        Activity::Dig,
        Activity::Roar,
        Activity::Fight,
        Activity::Investigate,
        Activity::Sniff,
        Activity::Idle,
    ]);
}

#[expect(clippy::too_many_lines)]
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(Swim::new(0.8))),
                Box::new(set_warden_look_target()),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
            ],
        ),
        ActivityData::with_gate_memory(
            Activity::Emerge,
            5,
            vec![Box::new(Timed::new(Emerging))],
            types::IS_EMERGING.id(),
        ),
        ActivityData::with_pairs_and_conditions(
            Activity::Dig,
            vec![
                (0, Box::new(Timed::new(ForceUnmount))),
                (1, Box::new(Timed::new(Digging))),
            ],
            vec![
                (types::ROAR_TARGET.id(), MemoryStatus::ValueAbsent),
                (types::DIG_COOLDOWN.id(), MemoryStatus::ValueAbsent),
            ],
        ),
        ActivityData::with_priority_start(
            Activity::Idle,
            10,
            vec![
                Box::new(set_roar_target()),
                Box::new(try_to_sniff()),
                Box::new(run_one_with_conditions(
                    vec![(types::IS_SNIFFING.id(), MemoryStatus::ValueAbsent)],
                    vec![
                        (
                            Box::new(random_stroll::stroll(SPEED_MULTIPLIER_WHEN_IDLING)),
                            2,
                        ),
                        (Box::new(DoNothing::new(30, 60)), 1),
                    ],
                )),
            ],
        ),
        ActivityData::with_gate_memory(
            Activity::Roar,
            10,
            vec![Box::new(Timed::new(Roar))],
            types::ROAR_TARGET.id(),
        ),
        ActivityData::with_gate_memory(
            Activity::Fight,
            10,
            vec![
                Box::new(dig_cooldown_setter()),
                Box::new(stop_attacking_if_target_invalid::create_with(
                    |tick, target| {
                        warden(tick).is_none_or(|warden| {
                            !warden.anger_level().is_angry() || !warden.can_target_entity(target)
                        })
                    },
                    |tick, target| {
                        if let Some(warden) = warden(tick) {
                            if !warden.can_target_entity(target) {
                                warden.clear_anger(target);
                            }
                            warden.set_dig_cooldown_state(tick.state);
                        }
                    },
                    false,
                )),
                Box::new(set_target_look_target()),
                Box::new(
                    set_walk_target_from_attack_target_if_target_out_of_reach::create(
                        SPEED_MULTIPLIER_WHEN_FIGHTING,
                    ),
                ),
                Box::new(Timed::new(SonicBoom)),
                Box::new(melee_attack::create(|_| true, MELEE_ATTACK_COOLDOWN)),
            ],
            types::ATTACK_TARGET.id(),
        ),
        ActivityData::with_gate_memory(
            Activity::Investigate,
            5,
            vec![
                Box::new(set_roar_target()),
                Box::new(go_to_target_location::create(
                    types::DISTURBANCE_LOCATION,
                    2,
                    SPEED_MULTIPLIER_WHEN_INVESTIGATING,
                )),
            ],
            types::DISTURBANCE_LOCATION.id(),
        ),
        ActivityData::with_gate_memory(
            Activity::Sniff,
            5,
            vec![Box::new(set_roar_target()), Box::new(Timed::new(Sniffing))],
            types::IS_SNIFFING.id(),
        ),
    ]
}

// warden: DIG_COOLDOWN_SETTER keeps the cooldown running while it fights
fn dig_cooldown_setter() -> OneShot {
    OneShot::new(
        "DigCooldownSetter",
        vec![(types::DIG_COOLDOWN.id(), MemoryStatus::Registered)],
        |tick| {
            if tick.state.has_memory_value(types::DIG_COOLDOWN.id()) {
                tick.state
                    .set_with_expiry(types::DIG_COOLDOWN, (), DIGGING_COOLDOWN);
            }
            true
        },
    )
}

// warden: SetRoarTarget.create(Warden::getEntityAngryAt)
fn set_roar_target() -> OneShot {
    OneShot::new(
        "SetRoarTarget",
        vec![
            (types::ROAR_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::CANT_REACH_WALK_TARGET_SINCE.id(),
                MemoryStatus::Registered,
            ),
        ],
        |tick| {
            let Some(target) =
                warden(tick)
                    .and_then(WardenEntity::entity_angry_at)
                    .filter(|target| {
                        warden(tick).is_some_and(|warden| warden.can_target_entity(target.as_ref()))
                    })
            else {
                return false;
            };
            tick.state.set(types::ROAR_TARGET, target);
            tick.state.erase(types::CANT_REACH_WALK_TARGET_SINCE.id());
            true
        },
    )
}

// warden: SetWardenLookTarget
fn set_warden_look_target() -> OneShot {
    OneShot::new(
        "SetWardenLookTarget",
        vec![
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::DISTURBANCE_LOCATION.id(), MemoryStatus::Registered),
            (types::ROAR_TARGET.id(), MemoryStatus::Registered),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
        ],
        |tick| {
            let target = tick
                .state
                .get(types::ROAR_TARGET)
                .map(|target| target.get_entity().block_pos.load())
                .or_else(|| tick.state.get(types::DISTURBANCE_LOCATION).copied());
            let Some(target) = target else {
                return false;
            };
            tick.state.set(
                types::LOOK_TARGET,
                Arc::new(BlockPosTracker::new(target)) as Arc<dyn PositionTracker>,
            );
            true
        },
    )
}

// warden: TryToSniff
fn try_to_sniff() -> OneShot {
    OneShot::new(
        "TryToSniff",
        vec![
            (types::IS_SNIFFING.id(), MemoryStatus::Registered),
            (types::WALK_TARGET.id(), MemoryStatus::Registered),
            (types::SNIFF_COOLDOWN.id(), MemoryStatus::ValueAbsent),
            (types::NEAREST_ATTACKABLE.id(), MemoryStatus::ValuePresent),
            (types::DISTURBANCE_LOCATION.id(), MemoryStatus::ValueAbsent),
        ],
        |tick| {
            let cooldown = i64::from(tick.mob.get_random().random_range(100..=200));
            tick.state.set(types::IS_SNIFFING, ());
            tick.state
                .set_with_expiry(types::SNIFF_COOLDOWN, (), cooldown);
            tick.state.erase(types::WALK_TARGET.id());
            tick.mob.get_entity().set_pose(EntityPose::Sniffing);
            true
        },
    )
}

// warden: SetEntityLookTarget.create(isTarget, followRange)
fn set_target_look_target() -> OneShot {
    OneShot::new(
        "SetEntityLookTarget",
        vec![
            (types::LOOK_TARGET.id(), MemoryStatus::ValueAbsent),
            (
                types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
                MemoryStatus::ValuePresent,
            ),
        ],
        |tick| {
            let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() else {
                return false;
            };
            let follow_range = tick
                .mob
                .get_mob_entity()
                .living_entity
                .get_attribute_value(&Attributes::FOLLOW_RANGE);
            let me = tick.mob.get_entity().pos.load();
            let visible = tick
                .state
                .get(types::NEAREST_VISIBLE_LIVING_ENTITIES)
                .is_some_and(|visible| visible.contains(target.as_ref(), &tick.visibility()));
            if !visible
                || target.get_entity().pos.load().squared_distance_to_vec(&me)
                    > follow_range * follow_range
            {
                return false;
            }
            utils::look_at_entity(tick.state, target);
            true
        },
    )
}

// warden: Emerging
struct Emerging;

impl Behavior for Emerging {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        const CONDITIONS: [(MemoryModuleId, MemoryStatus); 3] = [
            (types::IS_EMERGING.id(), MemoryStatus::ValuePresent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
        ];
        &CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        EMERGE_DURATION
    }

    fn max_duration(&self) -> i32 {
        EMERGE_DURATION
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob.get_entity().set_pose(EntityPose::Emerging);
        tick.mob
            .play_entity_sound(Sound::EntityWardenEmerge, 5.0, 1.0);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.pose.load() == EntityPose::Emerging {
            entity.set_pose(EntityPose::Standing);
        }
    }

    fn debug_name(&self) -> &'static str {
        "Emerging"
    }
}

// warden: Digging
struct Digging;

impl Behavior for Digging {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        const CONDITIONS: [(MemoryModuleId, MemoryStatus); 2] = [
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
        ];
        &CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        DIGGING_DURATION
    }

    fn max_duration(&self) -> i32 {
        DIGGING_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        let entity = tick.mob.get_entity();
        entity.on_ground.load(std::sync::atomic::Ordering::Relaxed)
            || entity
                .touching_water
                .load(std::sync::atomic::Ordering::Relaxed)
            || entity
                .touching_lava
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn can_still_use(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.mob.get_entity().removal_reason.load().is_none()
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.on_ground.load(std::sync::atomic::Ordering::Relaxed) {
            entity.set_pose(EntityPose::Digging);
            tick.mob.play_entity_sound(Sound::EntityWardenDig, 5.0, 1.0);
        } else {
            tick.mob
                .play_entity_sound(Sound::EntityWardenAgitated, 5.0, 1.0);
            self.stop(tick);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.removal_reason.load().is_none() {
            entity.world.load().remove_entity(tick.mob);
            let _ = RemovalReason::Discarded;
        }
    }

    fn debug_name(&self) -> &'static str {
        "Digging"
    }
}

// warden: ForceUnmount
struct ForceUnmount;

impl Behavior for ForceUnmount {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        &[]
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if let Some(vehicle) = entity.get_vehicle() {
            vehicle.get_entity().remove_passenger_sync(entity.entity_id);
        }
    }

    fn debug_name(&self) -> &'static str {
        "ForceUnmount"
    }
}

// warden: Roar
struct Roar;

impl Behavior for Roar {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        const CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
            (types::ROAR_TARGET.id(), MemoryStatus::ValuePresent),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::ROAR_SOUND_COOLDOWN.id(), MemoryStatus::Registered),
            (types::ROAR_SOUND_DELAY.id(), MemoryStatus::Registered),
        ];
        &CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        ROAR_DURATION
    }

    fn max_duration(&self) -> i32 {
        ROAR_DURATION
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.state
            .set_with_expiry(types::ROAR_SOUND_DELAY, (), ROAR_SOUND_DELAY);
        tick.state.erase(types::WALK_TARGET.id());
        let Some(target) = tick.state.get(types::ROAR_TARGET).cloned() else {
            return;
        };
        utils::look_at_entity(tick.state, target.clone());
        tick.mob.get_entity().set_pose(EntityPose::Roaring);
        if let Some(warden) = warden(tick) {
            warden.increase_anger_at_state(tick.state, Some(&target), 20, false);
        }
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if !tick.state.has_memory_value(types::ROAR_SOUND_DELAY.id())
            && !tick.state.has_memory_value(types::ROAR_SOUND_COOLDOWN.id())
        {
            tick.state.set_with_expiry(
                types::ROAR_SOUND_COOLDOWN,
                (),
                i64::from(ROAR_DURATION) - ROAR_SOUND_DELAY,
            );
            tick.mob
                .play_entity_sound(Sound::EntityWardenRoar, 3.0, 1.0);
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.pose.load() == EntityPose::Roaring {
            entity.set_pose(EntityPose::Standing);
        }
        if let Some(target) = tick.state.get(types::ROAR_TARGET).cloned()
            && let Some(warden) = warden(tick)
        {
            warden.set_attack_target_state(tick.state, target);
        }
        tick.state.erase(types::ROAR_TARGET.id());
    }

    fn debug_name(&self) -> &'static str {
        "Roar"
    }
}

// warden: Sniffing
struct Sniffing;

impl Behavior for Sniffing {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        const CONDITIONS: [(MemoryModuleId, MemoryStatus); 7] = [
            (types::IS_SNIFFING.id(), MemoryStatus::ValuePresent),
            (types::ATTACK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
            (types::LOOK_TARGET.id(), MemoryStatus::Registered),
            (types::NEAREST_ATTACKABLE.id(), MemoryStatus::Registered),
            (types::DISTURBANCE_LOCATION.id(), MemoryStatus::Registered),
            (types::SNIFF_COOLDOWN.id(), MemoryStatus::Registered),
        ];
        &CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        SNIFFING_DURATION
    }

    fn max_duration(&self) -> i32 {
        SNIFFING_DURATION
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.mob
            .play_entity_sound(Sound::EntityWardenSniff, 5.0, 1.0);
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        let entity = tick.mob.get_entity();
        if entity.pose.load() == EntityPose::Sniffing {
            entity.set_pose(EntityPose::Standing);
        }
        tick.state.erase(types::IS_SNIFFING.id());
        let Some(warden) = warden(tick) else {
            return;
        };
        let Some(target) = tick
            .state
            .get(types::NEAREST_ATTACKABLE)
            .cloned()
            .filter(|target| warden.can_target_entity(target.as_ref()))
        else {
            return;
        };
        if closer_than(entity, target.get_entity(), 6.0, 20.0) {
            warden.increase_anger_at_state(tick.state, Some(&target), 35, true);
        }
        if !tick
            .state
            .has_memory_value(types::DISTURBANCE_LOCATION.id())
        {
            warden.set_disturbance_location_state(tick.state, target.get_entity().block_pos.load());
        }
    }

    fn debug_name(&self) -> &'static str {
        "Sniffing"
    }
}

// warden: SonicBoom
struct SonicBoom;

impl Behavior for SonicBoom {
    fn entry_conditions(&self) -> &[(MemoryModuleId, MemoryStatus)] {
        const CONDITIONS: [(MemoryModuleId, MemoryStatus); 4] = [
            (types::ATTACK_TARGET.id(), MemoryStatus::ValuePresent),
            (types::SONIC_BOOM_COOLDOWN.id(), MemoryStatus::ValueAbsent),
            (
                types::SONIC_BOOM_SOUND_COOLDOWN.id(),
                MemoryStatus::Registered,
            ),
            (types::SONIC_BOOM_SOUND_DELAY.id(), MemoryStatus::Registered),
        ];
        &CONDITIONS
    }

    fn min_duration(&self) -> i32 {
        SONIC_BOOM_DURATION
    }

    fn max_duration(&self) -> i32 {
        SONIC_BOOM_DURATION
    }

    fn check_extra_start_conditions(&mut self, tick: &mut BrainTick<'_>) -> bool {
        tick.state.get(types::ATTACK_TARGET).is_some_and(|target| {
            closer_than(tick.mob.get_entity(), target.get_entity(), 15.0, 20.0)
        })
    }

    fn can_still_use(&mut self, _tick: &mut BrainTick<'_>) -> bool {
        true
    }

    fn start(&mut self, tick: &mut BrainTick<'_>) {
        tick.state.set_with_expiry(
            types::ATTACK_COOLING_DOWN,
            true,
            i64::from(SONIC_BOOM_DURATION),
        );
        tick.state.set_with_expiry(
            types::SONIC_BOOM_SOUND_DELAY,
            (),
            SONIC_BOOM_TICKS_BEFORE_SOUND,
        );
        let entity = tick.mob.get_entity();
        tick.world
            .send_entity_status(entity, EntityStatus::SonicCharge, ActorEventID::None);
        tick.mob
            .play_entity_sound(Sound::EntityWardenSonicCharge, 3.0, 1.0);
    }

    fn tick(&mut self, tick: &mut BrainTick<'_>) {
        if let Some(target) = tick.state.get(types::ATTACK_TARGET).cloned() {
            let pos = target.get_entity().pos.load();
            tick.mob
                .get_mob_entity()
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at(tick.mob, pos.x, pos.y, pos.z);
        }
        if tick
            .state
            .has_memory_value(types::SONIC_BOOM_SOUND_DELAY.id())
            || tick
                .state
                .has_memory_value(types::SONIC_BOOM_SOUND_COOLDOWN.id())
        {
            return;
        }
        tick.state.set_with_expiry(
            types::SONIC_BOOM_SOUND_COOLDOWN,
            (),
            i64::from(SONIC_BOOM_DURATION) - SONIC_BOOM_TICKS_BEFORE_SOUND,
        );
        let Some(warden) = warden(tick) else {
            return;
        };
        let Some(target) = tick
            .state
            .get(types::ATTACK_TARGET)
            .cloned()
            .filter(|target| warden.can_target_entity(target.as_ref()))
            .filter(|target| closer_than(tick.mob.get_entity(), target.get_entity(), 15.0, 20.0))
        else {
            return;
        };
        let body = tick.mob.get_entity();
        // WARDEN_CHEST attachment (0, 1.6, 0)
        let source = body.pos.load() + Vector3::new(0.0, 1.6, 0.0);
        let delta = target.get_entity().get_eye_pos() - source;
        let direction = delta.normalize();
        let steps = delta.length().floor() as i32 + 7;
        for i in 1..steps {
            let at = source + direction * f64::from(i);
            for player in tick.world.players.load().iter() {
                if player.position().squared_distance_to_vec(&at) <= 32.0 * 32.0 {
                    player.try_send_client_packet(
                        &pumpkin_protocol::java::client::play::CParticle::new(
                            false,
                            false,
                            at,
                            Vector3::new(0.0, 0.0, 0.0),
                            0.0,
                            1,
                            VarInt(i32::from(Particle::SonicBoom.to_id())),
                            &[],
                        ),
                    );
                }
            }
        }
        tick.mob
            .play_entity_sound(Sound::EntityWardenSonicBoom, 3.0, 1.0);
        let hurt = target.damage_with_context(
            target.as_ref(),
            10.0,
            DamageType::SONIC_BOOM,
            Some(body.pos.load()),
            Some(tick.mob),
            Some(tick.mob),
        );
        if hurt {
            let resistance = target.get_living_entity().map_or(0.0, |living| {
                living.get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE)
            });
            let vertical = 0.5 * (1.0 - resistance);
            let horizontal = 2.5 * (1.0 - resistance);
            warden_push(
                target.as_ref(),
                Vector3::new(
                    direction.x * horizontal,
                    direction.y * vertical,
                    direction.z * horizontal,
                ),
            );
        }
    }

    fn stop(&mut self, tick: &mut BrainTick<'_>) {
        tick.state
            .set_with_expiry(types::SONIC_BOOM_COOLDOWN, (), SONIC_BOOM_COOLDOWN);
    }

    fn debug_name(&self) -> &'static str {
        "SonicBoom"
    }
}

fn warden_push(target: &dyn EntityBase, push: Vector3<f64>) {
    if let Some(player) = target.get_player() {
        let current = crate::entity::Entity::known_movement(target);
        player.set_velocity(current + push);
    } else {
        let entity = target.get_entity();
        entity.velocity.store(entity.velocity.load() + push);
        entity
            .velocity_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
