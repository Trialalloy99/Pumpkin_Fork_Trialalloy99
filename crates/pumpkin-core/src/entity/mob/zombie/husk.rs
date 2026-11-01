use pumpkin_data::BlockState;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;
use std::sync::Arc;

use crate::entity::mob::equipment::RegionalDifficulty;
use crate::entity::mob::spawn::{SpawnGroupData, SpawnReason, finalize_spawn};
use crate::entity::mob::zombie::ZombieEntityBase;
use crate::entity::r#type::from_type;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
};
use crate::world::World;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_nbt::compound::NbtCompound;
use rand::RngExt;

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

impl HuskEntity {
    // husk: ride a new camel husk with a parched behind, iron spear (Husk.finalizeSpawn)
    fn spawn_camel_husk_jockey(&self, world: &Arc<World>, reason: SpawnReason) {
        let mob_entity = self.get_mob_entity();
        mob_entity.set_item_slot(
            &EquipmentSlot::MAIN_HAND,
            ItemStack::new(1, &Item::IRON_SPEAR),
        );
        let entity = self.get_entity();
        let pos = entity.pos.load();
        let camel_husk = from_type(&EntityType::CAMEL_HUSK, pos, world, uuid::Uuid::new_v4());
        finalize_spawn(&camel_husk, world, None, reason);
        let parched = from_type(&EntityType::PARCHED, pos, world, uuid::Uuid::new_v4());
        parched.get_entity().set_rotation(entity.yaw.load(), 0.0);
        finalize_spawn(&parched, world, None, reason);
        if let Some(camel_mob) = camel_husk.get_mob() {
            camel_mob.get_mob_entity().add_pending_rider(parched);
        }
        mob_entity.set_pending_vehicle(camel_husk, true);
    }
}

impl Mob for HuskEntity {
    fn killed_entity(&self, victim: &dyn EntityBase) -> bool {
        crate::entity::mob::zombie::infect_villager(self.get_entity(), victim)
    }

    // zombie family: no glow ink sacs (Zombie.wantsToPickUp)
    fn mob_wants_to_pick_up(&self, stack: &pumpkin_data::item_stack::ItemStack) -> bool {
        stack.item != &pumpkin_data::item::Item::GLOW_INK_SAC
    }

    // zombie family: spawn rolls, hit effects, reinforcements, baby xp (Zombie)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut group_data = self.entity.finalize_zombie_spawn(self, world, group_data);
        let mob_entity = self.get_mob_entity();
        let reason = mob_entity.spawn_reason();
        let multiplier =
            RegionalDifficulty::at(world, self.get_entity().pos.load()).special_multiplier;
        let mut random = rand::rng();
        // husk: second loot roll + camel husk jockey (Husk.finalizeSpawn)
        if reason != SpawnReason::Conversion {
            mob_entity.set_can_pick_up_loot(random.random::<f32>() < 0.55 * multiplier);
        }
        if let Some(SpawnGroupData::Zombie {
            tried_camel_husk, ..
        }) = group_data.as_mut()
        {
            *tried_camel_husk = reason != SpawnReason::Natural;
            if !*tried_camel_husk {
                let pos = self.get_entity().block_pos.load();
                let spawn_box = EntityType::CAMEL_HUSK.get_spawn_aabb(
                    f64::from(pos.0.x) + 0.5,
                    f64::from(pos.0.y),
                    f64::from(pos.0.z) + 0.5,
                );
                if world.is_space_empty(spawn_box) {
                    *tried_camel_husk = true;
                    if random.random::<f32>() < 0.1 {
                        self.spawn_camel_husk_jockey(world, reason);
                    }
                }
            }
        }
        group_data
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
