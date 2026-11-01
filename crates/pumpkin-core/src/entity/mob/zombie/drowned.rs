use pumpkin_data::BlockState;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;
use std::sync::Arc;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::living::LivingEntity;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::mob::zombie::ZombieEntityBase;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
};
use crate::world::World;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_nbt::compound::NbtCompound;

const DROWNED_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true).matching(ok_target),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
    Hunt::new(3, TargetMatch::Type(&EntityType::AXOLOTL), true),
    hunting::baby_turtles(5, true),
];

/// Vanilla `okTarget`: players only at night or in water.
fn ok_target(_hunter: &dyn Mob, target: &LivingEntity, world: &World) -> bool {
    !world.is_bright_outside() || target.entity.is_in_water()
}

pub struct DrownedEntity {
    entity: Arc<ZombieEntityBase>,
}

impl DrownedEntity {
    #[must_use]
    pub fn base(&self) -> &ZombieEntityBase {
        &self.entity
    }

    pub fn new(entity: Entity) -> Arc<Self> {
        Self::with_can_break_doors(entity, false)
    }

    #[must_use]
    pub fn with_can_break_doors(entity: Entity, can_break_doors: bool) -> Arc<Self> {
        let entity = ZombieEntityBase::with_targets(
            entity,
            can_break_doors,
            ZombieEntityBase::revenge_goal()
                .ignoring(|entity_type| entity_type == &EntityType::DROWNED),
            DROWNED_HUNTS,
            false,
        );
        let zombie = Self { entity };
        Arc::new(zombie)
    }
}

impl Mob for DrownedEntity {
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
        Some(Sound::EntityDrownedAmbient)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityDrownedStep, 0.15, 1.0);
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
