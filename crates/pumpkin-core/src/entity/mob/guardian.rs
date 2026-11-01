use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::attributes::Attributes;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::potion::Effect;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_data::{damage::DamageType, sound::Sound};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_protocol::java::client::play::{CGameEvent, GameEvent};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::{Difficulty, GameMode};
use rand::RngExt;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::control::move_control::{MoveControl, Operation};
use crate::entity::ai::control::{Control, MoveControlTrait};
use crate::entity::ai::goal::move_towards_restriction::MoveTowardsRestrictionGoal;
use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::ai::pathfinder::Navigator;
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::custom_sound::CustomSound;
use crate::entity::living::LivingEntity;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};
use crate::plugin::api::events::entity::entity_target::TargetReason;
use crate::world::World;

const GUARDIAN_PREY: TargetMatch = TargetMatch::AnyOf(&[
    &EntityType::PLAYER,
    &EntityType::SQUID,
    &EntityType::GLOW_SQUID,
    &EntityType::AXOLOTL,
]);

/// Vanilla `GuardianAttackSelector`: nothing within 3 blocks.
fn outside_melee_range(hunter: &dyn Mob, target: &LivingEntity, _world: &World) -> bool {
    let hunter_pos = hunter.get_mob_entity().living_entity.entity.pos.load();
    target
        .entity
        .pos
        .load()
        .squared_distance_to_vec(&hunter_pos)
        > 9.0
}

pub const GUARDIAN_HUNTS: &[Hunt] =
    &[Hunt::new(1, GUARDIAN_PREY, true).matching(outside_melee_range)];

const ATTACK_TIME: i32 = 80;
const ELDER_ATTACK_TIME: i32 = 60;
const STROLL_INTERVAL: i32 = 80;
const ELDER_STROLL_INTERVAL: i32 = 400;
const MAX_AIR_SUPPLY: i32 = 300;
const AMBIENT_SOUND_INTERVAL: i32 = 160;
const ELDER_EFFECT_INTERVAL: i32 = 1200;
const ELDER_EFFECT_RADIUS: f64 = 50.0;
const ELDER_EFFECT_DURATION: i32 = 6000;
const ELDER_EFFECT_AMPLIFIER: u8 = 2;
const ELDER_EFFECT_DISPLAY_LIMIT: i32 = 1200;
const ELDER_HOME_RANGE: i32 = 16;

// guardian + elder guardian (Guardian, ElderGuardian via `elder`)
pub struct GuardianEntity {
    pub mob_entity: MobEntity,
    elder: bool,
    moving: AtomicBool,
    active_attack_target: AtomicI32,
    stroll_trigger: Arc<AtomicBool>,
}

impl GuardianEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        Self::create(entity, false)
    }

    pub fn new_elder(entity: Entity) -> Arc<Self> {
        Self::create(entity, true)
    }

    fn create(entity: Entity, elder: bool) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Navigator::water_bound(false);
        if elder {
            mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        let stroll_trigger = Arc::new(AtomicBool::new(false));
        let mob_arc = Arc::new(Self {
            mob_entity,
            elder,
            moving: AtomicBool::new(false),
            active_attack_target: AtomicI32::new(0),
            stroll_trigger: stroll_trigger.clone(),
        });
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            *mob_arc
                .mob_entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Box::new(GuardianMoveControl::new(Arc::downgrade(&mob_arc)));

            let goal_selector = &mob_arc.mob_entity.goals_selector;
            goal_selector.add_goal(
                4,
                Box::new(GuardianAttackGoal::new(Arc::downgrade(&mob_arc))),
            );
            goal_selector.add_goal(
                5,
                Box::new(GuardianRestrictionGoal {
                    inner: MoveTowardsRestrictionGoal::new(1.0),
                }),
            );
            goal_selector.add_goal(
                7,
                Box::new(GuardianStrollGoal {
                    inner: WanderAroundGoal::with_interval(
                        1.0,
                        if elder {
                            ELDER_STROLL_INTERVAL
                        } else {
                            STROLL_INTERVAL
                        },
                    ),
                    trigger: stroll_trigger,
                }),
            );
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::matching(
                    mob_weak,
                    TargetMatch::AnyOf(&[&EntityType::GUARDIAN, &EntityType::ELDER_GUARDIAN]),
                    12.0,
                    0.01,
                ),
            );
            goal_selector.add_goal(9, Box::new(RandomLookAroundGoal::default()));

            let target_selector = &mob_arc.mob_entity.target_selector;
            hunting::apply(target_selector, &mob_arc.mob_entity, GUARDIAN_HUNTS);
        };

        mob_arc
    }

    const fn attack_duration(&self) -> i32 {
        if self.elder {
            ELDER_ATTACK_TIME
        } else {
            ATTACK_TIME
        }
    }

    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.moving.load(Ordering::Relaxed)
    }

    fn set_moving(&self, moving: bool) {
        self.moving.store(moving, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked_data::guardian::DATA_ID_MOVING, moving);
    }

    fn set_active_attack_target(&self, entity_id: i32) {
        self.active_attack_target
            .store(entity_id, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked_data::guardian::DATA_ID_ATTACK_TARGET, entity_id);
    }

    fn has_active_attack_target(&self) -> bool {
        self.active_attack_target.load(Ordering::Relaxed) != 0
    }

    // elder guardian: mining fatigue to players around (MobEffectUtil.addEffectToPlayersAround)
    fn apply_elder_effect(&self) {
        let entity = self.get_entity();
        let pos = entity.pos.load();
        let world = entity.world.load();
        let silent = entity.is_silent();
        let self_base: &dyn EntityBase = self;
        for player in world.get_nearby_players(pos, ELDER_EFFECT_RADIUS) {
            let mode = player.gamemode.load();
            if !matches!(mode, GameMode::Survival | GameMode::Adventure)
                || self_base.is_allied_to(player.as_ref())
                || player
                    .living_entity
                    .get_effect(&StatusEffect::MINING_FATIGUE)
                    .is_some_and(|effect| {
                        effect.amplifier >= ELDER_EFFECT_AMPLIFIER
                            && (effect.duration == -1
                                || effect.duration > ELDER_EFFECT_DISPLAY_LIMIT - 1)
                    })
            {
                continue;
            }
            player.living_entity.add_effect(Effect {
                effect_type: &StatusEffect::MINING_FATIGUE,
                duration: ELDER_EFFECT_DURATION,
                amplifier: ELDER_EFFECT_AMPLIFIER,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
            player.try_send_client_packet(&CGameEvent::new(
                GameEvent::PlayElderGuardianMobAppearance,
                if silent { 0.0 } else { 1.0 },
            ));
        }
    }
}

impl CustomSound for GuardianEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        let in_water = self.get_entity().is_in_water();
        Some(match (self.elder, in_water) {
            (false, true) => Sound::EntityGuardianHurt,
            (false, false) => Sound::EntityGuardianHurtLand,
            (true, true) => Sound::EntityElderGuardianHurt,
            (true, false) => Sound::EntityElderGuardianHurtLand,
        })
    }

    fn death_sound(&self) -> Option<Sound> {
        let in_water = self.get_entity().is_in_water();
        Some(match (self.elder, in_water) {
            (false, true) => Sound::EntityGuardianDeath,
            (false, false) => Sound::EntityGuardianDeathLand,
            (true, true) => Sound::EntityElderGuardianDeath,
            (true, false) => Sound::EntityElderGuardianDeathLand,
        })
    }
}

impl Mob for GuardianEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        let in_water = self.get_entity().is_in_water();
        Some(match (self.elder, in_water) {
            (false, true) => Sound::EntityGuardianAmbient,
            (false, false) => Sound::EntityGuardianAmbientLand,
            (true, true) => Sound::EntityElderGuardianAmbient,
            (true, false) => Sound::EntityElderGuardianAmbientLand,
        })
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        AMBIENT_SOUND_INTERVAL
    }

    fn get_max_look_pitch_change(&self) -> f32 {
        180.0
    }

    // guardian: water is worth 10 + light (Guardian.getWalkTargetValue)
    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let world = self.get_entity().world.load();
        let light_cost = world.get_light_level_dependent_magic_value(pos) - 0.5;
        if world.get_fluid(pos).has_tag(&tag::Fluid::MINECRAFT_WATER) {
            10.0 + light_cost
        } else {
            -light_cost
        }
    }

    // guardian: air refill, land flop, face beam target (Guardian.aiStep)
    fn before_mob_ai_step(&self) {
        let entity = self.get_entity();
        if !entity.is_alive() {
            return;
        }
        if entity.is_in_water() {
            entity.set_air_supply(MAX_AIR_SUPPLY);
        } else if entity.on_ground.load(Ordering::Relaxed) {
            let mut random = self.get_random();
            let push = Vector3::new(
                f64::from((random.random::<f32>() * 2.0 - 1.0) * 0.4),
                0.5,
                f64::from((random.random::<f32>() * 2.0 - 1.0) * 0.4),
            );
            entity.velocity.store(entity.velocity.load() + push);
            entity.yaw.store(random.random::<f32>() * 360.0);
            entity.on_ground.store(false, Ordering::Relaxed);
            entity.velocity_dirty.store(true, Ordering::Relaxed);
        }
        if self.has_active_attack_target() {
            entity.yaw.store(entity.head_yaw.load());
        }
    }

    // guardian: slow swim, sinks when idle (Guardian.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(living.movement_input.load(), 0.1);
        living.make_move(caller);
        let mut velocity = entity.velocity.load() * 0.9;
        if !self.is_moving() && self.mob_entity.get_target().is_none() {
            velocity.y -= 0.005;
        }
        entity.velocity.store(velocity);
    }

    // guardian: thorns when hit while still (Guardian.hurtServer)
    fn pre_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        if !self.is_moving()
            && !damage_type.has_tag(&tag::DamageType::MINECRAFT_AVOIDS_GUARDIAN_THORNS)
            && damage_type != DamageType::THORNS
            && let Some(direct) = source
            && direct.get_living_entity().is_some()
        {
            direct.damage_with_context(
                direct,
                2.0,
                DamageType::THORNS,
                None,
                Some(self),
                Some(self),
            );
        }
        self.stroll_trigger.store(true, Ordering::Relaxed);
        self.pre_damage(damage_type, source)
    }

    // elder guardian: fatigue pulse + home (ElderGuardian.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        if !self.elder {
            return;
        }
        let entity = self.get_entity();
        if (entity.tick_count.load(Ordering::Relaxed) + entity.entity_id) % ELDER_EFFECT_INTERVAL
            == 0
        {
            self.apply_elder_effect();
        }
        if !self.mob_entity.has_position_target() {
            self.mob_entity
                .position_target
                .store(entity.block_pos.load());
            self.mob_entity
                .position_target_range
                .store(ELDER_HOME_RANGE, Ordering::Relaxed);
        }
    }
}

// guardian: stroll goal that hits/attacks can force (RandomStrollGoal.trigger)
struct GuardianStrollGoal {
    inner: WanderAroundGoal,
    trigger: Arc<AtomicBool>,
}

impl Goal for GuardianStrollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if self.trigger.swap(false, Ordering::Relaxed) {
            self.inner.trigger();
        }
        self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// guardian: restriction goal with MOVE|LOOK flags
struct GuardianRestrictionGoal {
    inner: MoveTowardsRestrictionGoal,
}

impl Goal for GuardianRestrictionGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// guardian: beam attack goal (Guardian.GuardianAttackGoal)
struct GuardianAttackGoal {
    guardian: Weak<GuardianEntity>,
    attack_time: i32,
}

impl GuardianAttackGoal {
    const fn new(guardian: Weak<GuardianEntity>) -> Self {
        Self {
            guardian,
            attack_time: 0,
        }
    }
}

fn living_target(mob: &dyn Mob) -> Option<Arc<dyn EntityBase>> {
    mob.get_mob_entity().get_target().filter(|target| {
        target
            .get_living_entity()
            .is_some_and(LivingEntity::is_alive)
    })
}

impl Goal for GuardianAttackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        living_target(mob).is_some()
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(target) = living_target(mob) else {
            return false;
        };
        self.guardian
            .upgrade()
            .is_some_and(|guardian| guardian.elder)
            || mob
                .get_entity()
                .pos
                .load()
                .squared_distance_to_vec(&target.get_entity().pos.load())
                > 9.0
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.attack_time = -10;
        let mob_entity = mob.get_mob_entity();
        mob_entity.steered().navigator().stop();
        if let Some(target) = mob_entity.get_target() {
            mob_entity
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at_entity_with_range(&target, 90.0, 90.0);
        }
        mob.get_entity()
            .velocity_dirty
            .store(true, Ordering::Relaxed);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(guardian) = self.guardian.upgrade() {
            guardian.set_active_attack_target(0);
            guardian.stroll_trigger.store(true, Ordering::Relaxed);
        }
        mob.set_mob_target(None, TargetReason::Unknown);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        let Some(target) = mob_entity.get_target() else {
            return;
        };
        let Some(guardian) = self.guardian.upgrade() else {
            return;
        };
        mob_entity.steered().navigator().stop();
        mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_entity_with_range(&target, 90.0, 90.0);
        if !mob.has_line_of_sight(target.get_entity()) {
            mob.set_mob_target(None, TargetReason::Unknown);
            return;
        }
        self.attack_time += 1;
        let entity = mob.get_entity();
        if self.attack_time == 0 {
            guardian.set_active_attack_target(target.get_entity().entity_id);
            if !entity.is_silent() {
                entity.world.load().send_entity_status(
                    entity,
                    EntityStatus::GuardianAttackSound,
                    ActorEventID::None,
                );
            }
        } else if self.attack_time >= guardian.attack_duration() {
            let mut magic_damage = 1.0;
            if entity.world.load().level_info.load().difficulty == Difficulty::Hard {
                magic_damage += 2.0;
            }
            if guardian.elder {
                magic_damage += 2.0;
            }
            let guardian_base: &dyn EntityBase = guardian.as_ref();
            target.damage_with_context(
                target.as_ref(),
                magic_damage,
                DamageType::INDIRECT_MAGIC,
                None,
                Some(guardian_base),
                Some(guardian_base),
            );
            mob_entity.try_attack(mob, target.as_ref());
            mob.set_mob_target(None, TargetReason::Unknown);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// guardian: swim move control with wobble + smoothed look (Guardian.GuardianMoveControl)
struct GuardianMoveControl {
    guardian: Weak<GuardianEntity>,
    wanted: Vector3<f64>,
    speed_modifier: f64,
    operation: Operation,
    last_look: Option<(Vector3<f64>, i32)>,
}

impl GuardianMoveControl {
    const fn new(guardian: Weak<GuardianEntity>) -> Self {
        Self {
            guardian,
            wanted: Vector3::new(0.0, 0.0, 0.0),
            speed_modifier: 0.0,
            operation: Operation::Wait,
            last_look: None,
        }
    }
}

impl Control for GuardianMoveControl {}

impl MoveControlTrait for GuardianMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let Some(guardian) = self.guardian.upgrade() else {
            return;
        };
        let mob_entity = mob.get_mob_entity();
        let living = &mob_entity.living_entity;
        let entity = &living.entity;
        if self.operation != Operation::MoveTo || mob.is_navigator_idle() {
            mob_entity.set_speed(0.0);
            guardian.set_moving(false);
            return;
        }

        let pos = entity.pos.load();
        let delta = self.wanted - pos;
        let length = delta.length();
        let direction = delta / length;
        let y_rot_d = (delta.z.atan2(delta.x).to_degrees() as f32) - 90.0;
        let yaw = MoveControl::rotlerp(entity.yaw.load(), y_rot_d, 90.0);
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);

        let target_speed =
            (self.speed_modifier * living.get_attribute_value(&Attributes::MOVEMENT_SPEED)) as f32;
        let speed = living.get_speed() + (target_speed - living.get_speed()) * 0.125;
        mob_entity.set_speed(speed);

        let tick = f64::from(entity.tick_count.load(Ordering::Relaxed) + entity.entity_id);
        let push = (tick * 0.5).sin() * 0.05;
        let (sin, cos) = f64::from(yaw).to_radians().sin_cos();
        let y_push = (tick * 0.75).sin() * 0.05;
        entity.velocity.store(
            entity.velocity.load()
                + Vector3::new(
                    push * cos,
                    y_push * (sin + cos) * 0.25 + f64::from(speed) * direction.y * 0.1,
                    push * sin,
                ),
        );

        let new_look = Vector3::new(
            pos.x + direction.x * 2.0,
            entity.get_eye_y() + direction.y / length,
            pos.z + direction.z * 2.0,
        );
        let now = entity.tick_count.load(Ordering::Relaxed);
        let old_look = self
            .last_look
            .filter(|(_, at)| now - at <= 2)
            .map_or(new_look, |(look, _)| look);
        let look = old_look + (new_look - old_look) * 0.125;
        mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_with_range(look.x, look.y, look.z, 10.0, 40.0);
        self.last_look = Some((look, now));
        guardian.set_moving(true);
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.wanted = Vector3::new(x, y, z);
        self.speed_modifier = speed_modifier;
        self.operation = Operation::MoveTo;
    }

    fn has_wanted(&self) -> bool {
        self.operation == Operation::MoveTo
    }
}
