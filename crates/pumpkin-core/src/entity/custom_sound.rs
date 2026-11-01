use crate::entity::mob::Mob;
use pumpkin_data::sound::{Sound, SoundCategory};

pub trait CustomSound: Mob {
    fn death_sound(&self) -> Option<Sound>;
    fn hurt_sound(&self) -> Option<Sound>;

    fn sound_volume(&self) -> f32 {
        1.0
    }

    // mob sounds: the type's sound source by default (Entity.getSoundSource)
    fn sound_category(&self) -> SoundCategory {
        self.get_mob_entity()
            .living_entity
            .entity
            .entity_type
            .sound_source
    }
}
