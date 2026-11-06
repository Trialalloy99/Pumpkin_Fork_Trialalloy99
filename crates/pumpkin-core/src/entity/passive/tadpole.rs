mod ai;

use std::sync::{
    Arc, PoisonError,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;

use super::{
    abstract_fish::{AbstractFish, FishData},
    water_animal::WaterAnimal,
};
use crate::entity::ai::brain::memory::PackedMemories;
use crate::entity::{
    Entity, EntityBase,
    ai::control::{
        smooth_swimming_look_control::SmoothSwimmingLookControl,
        smooth_swimming_move_control::SmoothSwimmingMoveControl,
    },
    ai::pathfinder::{Navigator, node::PathType},
    bucketable::Bucketable,
    mob::{
        Mob, MobEntity,
        conversion::convert_single,
        spawn::{SpawnGroupData, SpawnReason, finalize_spawn},
    },
    player::Player,
};
use crate::world::World;

// tadpole: constants (Tadpole, TadpoleAi)
const TICKS_TO_BE_FROG: i32 = 24000;
const AGE_LOCK_PARTICLE_COOLDOWN: i32 = 40;
const FEED_SPEED_UP_FACTOR: f32 = 0.1;

pub struct TadpoleEntity {
    pub mob_entity: MobEntity,
    fish_data: FishData,
    tadpole_age: AtomicI32,
    age_locked: AtomicBool,
    age_lock_particle_timer: AtomicI32,
}

impl TadpoleEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        *navigator = Navigator::water_bound(false);
        navigator.set_pathfinding_malus(PathType::Water, 0.0);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) =
            Box::new(SmoothSwimmingMoveControl::new(85, 10, 0.02, 0.1, true));
        *mob_entity
            .look_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Box::new(SmoothSwimmingLookControl::new(10));
        let tadpole = Self {
            mob_entity,
            fish_data: FishData::default(),
            tadpole_age: AtomicI32::new(0),
            age_locked: AtomicBool::new(false),
            age_lock_particle_timer: AtomicI32::new(0),
        };
        // tadpole: fromBucket() is always true (Tadpole.fromBucket)
        tadpole.fish_data.from_bucket.store(true, Ordering::Relaxed);
        Arc::new(tadpole)
    }

    fn is_food(item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_FROG_FOOD)
    }

    #[must_use]
    pub fn is_age_locked(&self) -> bool {
        self.age_locked.load(Ordering::Relaxed)
    }

    fn set_age_locked(&self, locked: bool) {
        self.age_locked.store(locked, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked_data::tadpole::AGE_LOCKED, locked);
    }

    // tadpole: Tadpole.setAge, becomes a frog at 24000
    fn set_tadpole_age(&self, age: i32) {
        self.tadpole_age.store(age, Ordering::Relaxed);
        if age >= TICKS_TO_BE_FROG {
            self.age_up();
        }
    }

    // tadpole: Tadpole.ageUp, converts to a frog (ConversionParams.single)
    fn age_up(&self) {
        let world = self.get_entity().world.load_full();
        let converted = convert_single(self, &EntityType::FROG, false, false, |frog| {
            if let Some(frog_mob) = frog.get_mob() {
                finalize_spawn(frog, &world, None, SpawnReason::Conversion);
                frog_mob
                    .get_mob_entity()
                    .persistence_required
                    .store(true, Ordering::Relaxed);
            }
        });
        if let Some(frog) = converted {
            world.play_sound_fine(
                Sound::EntityTadpoleGrowUp,
                SoundCategory::Neutral,
                &frog.get_entity().pos.load(),
                0.15,
                1.0,
            );
        }
    }

    // tadpole: Tadpole.feed, speeds up growing (AgeableMob.getSpeedUpSecondsWhenFeeding)
    fn feed(&self, player: &Arc<Player>, item_stack: &mut ItemStack) {
        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        let ticks_left = (TICKS_TO_BE_FROG - self.tadpole_age.load(Ordering::Relaxed)).max(0);
        let seconds = ((ticks_left / 20) as f32 * FEED_SPEED_UP_FACTOR) as i32;
        self.set_tadpole_age(self.tadpole_age.load(Ordering::Relaxed) + seconds * 20);
    }

    // tadpole: golden dandelion toggles the age lock (AgeableMob.canUseGoldenDandelion)
    fn use_golden_dandelion(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.item != &Item::GOLDEN_DANDELION
            || self.age_lock_particle_timer.load(Ordering::Relaxed) != 0
            || self
                .get_entity()
                .entity_type
                .has_tag(&tag::EntityType::MINECRAFT_CANNOT_BE_AGE_LOCKED)
        {
            return false;
        }
        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        let locked = !self.is_age_locked();
        self.set_age_locked(locked);
        self.tadpole_age.store(0, Ordering::Relaxed);
        self.age_lock_particle_timer
            .store(AGE_LOCK_PARTICLE_COOLDOWN, Ordering::Relaxed);
        if locked {
            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        let entity = self.get_entity();
        entity.world.load().play_sound(
            if locked {
                Sound::ItemGoldenDandelionUse
            } else {
                Sound::ItemGoldenDandelionUnuse
            },
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
        true
    }
}

impl Mob for TadpoleEntity {
    crate::fish_mob_hooks!();

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // tadpole: customServerAiStep (tadpoleBrain, tadpoleActivityUpdate)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.fish_write_nbt(nbt);
        nbt.put_int("Age", self.tadpole_age.load(Ordering::Relaxed));
        nbt.put_bool("AgeLocked", self.is_age_locked());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.fish_read_nbt(nbt);
        self.set_tadpole_age(nbt.get_int("Age").unwrap_or(0));
        self.set_age_locked(nbt.get_bool("AgeLocked").unwrap_or(false));
    }

    fn mob_init_data_tracker(&self) {
        self.set_from_bucket(self.is_from_bucket());
        self.get_entity()
            .set_synced_data(tracked_data::tadpole::AGE_LOCKED, self.is_age_locked());
    }

    // tadpole: grows one tick at a time while not age locked (Tadpole.aiStep)
    fn mob_ai_step(&self) {
        if !self.is_age_locked() {
            self.set_tadpole_age(self.tadpole_age.load(Ordering::Relaxed) + 1);
        }
        let timer = self.age_lock_particle_timer.load(Ordering::Relaxed);
        if timer > 0 {
            self.age_lock_particle_timer
                .store(timer - 1, Ordering::Relaxed);
        }
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.mob_entity.finalize_spawn_base();
        group_data
    }

    fn should_drop_experience(&self) -> bool {
        false
    }

    // tadpole: Tadpole.mobInteract, food speeds up growing, golden dandelion locks it
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if Self::is_food(item_stack) && !self.is_age_locked() {
            self.feed(player, item_stack);
            return true;
        }
        self.use_golden_dandelion(player, item_stack)
    }
}

impl WaterAnimal for TadpoleEntity {}

impl AbstractFish for TadpoleEntity {
    fn flop_sound(&self) -> Sound {
        Sound::EntityTadpoleFlop
    }

    fn fish_data(&self) -> &FishData {
        &self.fish_data
    }

    fn ambient_sound(&self) -> Option<Sound> {
        None
    }
}

impl crate::entity::custom_sound::CustomSound for TadpoleEntity {
    fn death_sound(&self) -> Option<Sound> {
        self.get_entity().entity_type.death_sound
    }

    fn hurt_sound(&self) -> Option<Sound> {
        self.get_entity().entity_type.hurt_sound
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Neutral
    }
}

impl Bucketable for TadpoleEntity {
    fn is_from_bucket(&self) -> bool {
        true
    }

    fn set_from_bucket(&self, _from_bucket: bool) {}

    fn bucket_item(&self) -> &'static Item {
        &Item::TADPOLE_BUCKET
    }

    fn pickup_sound(&self) -> Sound {
        Sound::ItemBucketFillTadpole
    }

    // tadpole: Tadpole.saveToBucketTag
    fn save_to_bucket_tag(&self, bucket: &mut ItemStack) {
        self.save_default_data_to_bucket_tag(bucket);
        let mut nbt = bucket
            .get_data_component::<pumpkin_data::data_component_impl::BucketEntityDataImpl>()
            .and_then(|data| data.nbt.clone())
            .unwrap_or_default();
        nbt.put_int("Age", self.tadpole_age.load(Ordering::Relaxed));
        nbt.put_bool("AgeLocked", self.is_age_locked());
        bucket.set_data_component(pumpkin_data::data_component_impl::BucketEntityDataImpl {
            nbt: Some(nbt),
        });
    }

    fn load_from_bucket_tag(&self, nbt: &NbtCompound) {
        let entity = self.get_entity();
        let mob = self.get_mob_entity();
        if let Some(value) = nbt.get_bool("NoAI") {
            mob.set_no_ai(value);
        }
        if let Some(value) = nbt.get_bool("Silent") {
            entity.set_silent(value);
        }
        if let Some(health) = nbt.get_float("Health") {
            mob.living_entity.set_health(health);
        }
        if let Some(age) = nbt.get_int("Age") {
            self.set_tadpole_age(age);
        }
        self.set_age_locked(nbt.get_bool("AgeLocked").unwrap_or(false));
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

    #[tokio::test]
    async fn tadpole_brain_idles() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let tadpole = TadpoleEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::TADPOLE,
        ));
        let active = tadpole.mob_entity.with_brain(tadpole.as_ref(), |brain| {
            ai::update_activity(brain);
            brain.get_active_non_core_activity()
        });
        assert_eq!(active, Some(Activity::Idle));
        world.level.shutdown().await;
        Ok(())
    }
}
