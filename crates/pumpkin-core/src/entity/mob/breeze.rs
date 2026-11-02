use std::sync::Arc;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag;
use pumpkin_data::tag::Taggable;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::{Rng, RngExt};

use super::breeze_ai;
use crate::entity::ai::brain::Brain;
use crate::entity::ai::brain::memory::{PackedMemories, types};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::{Mob, MobEntity};
use crate::entity::projectile_deflection::ProjectileDeflectionType;
use crate::entity::{Entity, EntityBase};
use crate::plugin::api::events::entity::entity_target::TargetReason;

pub(super) const JUMP_CIRCLE_INNER_RADIUS: f64 = 4.0;
pub(super) const JUMP_CIRCLE_MIDDLE_RADIUS: f64 = 8.0;
const MAX_HEAD_Y_ROT: f32 = 30.0;
const HEAD_ROT_SPEED: f32 = 25.0;
const FALL_DISTANCE_SOUND_TRIGGER_THRESHOLD: f32 = 3.0;
const MAX_LINE_OF_SIGHT_TEST_RANGE: f64 = 50.0;
const INNER_CIRCLE_VERTICAL_RANGE: f64 = 10.0;

pub struct BreezeEntity {
    pub mob_entity: MobEntity,
}

impl BreezeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        {
            let mut navigator = mob_entity
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // breeze: setPathfindingMalus ON_TOP_OF_TRAPDOOR, FIRE
            navigator.set_pathfinding_malus(PathType::DangerTrapdoor, -1.0);
            navigator.set_pathfinding_malus(PathType::DamageFire, -1.0);
        }
        Arc::new(Self { mob_entity })
    }

    // breeze: withinInnerCircleRange
    pub(super) fn within_inner_circle_range(&self, target: Vector3<f64>) -> bool {
        let ours = centered(self.get_entity().block_pos.load());
        let dx = target.x - ours.x;
        let dz = target.z - ours.z;
        dx * dx + dz * dz < JUMP_CIRCLE_INNER_RADIUS * JUMP_CIRCLE_INNER_RADIUS
            && (target.y - ours.y).abs() < INNER_CIRCLE_VERTICAL_RANGE
    }

    // breeze: getFiringYPosition
    pub(super) fn firing_y_position(&self) -> f64 {
        let entity = self.get_entity();
        entity.pos.load().y
            + f64::from(entity.entity_dimension.load().height) / 2.0
            + f64::from(0.3_f32)
    }

    // breeze: getTargetFromBrain
    fn sync_target_from_brain(&self) {
        let target = self
            .mob_entity
            .with_brain(self, |brain| brain.get(types::ATTACK_TARGET).cloned());
        self.apply_mob_target(target);
    }
}

pub(super) fn centered(pos: BlockPos) -> Vector3<f64> {
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y) + 0.5,
        f64::from(pos.0.z) + 0.5,
    )
}

pub(super) fn bottom_centered(pos: BlockPos) -> Vector3<f64> {
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y),
        f64::from(pos.0.z) + 0.5,
    )
}

// breeze: Mob.lookAt(Anchor.EYES, pos)
pub(super) fn look_at_eyes(mob: &dyn Mob, target: Vector3<f64>) {
    let entity = mob.get_entity();
    let delta = target.sub(&entity.get_eye_pos());
    let root = delta.x.hypot(delta.z);
    let pitch = pumpkin_util::math::wrap_degrees((-delta.y.atan2(root) as f32).to_degrees());
    let yaw = pumpkin_util::math::wrap_degrees((delta.z.atan2(delta.x) as f32).to_degrees() - 90.0);
    entity.pitch.store(pitch);
    entity.yaw.store(yaw);
    entity.head_yaw.store(yaw);
}

// breeze: RandomSource.nextGaussian (Marsaglia polar)
fn next_gaussian(rng: &mut impl Rng) -> f64 {
    loop {
        let v1 = 2.0 * rng.random::<f64>() - 1.0;
        let v2 = 2.0 * rng.random::<f64>() - 1.0;
        let s = v1 * v1 + v2 * v2;
        if s < 1.0 && s != 0.0 {
            return v1 * (-2.0 * s.ln() / s).sqrt();
        }
    }
}

// breeze: BreezeUtil.randomPointBehindTarget
pub(super) fn random_point_behind_target(
    enemy: &dyn EntityBase,
    rng: &mut impl Rng,
) -> Vector3<f64> {
    let enemy_entity = enemy.get_entity();
    let view_angle = enemy_entity.head_yaw.load() + 180.0 + next_gaussian(rng) as f32 * 90.0 / 2.0;
    let r = f64::from(4.0 + rng.random::<f32>() * (8.0 - 4.0));
    let direction = Vector3::rotation_vector(0.0, f64::from(view_angle));
    enemy_entity.pos.load().add(&direction.multiply(r, r, r))
}

// breeze: BreezeUtil.hasLineOfSight
pub(super) fn has_line_of_sight(breeze: &dyn Mob, target: Vector3<f64>) -> bool {
    let entity = breeze.get_entity();
    let from = entity.pos.load();
    let follow_range = breeze
        .get_mob_entity()
        .living_entity
        .get_attribute_value(&Attributes::FOLLOW_RANGE);
    if target.sub(&from).length() > MAX_LINE_OF_SIGHT_TEST_RANGE.max(follow_range) {
        return false;
    }
    entity
        .world
        .load()
        .clip(&pumpkin_world::clip::ClipContext::new(
            from,
            target,
            pumpkin_world::clip::ClipBlock::Collider,
            pumpkin_world::clip::ClipFluid::None,
        ))
        .is_miss
}

impl Mob for BreezeEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        breeze_ai::make_brain(self, packed)
    }

    // breeze: customServerAiStep (breezeBrain, breezeActivityUpdate)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, breeze_ai::update_activity);
        self.sync_target_from_brain();
    }

    // breeze: canAttack
    fn can_attack(&self, target: &dyn EntityBase) -> bool {
        let target_type = target.get_entity().entity_type;
        (target_type == &EntityType::PLAYER || target_type == &EntityType::IRON_GOLEM)
            && self.mob_entity.living_entity.can_attack(target)
    }

    // breeze: getTarget reads the brain, Mob.setTarget never sticks
    fn set_mob_target(&self, _target: Option<Arc<dyn EntityBase>>, _reason: TargetReason) -> bool {
        true
    }

    fn get_max_head_rotation(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    fn get_max_look_yaw_change(&self) -> f32 {
        HEAD_ROT_SPEED
    }

    // breeze: isInvulnerableTo, damage caused by another Breeze
    fn pre_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        !cause.is_some_and(|cause| cause.get_entity().entity_type == &EntityType::BREEZE)
            && self.pre_damage(damage_type, source)
    }

    // breeze: deflection (PROJECTILE_DEFLECTION), wind charges pass
    fn projectile_deflection(&self, projectile: &dyn EntityBase) -> ProjectileDeflectionType {
        let projectile_type = projectile.get_entity().entity_type;
        if projectile_type == &EntityType::BREEZE_WIND_CHARGE
            || projectile_type == &EntityType::WIND_CHARGE
        {
            return ProjectileDeflectionType::None;
        }
        if self
            .get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_DEFLECTS_PROJECTILES)
        {
            ProjectileDeflectionType::SimpleWithSound(Sound::EntityBreezeDeflect)
        } else {
            ProjectileDeflectionType::None
        }
    }

    // breeze: causeFallDamage landing sound
    fn on_cause_fall_damage(&self, fall_distance: f32) {
        if fall_distance > FALL_DISTANCE_SOUND_TRIGGER_THRESHOLD {
            let entity = self.get_entity();
            entity.world.load().play_sound_fine(
                Sound::EntityBreezeLand,
                SoundCategory::Hostile,
                &entity.pos.load(),
                1.0,
                1.0,
            );
        }
    }
}

// breeze: ambient, whirl sounds and ground particles are client-side (playLocalSound, addParticle)
impl CustomSound for BreezeEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBreezeDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBreezeHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}
