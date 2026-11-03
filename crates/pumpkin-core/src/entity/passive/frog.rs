mod ai;

use std::sync::{
    Arc, PoisonError,
    atomic::{AtomicI32, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::{optional_int::OptionalInt, var_int::VarInt};
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::brain::memory::{PackedMemories, types};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::control::smooth_swimming_move_control::SmoothSwimmingMoveControl,
    ai::pathfinder::{Navigator, node::PathType},
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

use pumpkin_data::frog_variant::FrogVariant;

// frog: constants (Frog)
const FROG_FALL_DAMAGE_REDUCTION: i32 = 5;
const FROG_HEAD_ROT_SPEED: f32 = 35.0;
const FROG_MAX_HEAD_Y_ROT: f32 = 5.0;
const FROG_BREEDING_COOLDOWN: i32 = 6000;

/// Represents a Frog, an amphibious mob that can eat small slimes and magma cubes.
///
/// Wiki: <https://minecraft.wiki/w/Frog>
pub struct FrogEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub tongue_target_id: AtomicI32,
}

impl FrogEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        // frog: FrogPathNavigation, pathfinding malus (Frog constructor)
        *navigator = Navigator::frog();
        navigator.set_pathfinding_malus(PathType::Water, 4.0);
        navigator.set_pathfinding_malus(PathType::Trapdoor, -1.0);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) =
            Box::new(SmoothSwimmingMoveControl::new(85, 10, 0.02, 0.1, true));
        Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(FrogVariant::Temperate.id() as i32),
            tongue_target_id: AtomicI32::new(-1),
        })
    }

    // frog: getTargetFromBrain
    fn sync_target_from_brain(&self) {
        let target = self
            .mob_entity
            .with_brain(self, |brain| brain.get(types::ATTACK_TARGET).cloned());
        self.apply_mob_target(target);
    }

    // frog: IS_PREGNANT memory
    #[must_use]
    pub fn is_pregnant(&self) -> bool {
        self.mob_entity.with_brain(self, |brain| {
            brain.has_memory_value(types::IS_PREGNANT.id())
        })
    }

    pub fn set_pregnant(&self, pregnant: bool) {
        self.mob_entity.with_brain(self, |brain| {
            if pregnant {
                brain.set(types::IS_PREGNANT, ());
            } else {
                brain.erase(types::IS_PREGNANT.id());
            }
        });
    }

    // frog: Frog.canEat, small slimes and magma cubes only
    #[must_use]
    pub fn can_eat(target: &dyn EntityBase) -> bool {
        if !target
            .get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_FROG_FOOD)
        {
            return false;
        }
        // frog: AbstractCubeMob size must be 1
        if let Some(slime) = target
            .cast_any()
            .downcast_ref::<crate::entity::mob::slime::SlimeEntity>()
        {
            return slime.get_size() == 1;
        }
        if let Some(magma) = target
            .cast_any()
            .downcast_ref::<crate::entity::mob::magma_cube::MagmaCubeEntity>()
        {
            return magma.slime.get_size() == 1;
        }
        true
    }

    pub fn set_tongue_target(&self, target: Option<&dyn EntityBase>) {
        let id = target.map(|target| target.get_entity().entity_id);
        self.tongue_target_id
            .store(id.unwrap_or(-1), Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::frog::DATA_TONGUE_TARGET_ID,
            OptionalInt(id),
        );
    }

    #[must_use]
    pub fn tongue_target(&self) -> Option<Arc<dyn EntityBase>> {
        let id = self.tongue_target_id.load(Ordering::Relaxed);
        (id >= 0)
            .then(|| self.get_entity().world.load().get_entity_by_id(id))
            .flatten()
    }

    #[must_use]
    pub fn get_variant(&self) -> FrogVariant {
        FrogVariant::from_id(self.variant.load(Ordering::Relaxed) as u32)
    }

    pub fn set_variant(&self, variant: FrogVariant) {
        self.set_variant_id(variant.id() as i32);
    }

    pub fn set_variant_id(&self, raw_id: i32) {
        self.variant.store(raw_id, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::frog::VARIANT, VarInt(raw_id));
    }
}

impl AgeableMob for FrogEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn can_be_a_baby(&self) -> bool {
        false
    }
}

impl Animal for FrogEntity {
    // frog: eat sound (Frog.playEatingSound)
    fn eating_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        Some(pumpkin_data::sound::Sound::EntityFrogEat)
    }

    // frog: Frog.getBreedOffspring
    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let frog = Self::new(Entity::new(
            entity.world.load_full(),
            entity.pos.load(),
            &EntityType::FROG,
        ));
        ai::init_memories(&frog);
        frog
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_FROG_FOOD)
    }
}

impl Mob for FrogEntity {
    // frog: variant by biome, long jump cooldown (Frog.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let biome = world.get_biome(&self.get_entity().block_pos.load());
        let variant = if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_COLD_VARIANT_FROGS) {
            FrogVariant::Cold
        } else if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_WARM_VARIANT_FROGS) {
            FrogVariant::Warm
        } else {
            FrogVariant::Temperate
        };
        self.set_variant(variant);
        ai::init_memories(self);
        self.finalize_spawn_super(group_data)
    }

    // frog: pregnant instead of a baby (Frog.spawnChildFromBreeding)
    fn custom_breed(&self, mate: &dyn EntityBase) -> bool {
        let Some(mate_mob) = mate.get_mob() else {
            return false;
        };
        crate::entity::ai::goal::breed::BreedGoal::award_breeding(self, mate, None);
        self.mob_entity
            .breeding_cooldown
            .store(FROG_BREEDING_COOLDOWN, Ordering::Relaxed);
        mate.set_breeding_cooldown(FROG_BREEDING_COOLDOWN);
        self.mob_entity.reset_love_ticks();
        mate_mob.get_mob_entity().reset_love_ticks();
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if world.level_info.load().game_rules.mob_drops {
            crate::entity::experience_orb::ExperienceOrbEntity::spawn(
                &world,
                entity.pos.load(),
                rand::random_range(1..=7),
            );
        }
        self.set_pregnant(true);
        true
    }

    // frog: createAttributes, attack damage 10.0 (Frog.createAttributes)
    fn mob_attack_damage(&self, _base: f32) -> f32 {
        10.0
    }

    // frog: Frog.calculateFallDamage
    fn fall_damage_reduction(&self) -> i32 {
        FROG_FALL_DAMAGE_REDUCTION
    }

    fn mob_is_pushed_by_fluids(&self) -> bool {
        false
    }

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // frog: customServerAiStep (frogBrain, frogActivityUpdate)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
        self.sync_target_from_brain();
    }

    // frog: slow swim without gravity pull (Frog.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(
            living.movement_input.load(),
            f64::from(living.get_speed()),
        );
        living.make_move(caller);
        entity.velocity.store(entity.velocity.load() * 0.9);
    }

    // frog: Frog.getAmbientSound
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityFrogAmbient)
    }

    // frog: getHeadRotSpeed
    fn get_max_look_yaw_change(&self) -> f32 {
        FROG_HEAD_ROT_SPEED
    }

    // frog: getMaxHeadYRot
    fn get_max_head_rotation(&self) -> f32 {
        FROG_MAX_HEAD_Y_ROT
    }

    // frog: FrogLookControl.resetXRotOnTick, keeps the pitch while using the tongue
    fn look_resets_pitch(&self) -> bool {
        self.tongue_target_id.load(Ordering::Relaxed) < 0
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityFrogStep, 0.15, 1.0);
    }

    fn mob_get_component(
        &self,
        component: pumpkin_data::data_component::DataComponent,
    ) -> Option<Box<dyn pumpkin_data::data_component_impl::DataComponentImpl>> {
        use pumpkin_data::{data_component::DataComponent, data_component_impl::FrogVariantImpl};
        match component {
            DataComponent::FrogVariant => Some(Box::new(FrogVariantImpl {
                value: format!("minecraft:{}", self.get_variant().to_name()).into(),
            })),
            _ => None,
        }
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let variant_id = self.variant.load(Ordering::Relaxed) as u32;
        let variant_str =
            self.resolve_dynamic_entry_id("minecraft:frog_variant", variant_id, |_id| {
                self.get_variant().asset_id().to_string()
            });
        nbt.put_string("variant", variant_str);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(variant_str) = nbt.get_string("variant")
            && let Some(id) =
                self.resolve_dynamic_raw_id("minecraft:frog_variant", variant_str, |n| {
                    FrogVariant::from_name(n).map(|v| v.id())
                })
        {
            self.set_variant_id(id as i32);
        }
        // frog: older saves kept the pregnancy outside the brain
        if nbt.get_bool("IsPregnant") == Some(true) {
            self.set_pregnant(true);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_set_variant_name(&self, name: &str) {
        if let Some(id) = self.resolve_dynamic_raw_id("minecraft:frog_variant", name, |n| {
            FrogVariant::from_name(n).map(|v| v.id())
        }) {
            self.set_variant_id(id as i32);
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::frog::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::frog::VARIANT,
            VarInt(self.variant.load(Ordering::Relaxed)),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityFrogAmbient)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use pumpkin_data::environment_attribute::Activity;
    use pumpkin_util::math::vector3::Vector3;

    use super::*;
    use crate::entity::ai::test_support::world;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn frog_in(world: &Arc<World>) -> Arc<FrogEntity> {
        FrogEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::FROG,
        ))
    }

    #[tokio::test]
    async fn activities_follow_the_frog_ai_priority() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let frog = frog_in(&world);
        let victim = frog_in(&world);
        let active = |frog: &Arc<FrogEntity>| {
            frog.mob_entity.with_brain(frog.as_ref(), |brain| {
                ai::update_activity(brain);
                brain.get_active_non_core_activity()
            })
        };
        assert_eq!(active(&frog), Some(Activity::LongJump));
        frog.mob_entity.with_brain(frog.as_ref(), |brain| {
            brain.set(types::LONG_JUMP_COOLDOWN_TICKS, 100);
        });
        assert_eq!(active(&frog), Some(Activity::Idle));
        frog.mob_entity.with_brain(frog.as_ref(), |brain| {
            brain.set(types::IS_IN_WATER, ());
        });
        assert_eq!(active(&frog), Some(Activity::Swim));
        frog.mob_entity.with_brain(frog.as_ref(), |brain| {
            brain.erase(types::IS_IN_WATER.id());
        });
        frog.set_pregnant(true);
        assert!(frog.is_pregnant());
        assert_eq!(active(&frog), Some(Activity::LaySpawn));
        frog.mob_entity.with_brain(frog.as_ref(), |brain| {
            brain.set(types::ATTACK_TARGET, victim as Arc<dyn EntityBase>);
        });
        assert_eq!(active(&frog), Some(Activity::Tongue));
        world.level.shutdown().await;
        Ok(())
    }
}
