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

pub struct StraySkeletonEntity {
    entity: Arc<SkeletonEntityBase>,
}

impl StraySkeletonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = SkeletonEntityBase::new(entity);
        Arc::new(Self { entity })
    }
}

impl Mob for StraySkeletonEntity {
    abstract_skeleton_mob!(entity);

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityStrayStep, 0.15, 1.0);
    }

    // stray: slowness arrows, 600 ticks (Stray.getArrow)
    fn get_arrow(&self, arrow: ArrowEntity) -> ArrowEntity {
        arrow.add_effect(&StatusEffect::SLOWNESS, 600);
        arrow
    }
}
