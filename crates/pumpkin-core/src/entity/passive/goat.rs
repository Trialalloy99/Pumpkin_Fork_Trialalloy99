mod ai;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::item::items::bucket::fill_bucket_stack;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::InstrumentImpl;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::brain::memory::PackedMemories;
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::custom_sound::CustomSound;
use crate::entity::item::ItemEntity;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const GOAT_FALL_DAMAGE_REDUCTION: i32 = 10;
const GOAT_SCREAMING_CHANCE: f64 = 0.02;
const UNIHORN_CHANCE: f32 = 0.1;
const MAX_HEAD_ROTATION: f32 = 15.0;
const ADULT_ATTACK_DAMAGE: f64 = 2.0;
const BABY_ATTACK_DAMAGE: f64 = 1.0;

pub struct GoatEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub is_screaming: AtomicBool,
    pub has_left_horn: AtomicBool,
    pub has_right_horn: AtomicBool,
}

impl GoatEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        // goat: setCanFloat(true), no powder snow
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator.set_can_float(true);
        navigator.set_pathfinding_malus(PathType::PowderSnow, -1.0);
        // vanilla ON_TOP_OF_POWDER_SNOW, id 6 is DangerPowderSnow here
        navigator.set_pathfinding_malus(PathType::DangerPowderSnow, -1.0);
        let goat = Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            is_screaming: AtomicBool::new(false),
            has_left_horn: AtomicBool::new(true),
            has_right_horn: AtomicBool::new(true),
        });
        goat.update_attack_damage();
        goat
    }

    #[must_use]
    pub fn is_screaming(&self) -> bool {
        self.is_screaming.load(Ordering::Relaxed)
    }

    pub fn set_screaming(&self, screaming: bool) {
        self.is_screaming.store(screaming, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_IS_SCREAMING_GOAT,
            screaming,
        );
    }

    #[must_use]
    pub fn has_left_horn(&self) -> bool {
        self.has_left_horn.load(Ordering::Relaxed)
    }

    pub fn set_has_left_horn(&self, has_horn: bool) {
        self.has_left_horn.store(has_horn, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_LEFT_HORN,
            has_horn,
        );
    }

    #[must_use]
    pub fn has_right_horn(&self) -> bool {
        self.has_right_horn.load(Ordering::Relaxed)
    }

    pub fn set_has_right_horn(&self, has_horn: bool) {
        self.has_right_horn.store(has_horn, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_RIGHT_HORN,
            has_horn,
        );
    }

    // goat: ageBoundaryReached, attack damage by age
    fn update_attack_damage(&self) {
        let mut attributes = self
            .mob_entity
            .living_entity
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(damage) = attributes.get_mut(&Attributes::ATTACK_DAMAGE.id) {
            damage.base_value = if self.is_baby() {
                BABY_ATTACK_DAMAGE
            } else {
                ADULT_ATTACK_DAMAGE
            };
            damage.dirty.store(true, Ordering::Relaxed);
        }
    }

    // goat: createHorn, a horn with a random instrument of the goat kind
    fn create_horn(&self) -> ItemStack {
        let tag = if self.is_screaming() {
            &pumpkin_data::tag::Instrument::MINECRAFT_SCREAMING_GOAT_HORNS
        } else {
            &pumpkin_data::tag::Instrument::MINECRAFT_REGULAR_GOAT_HORNS
        };
        let mut stack = ItemStack::new(1, &Item::GOAT_HORN);
        if !tag.0.is_empty() {
            let instrument = tag.0[self.get_random().random_range(0..tag.0.len())];
            stack.set_data_component(InstrumentImpl {
                instrument: std::borrow::Cow::Borrowed(instrument),
            });
        }
        stack
    }

    // goat: dropHorn
    pub fn drop_horn(&self) -> bool {
        if self.is_baby() {
            return false;
        }
        let has_left = self.has_left_horn();
        let has_right = self.has_right_horn();
        if !has_left && !has_right {
            return false;
        }
        let drop_left = if !has_left {
            false
        } else if !has_right {
            true
        } else {
            self.get_random().random::<bool>()
        };
        if drop_left {
            self.set_has_left_horn(false);
        } else {
            self.set_has_right_horn(false);
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        let velocity = {
            let mut rng = self.get_random();
            Vector3::new(
                rng.random_range(-0.2..0.2),
                rng.random_range(0.3..0.7),
                rng.random_range(-0.2..0.2),
            )
        };
        let horn = ItemEntity::new_with_velocity(
            Entity::new(world.clone(), entity.pos.load(), &EntityType::ITEM),
            self.create_horn(),
            velocity,
            10,
        );
        world.spawn_entity(Arc::new(horn));
        true
    }
}

impl AgeableMob for GoatEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    // goat: ageBoundaryReached
    fn age_boundary_reached(&self) {
        self.update_attack_damage();
    }
}

impl Animal for GoatEntity {
    // goat: (screaming) eat sound (Goat.playEatingSound)
    fn eating_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingEat
        } else {
            Sound::EntityGoatEat
        })
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_GOAT_FOOD)
    }

    // goat: getBreedOffspring
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby = from_type(
            &EntityType::GOAT,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        if let Some(goat) = baby.cast_any().downcast_ref::<Self>() {
            ai::init_memories(goat);
            let mut rng = self.get_random();
            let selected_screaming = if rng.random::<bool>() {
                self.is_screaming()
            } else {
                partner
                    .cast_any()
                    .downcast_ref::<Self>()
                    .is_some_and(Self::is_screaming)
            };
            goat.set_screaming(selected_screaming || rng.random::<f64>() < GOAT_SCREAMING_CHANCE);
        }
        baby
    }
}

impl CustomSound for GoatEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingDeath
        } else {
            Sound::EntityGoatDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingHurt
        } else {
            Sound::EntityGoatHurt
        })
    }
}

impl Mob for GoatEntity {
    // goat: calculateFallDamage - 10
    fn fall_damage_reduction(&self) -> i32 {
        GOAT_FALL_DAMAGE_REDUCTION
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingAmbient
        } else {
            Sound::EntityGoatAmbient
        })
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityGoatStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    // goat: getMaxHeadYRot
    fn get_max_head_rotation(&self) -> f32 {
        MAX_HEAD_ROTATION
    }

    // goat: BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<EntityDimensions> {
        Some(EntityDimensions {
            width: 0.45,
            height: 0.65,
            eye_height: 0.593_75,
        })
    }

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // goat: customServerAiStep
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
    }

    // goat: finalizeSpawn
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        ai::init_memories(self);
        let (screaming, remove_horn, remove_left) = {
            let mut rng = self.get_random();
            (
                rng.random::<f64>() < GOAT_SCREAMING_CHANCE,
                rng.random::<f32>() < UNIHORN_CHANCE,
                rng.random::<bool>(),
            )
        };
        self.set_screaming(screaming);
        self.update_attack_damage();
        if !self.is_baby() && remove_horn {
            if remove_left {
                self.set_has_left_horn(false);
            } else {
                self.set_has_right_horn(false);
            }
        }
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("IsScreamingGoat", self.is_screaming());
        nbt.put_bool("HasLeftHorn", self.has_left_horn());
        nbt.put_bool("HasRightHorn", self.has_right_horn());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.set_screaming(nbt.get_bool("IsScreamingGoat").unwrap_or(false));
        self.set_has_left_horn(nbt.get_bool("HasLeftHorn").unwrap_or(true));
        self.set_has_right_horn(nbt.get_bool("HasRightHorn").unwrap_or(true));
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::goat::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_IS_SCREAMING_GOAT,
            self.is_screaming(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_LEFT_HORN,
            self.has_left_horn(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_RIGHT_HORN,
            self.has_right_horn(),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        // goat: milking with bucket (Goat.mobInteract)
        if item_stack.get_item() == &Item::BUCKET && !self.is_baby() {
            let entity = self.get_entity();
            let sound = if self.is_screaming() {
                Sound::EntityGoatScreamingMilk
            } else {
                Sound::EntityGoatMilk
            };
            entity
                .world
                .load()
                .play_sound(sound, SoundCategory::Neutral, &entity.pos.load());
            fill_bucket_stack(item_stack, player, &Item::MILK_BUCKET);
            return true;
        }
        let ambient_sound = if self.is_screaming() {
            Sound::EntityGoatScreamingAmbient
        } else {
            Sound::EntityGoatAmbient
        };
        self.animal_interact(player, item_stack, ambient_sound)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use pumpkin_data::environment_attribute::Activity;

    use super::*;
    use crate::entity::ai::test_support::world;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn goat_in(world: &Arc<World>) -> Arc<GoatEntity> {
        GoatEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::GOAT,
        ))
    }

    #[tokio::test]
    async fn cooldowns_keep_the_goat_idle_and_expiring_ones_open_the_ram_activity() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let goat = goat_in(&world);
        ai::init_memories(&goat);
        for _ in 0..3 {
            goat.custom_server_ai_step(goat.as_ref());
        }
        let idle = goat.mob_entity.with_brain(goat.as_ref(), |brain| {
            brain.activity_requirements_are_met(Activity::Idle)
                && !brain.activity_requirements_are_met(Activity::Ram)
        });
        assert!(idle);
        goat.mob_entity.with_brain(goat.as_ref(), |brain| {
            brain.erase(crate::entity::ai::brain::memory::types::RAM_COOLDOWN_TICKS.id());
        });
        let rams = goat.mob_entity.with_brain(goat.as_ref(), |brain| {
            brain.activity_requirements_are_met(Activity::Ram)
        });
        assert!(rams);
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_horn_drops_once_per_horn_and_never_from_a_baby() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let goat = goat_in(&world);
        assert!(goat.drop_horn());
        assert!(goat.drop_horn());
        assert!(!goat.drop_horn());
        let baby = goat_in(&world);
        baby.set_age(-24_000);
        assert!(!baby.drop_horn());
        world.level.shutdown().await;
        Ok(())
    }
}
