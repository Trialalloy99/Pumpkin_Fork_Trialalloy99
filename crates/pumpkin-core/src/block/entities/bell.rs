use crate::block::entities::BlockEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::memory::{MemoryStatus, types};
use crate::world::World;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::block_properties::HorizontalFacing;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use std::any::Any;
use std::sync::{Arc, Mutex};

const DURATION: i32 = 50;
const GLOW_DURATION: i32 = 60;
const MIN_TICKS_BETWEEN_SEARCHES: i64 = 60;
const MAX_RESONATION_TICKS: i32 = 40;
const TICKS_BEFORE_RESONATION: i32 = 5;
const SEARCH_RADIUS: f64 = 48.0;
const HEAR_BELL_RADIUS: f64 = 32.0;
const HIGHLIGHT_RAIDERS_RADIUS: f64 = 48.0;

pub struct BellBlockEntity {
    pub position: BlockPos,
    pub last_side_hit: AtomicCell<Option<HorizontalFacing>>,
    pub ring_ticks: AtomicCell<i32>,
    pub ringing: AtomicCell<bool>,
    resonating: AtomicCell<bool>,
    resonate_time: AtomicCell<i32>,
    last_ring_timestamp: AtomicCell<i64>,
    nearby_entities: Mutex<Option<Vec<Arc<dyn EntityBase>>>>,
}

impl BellBlockEntity {
    pub const ID: &'static str = "minecraft:bell";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            last_side_hit: AtomicCell::new(None),
            ring_ticks: AtomicCell::new(0),
            resonate_time: AtomicCell::new(0),
            resonating: AtomicCell::new(false),
            ringing: AtomicCell::new(false),
            last_ring_timestamp: AtomicCell::new(0),
            nearby_entities: Mutex::new(None),
        }
    }

    // bell: BellBlockEntity.onHit and triggerEvent(1)
    pub fn activate(&self, world: &Arc<World>, direction: HorizontalFacing) {
        self.last_side_hit.store(Some(direction));
        self.update_entities(world);
        self.resonate_time.store(0);
        self.ring_ticks.store(0);
        self.ringing.store(true);
    }

    fn is_within(&self, entity: &dyn EntityBase, distance: f64) -> bool {
        entity.get_entity().is_alive()
            && !entity.get_entity().is_removed()
            && self
                .position
                .to_centered_f64()
                .squared_distance_to_vec(&entity.get_entity().pos.load())
                < distance * distance
    }

    // bell: BellBlockEntity.updateEntities, everything within 32 blocks hears the bell
    fn update_entities(&self, world: &Arc<World>) {
        let now = world.get_world_age();
        let mut nearby = self
            .nearby_entities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now > self.last_ring_timestamp.load() + MIN_TICKS_BETWEEN_SEARCHES || nearby.is_none() {
            self.last_ring_timestamp.store(now);
            let center = self.position.to_f64();
            let area = BoundingBox::new(
                center.add_raw(-SEARCH_RADIUS, -SEARCH_RADIUS, -SEARCH_RADIUS),
                center.add_raw(
                    SEARCH_RADIUS + 1.0,
                    SEARCH_RADIUS + 1.0,
                    SEARCH_RADIUS + 1.0,
                ),
            );
            *nearby = Some(
                world
                    .get_all_at_box(&area)
                    .into_iter()
                    .filter(|entity| entity.get_living_entity().is_some())
                    .collect(),
            );
        }
        for entity in nearby.iter().flatten() {
            if !self.is_within(entity.as_ref(), HEAR_BELL_RADIUS) {
                continue;
            }
            if let Some(mob) = entity.get_mob() {
                mob.get_mob_entity().try_with_brain(mob, |brain| {
                    if brain.check(types::HEARD_BELL_TIME.id(), MemoryStatus::Registered) {
                        brain.set(types::HEARD_BELL_TIME, now);
                    }
                });
            }
        }
    }

    fn is_raider(entity: &dyn EntityBase) -> bool {
        entity
            .get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_RAIDERS)
    }

    // bell: BellBlockEntity.areRaidersNearby
    fn are_raiders_nearby(&self) -> bool {
        self.nearby_entities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flatten()
            .any(|entity| {
                self.is_within(entity.as_ref(), HEAR_BELL_RADIUS)
                    && Self::is_raider(entity.as_ref())
            })
    }

    // bell: BellBlockEntity.makeRaidersGlow
    fn make_raiders_glow(&self) {
        for entity in self
            .nearby_entities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flatten()
        {
            if self.is_within(entity.as_ref(), HIGHLIGHT_RAIDERS_RADIUS)
                && Self::is_raider(entity.as_ref())
                && let Some(living) = entity.get_living_entity()
            {
                living.add_effect(Effect {
                    effect_type: &StatusEffect::GLOWING,
                    duration: GLOW_DURATION,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                });
            }
        }
    }
}

impl BlockEntity for BellBlockEntity {
    fn write_nbt(&self, _nbt: &mut NbtCompound) {}

    fn from_nbt(_nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        Self::new(position)
    }

    fn tick(&self, world: &Arc<World>) {
        if self.ringing.load() {
            self.ring_ticks.fetch_add(1);
        }
        if self.ring_ticks.load() >= DURATION {
            self.ringing.store(false);
            self.ring_ticks.store(0);
        }
        if self.ring_ticks.load() >= TICKS_BEFORE_RESONATION
            && self.resonate_time.load() == 0
            && self.are_raiders_nearby()
        {
            self.resonating.store(true);
            world.play_sound_fine(
                Sound::BlockBellResonate,
                SoundCategory::Blocks,
                &self.position.to_centered_f64(),
                1.0,
                1.0,
            );
        }

        if self.resonating.load() {
            if self.resonate_time.load() < MAX_RESONATION_TICKS {
                self.resonate_time.fetch_add(1);
            } else {
                self.make_raiders_glow();
                self.resonating.store(false);
            }
        }
    }

    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
