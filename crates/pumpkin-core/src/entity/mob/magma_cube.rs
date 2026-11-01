use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::{attributes::Attributes, sound::Sound};

use crate::entity::{
    Entity, EntityBase,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, slime::SlimeEntity, spawn::SpawnGroupData},
};
use crate::world::World;

pub struct MagmaCubeEntity {
    pub slime: Arc<SlimeEntity>,
}

impl MagmaCubeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let slime = SlimeEntity::new(entity);
        let size = slime.get_size();
        {
            let mut attributes = slime
                .get_mob_entity()
                .living_entity
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
                speed.base_value = 0.2;
                speed.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(damage) = attributes.get_mut(&Attributes::ATTACK_DAMAGE.id) {
                damage.base_value = (size + 2) as f64;
                damage.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(armor) = attributes.get_mut(&Attributes::ARMOR.id) {
                armor.base_value = (size * 3) as f64;
                armor.dirty.store(true, Ordering::Relaxed);
            }
        }
        Arc::new(Self { slime })
    }

    pub fn set_size(&self, size: i32, update_health: bool) {
        self.slime.set_size(size, update_health);
        let size = self.slime.get_size();
        let mut attributes = self
            .slime
            .get_mob_entity()
            .living_entity
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(damage) = attributes.get_mut(&Attributes::ATTACK_DAMAGE.id) {
            damage.base_value = (size + 2) as f64;
            damage.dirty.store(true, Ordering::Relaxed);
        }
        if let Some(armor) = attributes.get_mut(&Attributes::ARMOR.id) {
            armor.base_value = (size * 3) as f64;
            armor.dirty.store(true, Ordering::Relaxed);
        }
    }
}

impl CustomSound for MagmaCubeEntity {
    fn death_sound(&self) -> Option<Sound> {
        let size = self.slime.get_size();
        Some(if size == 1 {
            Sound::EntityMagmaCubeDeathSmall
        } else {
            Sound::EntityMagmaCubeDeath
        })
    }
    fn hurt_sound(&self) -> Option<Sound> {
        let size = self.slime.get_size();
        Some(if size == 1 {
            Sound::EntityMagmaCubeHurtSmall
        } else {
            Sound::EntityMagmaCubeHurt
        })
    }
}

impl Mob for MagmaCubeEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_mob_entity(&self) -> &MobEntity {
        self.slime.get_mob_entity()
    }

    fn mob_metadata(&self) -> Option<crate::entity::mob::MobMetadata> {
        self.slime.mob_metadata()
    }

    fn mob_write_nbt(&self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        self.slime.mob_write_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &pumpkin_nbt::compound::NbtCompound) {
        self.slime.mob_read_nbt(nbt);
        self.set_size(self.slime.get_size(), false);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let data = self.slime.finalize_spawn(world, group_data);
        self.set_size(self.slime.get_size(), true);
        data
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.slime.get_size() as u32
    }

    fn mob_tick(&self, caller: &dyn EntityBase) {
        self.slime.mob_tick(caller);
    }

    fn post_tick(&self) {
        self.slime.post_tick();
    }

    fn mob_player_collision(&self, player: &Arc<crate::entity::player::Player>) {
        let damaged = self
            .slime
            .get_mob_entity()
            .try_attack(&*self.slime, &**player);
        if damaged {
            self.slime.on_attack(&**player);
        }
    }

    fn mob_entity_collision(&self, entity: &dyn EntityBase) {
        if entity.get_entity().entity_type == &pumpkin_data::entity::EntityType::IRON_GOLEM {
            let damaged = self.slime.get_mob_entity().try_attack(self, entity);
            if damaged {
                self.on_attack(entity);
            }
        }
    }
}
