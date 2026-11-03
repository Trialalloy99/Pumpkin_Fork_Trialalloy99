mod ai;

use std::sync::{
    Arc, PoisonError,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use rand::RngExt;

use crate::entity::ai::brain::memory::{PackedMemories, types};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::control::{
        Control, LookControlTrait, MoveControlTrait,
        smooth_swimming_look_control::SmoothSwimmingLookControl,
        smooth_swimming_move_control::SmoothSwimmingMoveControl,
    },
    ai::pathfinder::{Navigator, node::PathType},
    bucketable::Bucketable,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

// axolotl: constants (Axolotl)
const AXOLOTL_TOTAL_AIR_SUPPLY: i32 = 6000;
const REHYDRATE_AIR_SUPPLY: i32 = 1800;
const RARE_VARIANT_CHANCE: i32 = 1200;
const REGEN_BUFF_MAX_DURATION: i32 = 2400;
const REGEN_BUFF_BASE_DURATION: i32 = 100;

fn axolotl(mob: &dyn Mob) -> Option<&AxolotlEntity> {
    mob.cast_any().downcast_ref::<AxolotlEntity>()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum AxolotlVariant {
    #[default]
    Lucy = 0,
    Wild = 1,
    Gold = 2,
    Cyan = 3,
    Blue = 4,
}

impl AxolotlVariant {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::Wild,
            2 => Self::Gold,
            3 => Self::Cyan,
            4 => Self::Blue,
            _ => Self::Lucy,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    // axolotl: Variant.getCommonSpawnVariant / getRareSpawnVariant
    #[must_use]
    pub fn spawn_variant(common: bool) -> Self {
        if common {
            match rand::random_range(0..4) {
                0 => Self::Lucy,
                1 => Self::Wild,
                2 => Self::Gold,
                _ => Self::Cyan,
            }
        } else {
            Self::Blue
        }
    }
}

pub struct AxolotlEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub playing_dead: AtomicBool,
    pub from_bucket: AtomicBool,
    damaged_by_entity: AtomicBool,
}

impl AxolotlEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        *navigator = Navigator::amphibious(false);
        navigator.set_pathfinding_malus(PathType::Water, 0.0);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Box::new(AxolotlMoveControl {
            inner: SmoothSwimmingMoveControl::new(85, 10, 0.1, 0.5, false),
        });
        *mob_entity
            .look_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Box::new(AxolotlLookControl {
            inner: SmoothSwimmingLookControl::new(20),
        });
        mob_entity
            .living_entity
            .entity
            .air_supply
            .store(AXOLOTL_TOTAL_AIR_SUPPLY, Ordering::Relaxed);
        Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(AxolotlVariant::Lucy.id()),
            playing_dead: AtomicBool::new(false),
            from_bucket: AtomicBool::new(false),
            damaged_by_entity: AtomicBool::new(false),
        })
    }

    // axolotl: getTargetFromBrain
    fn sync_target_from_brain(&self) {
        let target = self
            .mob_entity
            .with_brain(self, |brain| brain.get(types::ATTACK_TARGET).cloned());
        self.apply_mob_target(target);
    }

    #[must_use]
    pub fn get_variant(&self) -> AxolotlVariant {
        AxolotlVariant::from_id(self.variant.load(Ordering::Relaxed))
    }

    pub fn set_variant(&self, variant: AxolotlVariant) {
        self.variant.store(variant.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::DATA_VARIANT,
            VarInt(variant.id()),
        );
    }

    #[must_use]
    pub fn is_playing_dead(&self) -> bool {
        self.playing_dead.load(Ordering::Relaxed)
    }

    pub fn set_playing_dead(&self, playing_dead: bool) {
        self.playing_dead.store(playing_dead, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::DATA_PLAYING_DEAD,
            playing_dead,
        );
    }

    // axolotl: Axolotl.rehydrate, splash water bottle
    pub fn rehydrate(&self) {
        let entity = self.get_entity();
        let air = entity.air_supply.load(Ordering::Relaxed) + REHYDRATE_AIR_SUPPLY;
        entity.set_air_supply(air.min(AXOLOTL_TOTAL_AIR_SUPPLY));
    }

    // axolotl: Axolotl.applySupportingEffects, regeneration for the killer player
    pub fn apply_supporting_effects(&self, player: &Arc<Player>) {
        let living = &player.living_entity;
        let previous = living
            .get_effect(&StatusEffect::REGENERATION)
            .map_or(0, |effect| effect.duration);
        if previous < REGEN_BUFF_MAX_DURATION - 1 {
            living.add_effect(Effect {
                effect_type: &StatusEffect::REGENERATION,
                duration: (REGEN_BUFF_BASE_DURATION + previous).min(REGEN_BUFF_MAX_DURATION),
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
        }
        living.remove_effect(&StatusEffect::MINING_FATIGUE);
    }
}

impl Bucketable for AxolotlEntity {
    fn is_from_bucket(&self) -> bool {
        self.from_bucket.load(Ordering::Relaxed)
    }

    fn set_from_bucket(&self, from_bucket: bool) {
        self.from_bucket.store(from_bucket, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::FROM_BUCKET,
            from_bucket,
        );
    }

    fn bucket_item(&self) -> &'static Item {
        &Item::AXOLOTL_BUCKET
    }

    fn pickup_sound(&self) -> Sound {
        Sound::ItemBucketFillAxolotl
    }

    // axolotl: Axolotl.saveToBucketTag, variant, age and hunting cooldown
    fn save_to_bucket_tag(&self, bucket: &mut ItemStack) {
        self.save_default_data_to_bucket_tag(bucket);
        let mut nbt = bucket
            .get_data_component::<pumpkin_data::data_component_impl::BucketEntityDataImpl>()
            .and_then(|data| data.nbt.clone())
            .unwrap_or_default();
        nbt.put_int("Variant", self.get_variant().id());
        nbt.put_int("Age", self.get_age());
        nbt.put_bool("AgeLocked", self.is_age_locked());
        let cooldown = self.mob_entity.with_brain(self, |brain| {
            brain.time_until_expiry(types::HAS_HUNTING_COOLDOWN.id())
        });
        if let Some(cooldown) = cooldown {
            nbt.put_long("HuntingCooldown", cooldown);
        }
        bucket.set_data_component(pumpkin_data::data_component_impl::BucketEntityDataImpl {
            nbt: Some(nbt),
        });
    }

    fn load_from_bucket_tag(&self, nbt: &NbtCompound) {
        if let Some(variant) = nbt.get_int("Variant") {
            self.set_variant(AxolotlVariant::from_id(variant));
        }
        self.set_age(nbt.get_int("Age").unwrap_or(0));
        self.set_age_locked(nbt.get_bool("AgeLocked").unwrap_or(false));
        let cooldown = nbt.get_long("HuntingCooldown");
        self.mob_entity.with_brain(self, |brain| match cooldown {
            Some(cooldown) => brain.set_with_expiry(types::HAS_HUNTING_COOLDOWN, true, cooldown),
            None => brain.erase(types::HAS_HUNTING_COOLDOWN.id()),
        });
        let entity = self.get_entity();
        let mob = self.get_mob_entity();
        if nbt.get_bool("NoAI") == Some(true) {
            mob.set_no_ai(true);
        }
        if nbt.get_bool("Silent") == Some(true) {
            entity.set_silent(true);
        }
        if nbt.get_bool("PersistenceRequired") == Some(true) {
            mob.persistence_required.store(true, Ordering::Relaxed);
        }
        if let Some(health) = nbt.get_float("Health") {
            mob.living_entity.set_health(health);
        }
    }
}

impl AgeableMob for AxolotlEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for AxolotlEntity {
    // axolotl: Axolotl.getWalkTargetValue
    fn animal_walk_target_value(&self, _pos: &pumpkin_util::math::position::BlockPos) -> f32 {
        0.0
    }

    // axolotl: offspring coat, rare 1 in 1200 (Axolotl.getBreedOffspring)
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let baby = Self::new(Entity::new(
            entity.world.load_full(),
            entity.pos.load(),
            &EntityType::AXOLOTL,
        ));
        let variant = if rand::random_range(0..RARE_VARIANT_CHANCE) == 0 {
            AxolotlVariant::spawn_variant(false)
        } else if rand::random::<bool>() {
            self.get_variant()
        } else {
            partner
                .cast_any()
                .downcast_ref::<Self>()
                .map_or_else(|| self.get_variant(), Self::get_variant)
        };
        baby.set_variant(variant);
        baby.mob_entity
            .persistence_required
            .store(true, Ordering::Relaxed);
        baby
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_AXOLOTL_FOOD)
    }
}

// axolotl: Axolotl.AxolotlMoveControl, frozen while playing dead
struct AxolotlMoveControl {
    inner: SmoothSwimmingMoveControl,
}

impl Control for AxolotlMoveControl {}

impl MoveControlTrait for AxolotlMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if !axolotl(mob).is_some_and(AxolotlEntity::is_playing_dead) {
            self.inner.tick(mob);
        }
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.inner.set_wanted_position(x, y, z, speed_modifier);
    }

    fn strafe(&mut self, forwards: f32, right: f32) {
        self.inner.strafe(forwards, right);
    }

    fn has_wanted(&self) -> bool {
        self.inner.has_wanted()
    }

    fn speed_modifier(&self) -> f64 {
        self.inner.speed_modifier
    }
}

// axolotl: Axolotl.AxolotlLookControl, frozen while playing dead
struct AxolotlLookControl {
    inner: SmoothSwimmingLookControl,
}

impl Control for AxolotlLookControl {}

impl LookControlTrait for AxolotlLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if !axolotl(mob).is_some_and(AxolotlEntity::is_playing_dead) {
            self.inner.tick(mob);
        }
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        max_yaw_change: f32,
        max_pitch_change: f32,
    ) {
        self.inner
            .look_at_with_range(x, y, z, max_yaw_change, max_pitch_change);
    }
}

impl Mob for AxolotlEntity {
    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_bucketable(&self) -> Option<&dyn Bucketable> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_int("Variant", self.get_variant().id());
        nbt.put_bool("FromBucket", self.is_from_bucket());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(variant) = nbt.get_int("Variant") {
            self.set_variant(AxolotlVariant::from_id(variant));
        }
        if let Some(from_bucket) = nbt.get_bool("FromBucket") {
            self.set_from_bucket(from_bucket);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // axolotl: group coats, babies from the third on (Axolotl.finalizeSpawn)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if self.get_mob_entity().spawn_reason() == crate::entity::mob::spawn::SpawnReason::Bucket {
            return group_data;
        }
        let (variants, group_size) = match group_data {
            Some(SpawnGroupData::Axolotl {
                variants,
                group_size,
            }) => (variants, group_size),
            _ => (
                [
                    AxolotlVariant::spawn_variant(true).id(),
                    AxolotlVariant::spawn_variant(true).id(),
                ],
                0,
            ),
        };
        self.set_variant(AxolotlVariant::from_id(
            variants[rand::random_range(0..variants.len())],
        ));
        if group_size >= 2 {
            self.set_age(-24000);
        }
        self.finalize_spawn_super(None);
        Some(SpawnGroupData::Axolotl {
            variants,
            group_size: group_size + 1,
        })
    }

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // axolotl: baseTick air supply (handleAirSupply) and customServerAiStep
    fn custom_server_ai_step(&self, caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let living = &self.mob_entity.living_entity;
        let air = entity.air_supply.load(Ordering::Relaxed);
        if living.is_alive() && !entity.is_in_water_or_rain() {
            entity.set_air_supply(air - 1);
            if living.should_take_drowning_damage() {
                entity.set_air_supply(0);
                caller.damage(caller, 2.0, DamageType::DRY_OUT);
            }
        } else {
            entity.set_air_supply(AXOLOTL_TOTAL_AIR_SUPPLY);
        }

        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
        if !self.mob_entity.is_no_ai() {
            let playing_dead = self
                .mob_entity
                .with_brain(self, |brain| brain.get(types::PLAY_DEAD_TICKS).copied())
                .is_some_and(|ticks| ticks > 0);
            self.set_playing_dead(playing_dead);
        }
        self.sync_target_from_brain();
    }

    // axolotl: sinks into play dead when hurt (Axolotl.hurtServer)
    fn pre_damage_with_context(
        &self,
        _damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.damaged_by_entity
            .store(source.is_some() || cause.is_some(), Ordering::Relaxed);
        true
    }

    // axolotl: hurtServer, play dead memory for 200 ticks
    fn modify_incoming_damage(&self, amount: f32, _damage_type: DamageType) -> f32 {
        let living = &self.mob_entity.living_entity;
        let health = living.health.load();
        let plays_dead = !self.mob_entity.is_no_ai()
            && {
                let mut rng = self.get_random();
                rng.random_range(0..3) == 0
                    && ((rng.random_range(0..3) as f32) < amount
                        || health / living.get_max_health() < 0.5)
            }
            && amount < health
            && self.get_entity().is_in_water()
            && self.damaged_by_entity.load(Ordering::Relaxed)
            && !self.is_playing_dead();
        if plays_dead {
            self.mob_entity.with_brain(self, |brain| {
                brain.set(types::PLAY_DEAD_TICKS, ai::TOTAL_PLAYDEAD_TIME);
            });
        }
        amount
    }

    // axolotl: getMaxHeadXRot
    fn get_max_look_pitch_change(&self) -> f32 {
        1.0
    }

    // axolotl: getMaxHeadYRot
    fn get_max_head_rotation(&self) -> f32 {
        1.0
    }

    fn mob_is_pushed_by_fluids(&self) -> bool {
        false
    }

    // axolotl: cannot be targeted while playing dead (Axolotl.canBeSeenAsEnemy)
    fn mob_can_be_seen_as_enemy(&self) -> bool {
        !self.is_playing_dead()
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        !self.is_from_bucket() && self.get_entity().custom_name.load().is_none()
    }

    fn requires_custom_persistence(&self) -> bool {
        self.is_from_bucket()
    }

    // axolotl: Axolotl.getAmbientSound
    fn get_ambient_sound(&self) -> Option<Sound> {
        if self.is_playing_dead() {
            return None;
        }
        Some(if self.get_entity().is_in_water() {
            Sound::EntityAxolotlIdleWater
        } else {
            Sound::EntityAxolotlIdleAir
        })
    }

    fn on_attack(&self, _target: &dyn EntityBase) {
        let entity = self.get_entity();
        entity.world.load().play_sound_fine(
            Sound::EntityAxolotlAttack,
            SoundCategory::Neutral,
            &entity.pos.load(),
            1.0,
            1.0,
        );
    }

    // axolotl: slow swim without gravity pull (Axolotl.travelInWater)
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

    // axolotl: BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.375,
            height: 0.21,
            eye_height: 0.093_75,
        })
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::axolotl::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::DATA_VARIANT,
            VarInt(self.get_variant().id()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::DATA_PLAYING_DEAD,
            self.is_playing_dead(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::axolotl::FROM_BUCKET,
            self.is_from_bucket(),
        );
    }

    // axolotl: bucket pickup, tropical fish bucket returns a water bucket (Axolotl.usePlayerItem)
    fn mob_interact_with_hand(
        &self,
        player: &Arc<Player>,
        stack: &mut ItemStack,
        _hand: pumpkin_util::Hand,
    ) -> Option<bool> {
        crate::entity::bucketable::bucket_mob_pickup(player, stack, self)
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.item == &Item::TROPICAL_FISH_BUCKET
            && self.is_food(item_stack)
            && self.get_age() == 0
            && self.mob_entity.is_breeding_ready()
            && !self.mob_entity.is_in_love()
        {
            let result = self.animal_interact(player, item_stack, Sound::EntityAxolotlIdleAir);
            if result && !player.is_creative() {
                crate::item::items::bucket::give_filled_mob_bucket(
                    &mut ItemStack::new(1, &Item::TROPICAL_FISH_BUCKET),
                    player,
                    ItemStack::new(1, &Item::WATER_BUCKET),
                );
            }
            return result;
        }
        self.animal_interact(player, item_stack, Sound::EntityAxolotlIdleAir)
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

    fn axolotl_in(world: &Arc<World>) -> Arc<AxolotlEntity> {
        AxolotlEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::AXOLOTL,
        ))
    }

    #[tokio::test]
    async fn play_dead_memory_takes_over_and_the_fight_leaves_a_hunting_cooldown() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let axolotl = axolotl_in(&world);

        axolotl.mob_entity.with_brain(axolotl.as_ref(), |brain| {
            brain.set(types::ATTACK_TARGET, axolotl.clone() as Arc<dyn EntityBase>);
            ai::update_activity(brain);
            assert_eq!(brain.get_active_non_core_activity(), Some(Activity::Fight));
            brain.erase(types::ATTACK_TARGET.id());
            ai::update_activity(brain);
            assert_eq!(brain.get_active_non_core_activity(), Some(Activity::Idle));
            assert!(brain.has_memory_value(types::HAS_HUNTING_COOLDOWN.id()));
            assert!(
                brain
                    .time_until_expiry(types::HAS_HUNTING_COOLDOWN.id())
                    .is_some_and(|ticks| ticks > 2000)
            );

            brain.set(types::PLAY_DEAD_TICKS, 200);
            ai::update_activity(brain);
            assert_eq!(
                brain.get_active_non_core_activity(),
                Some(Activity::PlayDead)
            );
            // Fight cannot interrupt playing dead
            brain.set(types::ATTACK_TARGET, axolotl.clone() as Arc<dyn EntityBase>);
            ai::update_activity(brain);
            assert_eq!(
                brain.get_active_non_core_activity(),
                Some(Activity::PlayDead)
            );
        });
        axolotl.custom_server_ai_step(axolotl.as_ref());
        assert!(axolotl.is_playing_dead());
        world.level.shutdown().await;
        Ok(())
    }
}
