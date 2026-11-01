use pumpkin_data::BlockState;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;
use std::sync::Arc;

use crate::entity::mob::equipment::RegionalDifficulty;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::mob::zombie::ZombieEntityBase;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
};
use crate::world::World;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::potion::Effect;
use pumpkin_nbt::compound::NbtCompound;

pub struct HuskEntity {
    entity: Arc<ZombieEntityBase>,
}

impl HuskEntity {
    #[must_use]
    pub fn base(&self) -> &ZombieEntityBase {
        &self.entity
    }

    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = ZombieEntityBase::new(entity);
        let zombie = Self { entity };
        Arc::new(zombie)
    }

    #[must_use]
    pub fn with_can_break_doors(entity: Entity, can_break_doors: bool) -> Arc<Self> {
        let entity = ZombieEntityBase::with_can_break_doors(entity, can_break_doors);
        let zombie = Self { entity };
        Arc::new(zombie)
    }
}

impl Mob for HuskEntity {
    // zombie family: spawn rolls, hit effects, reinforcements, baby xp (Zombie)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.entity.finalize_zombie_spawn(self, world, group_data)
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        self.entity.on_zombie_attack(self, target);
        if self.entity.main_hand_empty()
            && let Some(living) = target.get_living_entity()
        {
            let entity = self.get_entity();
            let difficulty = RegionalDifficulty::at(&entity.world.load_full(), entity.pos.load())
                .effective_difficulty;
            living.add_effect(Effect {
                effect_type: &StatusEffect::HUNGER,
                duration: 140 * difficulty as i32,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
        }
    }

    fn on_damage_with_context(
        &self,
        _damage_type: DamageType,
        _source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.entity.on_zombie_hurt(cause);
        true
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.entity
            .experience_reward(self.get_entity().entity_type.experience_reward)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityHuskAmbient)
    }

    fn post_tick(&self) {
        self.entity.tick_drowning(self);
    }

    fn populate_default_equipment_slots(
        &self,
        world: &Arc<World>,
        difficulty: &RegionalDifficulty,
    ) {
        self.entity
            .populate_default_equipment_slots(world, difficulty);
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityHuskStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity.mob_entity
    }

    fn spawn_as_baby(&self) -> bool {
        self.entity.spawn_as_baby()
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.entity.mob_write_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.entity.mob_read_nbt(nbt);
    }
}
