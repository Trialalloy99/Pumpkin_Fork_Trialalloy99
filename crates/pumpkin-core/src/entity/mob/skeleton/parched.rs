use pumpkin_data::BlockState;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;
use std::sync::Arc;

use crate::entity::{
    Entity, EntityBase,
    mob::{
        Mob,
        skeleton::{SkeletonEntityBase, abstract_skeleton_mob},
    },
    projectile::arrow::ArrowEntity,
};

pub struct ParchedSkeletonEntity {
    entity: Arc<SkeletonEntityBase>,
}

impl ParchedSkeletonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = SkeletonEntityBase::new(entity);
        Arc::new(Self { entity })
    }
}

// parched: weakness immunity lives in LivingEntity::can_be_affected
impl Mob for ParchedSkeletonEntity {
    abstract_skeleton_mob!(entity);

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityParchedStep, 0.15, 1.0);
    }

    // parched: weakness arrows, 600 ticks (Parched.getArrow)
    fn get_arrow(&self, arrow: ArrowEntity) -> ArrowEntity {
        arrow.add_effect(&StatusEffect::WEAKNESS, 600);
        arrow
    }

    // parched: slower bow (Parched.getHardAttackInterval/getAttackInterval)
    fn bow_attack_intervals(&self) -> (i32, i32) {
        (50, 70)
    }
}
