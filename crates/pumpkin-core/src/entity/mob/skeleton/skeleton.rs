use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::sound::Sound;
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::entity::{
    Entity, EntityBase,
    mob::{
        Mob, conversion,
        skeleton::{SkeletonEntityBase, abstract_skeleton_mob},
    },
};

/// Vanilla `Skeleton` freezing tracker: 140 ticks in powder snow, then 300 to convert.
const FREEZE_TIME: i32 = 140;
const CONVERSION_TIME: i32 = 300;

pub struct SkeletonEntity {
    entity: Arc<SkeletonEntityBase>,
    freezing_time: AtomicI32,
    conversion_time: AtomicI32,
    converting: AtomicBool,
}

impl SkeletonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = SkeletonEntityBase::new(entity);
        Arc::new(Self {
            entity,
            freezing_time: AtomicI32::new(0),
            conversion_time: AtomicI32::new(0),
            converting: AtomicBool::new(false),
        })
    }

    fn set_converting(&self, converting: bool) {
        self.converting.store(converting, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::skeleton::DATA_STRAY_CONVERSION_ID,
            converting,
        );
    }

    fn start_conversion(&self, time: i32) {
        self.conversion_time.store(time, Ordering::Relaxed);
        self.set_converting(true);
    }

    // skeleton: powder snow -> stray (Skeleton.freezingTracker, ConversionTracker.tick)
    fn tick_freezing(&self) {
        let entity = self.get_entity();
        if !entity.is_alive() || self.entity.mob_entity.is_no_ai() {
            return;
        }
        if !entity.is_in_powder_snow.load(Ordering::Relaxed) {
            self.freezing_time.store(-1, Ordering::Relaxed);
            if self.converting.load(Ordering::Relaxed) {
                self.set_converting(false);
            }
            return;
        }
        if self.converting.load(Ordering::Relaxed) {
            if self.conversion_time.fetch_sub(1, Ordering::Relaxed) - 1 < 0 {
                let world = entity.world.load_full();
                conversion::convert_single(self, &EntityType::STRAY, true, true, |converted| {
                    let converted_entity = converted.get_entity();
                    if !converted_entity.is_silent() {
                        world.sync_world_event(
                            WorldEvent::SoundSkeletonToStray,
                            converted_entity.block_pos.load(),
                            0,
                        );
                    }
                });
            }
        } else if self.freezing_time.fetch_add(1, Ordering::Relaxed) + 1 >= FREEZE_TIME {
            self.start_conversion(CONVERSION_TIME);
        }
    }
}

impl Mob for SkeletonEntity {
    abstract_skeleton_mob!(entity);

    // skeleton: freezing tracker runs before the mob tick (Skeleton.tick)
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.tick_freezing();
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntitySkeletonStep, 0.15, 1.0);
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let converting = self.converting.load(Ordering::Relaxed);
        nbt.put_int(
            "StrayConversionTime",
            if converting {
                self.conversion_time.load(Ordering::Relaxed)
            } else {
                -1
            },
        );
        let in_snow = self.get_entity().is_in_powder_snow.load(Ordering::Relaxed);
        nbt.put_int(
            "FreezingTime",
            if in_snow {
                self.freezing_time.load(Ordering::Relaxed)
            } else {
                -1
            },
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.freezing_time
            .store(nbt.get_int("FreezingTime").unwrap_or(0), Ordering::Relaxed);
        match nbt.get_int("StrayConversionTime").unwrap_or(-1) {
            -1 => self.set_converting(false),
            time => self.start_conversion(time),
        }
    }
}
