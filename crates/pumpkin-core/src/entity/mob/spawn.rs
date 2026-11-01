//! Vanilla `Mob.finalizeSpawn`: the step run on freshly created mobs before they enter the world.

use std::sync::Arc;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::effect::StatusEffect;
use rand::RngExt;

use crate::entity::EntityBase;
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::mob::MobEntity;
use crate::entity::passive::fox::FoxVariant;
use crate::world::World;

const RANDOM_SPAWN_BONUS_ID: &str = "minecraft:random_spawn_bonus";

/// State shared by every mob of one spawn group (vanilla `SpawnGroupData`).
pub enum SpawnGroupData {
    Ageable {
        group_size: u32,
        should_spawn_baby: bool,
        baby_spawn_chance: f32,
    },
    School(std::sync::Weak<dyn EntityBase>),
    TropicalFish {
        leader: std::sync::Weak<dyn EntityBase>,
        variant: i32,
    },
    /// The effect the first spider of a group rolled on hard difficulty.
    SpiderEffects(Option<&'static StatusEffect>),
    /// Vanilla `FoxGroupData`: the group's variant and how many joined so far.
    Fox {
        variant: FoxVariant,
        size: u32,
    },
    /// Vanilla `Axolotl.AxolotlGroupData`: two common coats for the group, babies from the third on.
    Axolotl {
        variants: [i32; 2],
        group_size: u32,
    },
    /// Vanilla `ZombieGroupData` (and `HuskGroupData`): baby group, jockey allowed, camel husk tried.
    Zombie {
        baby: bool,
        can_spawn_jockey: bool,
        tried_camel_husk: bool,
    },
    /// Vanilla `WolfPackData`: the variant every wolf of the pack shares.
    WolfPack(pumpkin_data::wolf_variant::WolfVariant),
    /// Vanilla `RabbitGroupData`: shared variant, babies after the first (chance 1.0).
    Rabbit {
        variant: i32,
        group_size: u32,
    },
}

impl MobEntity {
    /// Vanilla `Mob.finalizeSpawn`: random follow range bonus and left-handedness.
    pub fn finalize_spawn_base(&self) {
        let mut rng = rand::rng();
        {
            let mut attributes = self
                .living_entity
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(follow_range) = attributes.get_mut(&Attributes::FOLLOW_RANGE.id)
                && !follow_range
                    .modifiers
                    .iter()
                    .any(|modifier| modifier.id == RANDOM_SPAWN_BONUS_ID)
            {
                follow_range.add_or_replace_permanent_modifier(Modifier {
                    id: RANDOM_SPAWN_BONUS_ID.to_string(),
                    // Triangle distribution around 0.
                    amount: 0.114_85 * f64::from(rng.random::<f32>() - rng.random::<f32>()),
                    operation: ModifierOperation::MultiplyBase,
                });
            }
        }
        self.set_left_handed(rng.random::<f32>() < 0.05);
    }

    /// Queues a mob to be added and mounted onto this one when it enters the world.
    pub fn add_pending_rider(&self, rider: Arc<dyn EntityBase>) {
        self.pending_riders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(rider);
    }

    // mob spawn: reason passed to the last finalize_spawn (EntitySpawnReason)
    #[must_use]
    pub fn spawn_reason(&self) -> SpawnReason {
        SpawnReason::from_u8(self.spawn_reason.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Queues a vehicle that this mob mounts when it enters the world; `spawn` adds it too.
    pub fn set_pending_vehicle(&self, vehicle: Arc<dyn EntityBase>, spawn: bool) {
        *self
            .pending_vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((vehicle, spawn));
    }

    pub(crate) fn take_pending_vehicle(&self) -> Option<(Arc<dyn EntityBase>, bool)> {
        self.pending_vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn take_pending_riders(&self) -> Vec<Arc<dyn EntityBase>> {
        std::mem::take(
            &mut *self
                .pending_riders
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Vanilla `EntitySpawnReason`: why a mob was created, read by some `finalizeSpawn` overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SpawnReason {
    Natural,
    ChunkGeneration,
    Spawner,
    Structure,
    Breeding,
    MobSummoned,
    Jockey,
    Event,
    Conversion,
    Reinforcement,
    Triggered,
    Bucket,
    SpawnItemUse,
    #[default]
    Command,
    Dispenser,
    Patrol,
    TrialSpawner,
    Load,
    DimensionTravel,
}

impl SpawnReason {
    const ALL: [Self; 19] = [
        Self::Natural,
        Self::ChunkGeneration,
        Self::Spawner,
        Self::Structure,
        Self::Breeding,
        Self::MobSummoned,
        Self::Jockey,
        Self::Event,
        Self::Conversion,
        Self::Reinforcement,
        Self::Triggered,
        Self::Bucket,
        Self::SpawnItemUse,
        Self::Command,
        Self::Dispenser,
        Self::Patrol,
        Self::TrialSpawner,
        Self::Load,
        Self::DimensionTravel,
    ];

    #[must_use]
    pub fn from_u8(value: u8) -> Self {
        Self::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or_default()
    }
}

/// Finalizes `entity` if it is a mob, and returns the group data for the next mob of the group.
pub fn finalize_spawn(
    entity: &Arc<dyn EntityBase>,
    world: &Arc<World>,
    group_data: Option<SpawnGroupData>,
    reason: SpawnReason,
) -> Option<SpawnGroupData> {
    match entity.get_mob() {
        Some(mob) => {
            mob.get_mob_entity()
                .spawn_reason
                .store(reason as u8, std::sync::atomic::Ordering::Relaxed);
            mob.finalize_spawn(world, group_data)
        }
        None => group_data,
    }
}

impl MobEntity {
    /// Baby state kept only as a negative age and the shared baby flag (hoglins, zoglins and
    /// zombified piglins, which are not `AgeableMob`s here).
    pub fn set_baby_by_age(&self) {
        let entity = &self.living_entity.entity;
        entity
            .age
            .store(-24000, std::sync::atomic::Ordering::Relaxed);
        entity.set_synced_data(pumpkin_data::tracked_data::ageable_mob::DATA_BABY_ID, true);
    }

    /// Baby state kept as a mob's own flag and synced key (vanilla zombies and piglins), so it
    /// never ages up.
    pub fn set_baby_flag(
        &self,
        flag: &std::sync::atomic::AtomicBool,
        tracked: pumpkin_data::tracked_data::TrackedData,
        baby: bool,
    ) {
        flag.store(baby, std::sync::atomic::Ordering::Relaxed);
        let entity = &self.living_entity.entity;
        entity.set_synced_data(tracked, baby);
        // Pumpkin's negative age is what reports the Bedrock baby flag.
        entity.age.store(
            if baby { -24000 } else { 0 },
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}
