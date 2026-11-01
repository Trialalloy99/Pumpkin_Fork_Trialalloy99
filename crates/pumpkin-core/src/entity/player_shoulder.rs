use std::sync::atomic::Ordering;

use pumpkin_data::sound::SoundCategory;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::optional_int::OptionalInt;
use pumpkin_util::math::vector3::Vector3;

use super::passive::tamable::TamableAnimal;
use super::player::Player;
use super::{EntityBase, passive::parrot::ParrotEntity, r#type::load_entity_from_nbt};

// shoulder: ServerPlayer.removeEntitiesOnShoulder grace period
const SHOULDER_GRACE_TICKS: i64 = 20;
const AMBIENT_SOUND_CHANCE: u32 = 200;
const SHOULDER_RESPAWN_HEIGHT: f64 = 0.7;

impl Player {
    fn lock_shoulder(
        cell: &std::sync::Mutex<NbtCompound>,
    ) -> std::sync::MutexGuard<'_, NbtCompound> {
        cell.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn game_time(&self) -> i64 {
        self.world().get_world_age()
    }

    // shoulder: Player.setShoulderParrotLeft/Right tracked data
    fn sync_shoulder_variants(&self) {
        let variant = |tag: &NbtCompound| {
            OptionalInt(
                (tag.get_string("id") == Some("minecraft:parrot"))
                    .then(|| tag.get_int("Variant").unwrap_or(0)),
            )
        };
        let left = variant(&Self::lock_shoulder(&self.shoulder_entity_left));
        let right = variant(&Self::lock_shoulder(&self.shoulder_entity_right));
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::player::DATA_SHOULDER_PARROT_LEFT,
            left,
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::player::DATA_SHOULDER_PARROT_RIGHT,
            right,
        );
    }

    // shoulder: ServerPlayer.setEntityOnShoulder
    pub fn set_entity_on_shoulder(&self, entity_tag: NbtCompound) -> bool {
        let entity = self.get_entity();
        if entity.has_vehicle() || !entity.on_ground.load(Ordering::Relaxed) || entity.is_in_water()
        {
            return false;
        }
        let stored = {
            let mut left = Self::lock_shoulder(&self.shoulder_entity_left);
            if left.is_empty() {
                *left = entity_tag;
                true
            } else {
                drop(left);
                let mut right = Self::lock_shoulder(&self.shoulder_entity_right);
                if right.is_empty() {
                    *right = entity_tag;
                    true
                } else {
                    false
                }
            }
        };
        if stored {
            self.time_entity_sat_on_shoulder
                .store(self.game_time(), Ordering::Relaxed);
            self.sync_shoulder_variants();
        }
        stored
    }

    // shoulder: ServerPlayer.removeEntitiesOnShoulder
    pub fn remove_entities_on_shoulder(&self) {
        if self.time_entity_sat_on_shoulder.load(Ordering::Relaxed) + SHOULDER_GRACE_TICKS
            >= self.game_time()
        {
            return;
        }
        let tags = [
            std::mem::take(&mut *Self::lock_shoulder(&self.shoulder_entity_left)),
            std::mem::take(&mut *Self::lock_shoulder(&self.shoulder_entity_right)),
        ];
        let mut respawned = false;
        for tag in tags {
            respawned |= self.respawn_entity_on_shoulder(&tag);
        }
        if respawned || !self.shoulder_variants_empty() {
            self.sync_shoulder_variants();
        }
    }

    fn shoulder_variants_empty(&self) -> bool {
        Self::lock_shoulder(&self.shoulder_entity_left).is_empty()
            && Self::lock_shoulder(&self.shoulder_entity_right).is_empty()
    }

    // shoulder: ServerPlayer.respawnEntityOnShoulder
    fn respawn_entity_on_shoulder(&self, tag: &NbtCompound) -> bool {
        if tag.is_empty() {
            return false;
        }
        let world = self.world();
        let pos = self.get_entity().pos.load();
        let Some(entity) = load_entity_from_nbt(tag, pos, &world) else {
            return false;
        };
        if let Some(parrot) = entity
            .get_mob()
            .and_then(|mob| mob.cast_any().downcast_ref::<ParrotEntity>())
        {
            parrot.set_owner(Some(self.gameprofile.id));
        }
        entity
            .get_entity()
            .set_pos(Vector3::new(pos.x, pos.y + SHOULDER_RESPAWN_HEIGHT, pos.z));
        world.spawn_entity(entity);
        true
    }

    // shoulder: ServerPlayer.handleShoulderEntities, ambient parrot sound and dismount conditions
    pub fn handle_shoulder_entities(&self) {
        if self.shoulder_variants_empty() {
            return;
        }
        for cell in [&self.shoulder_entity_left, &self.shoulder_entity_right] {
            let is_parrot = {
                let tag = Self::lock_shoulder(cell);
                !tag.is_empty()
                    && !tag.get_bool("Silent").unwrap_or(false)
                    && tag.get_string("id") == Some("minecraft:parrot")
            };
            if is_parrot && rand::random_range(0..AMBIENT_SOUND_CHANCE) == 0 {
                let world = self.world();
                let entity = self.get_entity();
                if !ParrotEntity::imitate_nearby_mobs(&world, entity) {
                    world.play_sound_fine(
                        ParrotEntity::get_ambient_sound(&world),
                        SoundCategory::Players,
                        &entity.pos.load(),
                        1.0,
                        ParrotEntity::get_pitch(),
                    );
                }
            }
        }
        let entity = self.get_entity();
        let flying = self
            .abilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flying;
        if self.living_entity.fall_distance.load() > 0.5
            || entity.is_in_water()
            || flying
            || self.is_sleeping()
        {
            self.remove_entities_on_shoulder();
        }
    }

    // shoulder: Player.addAdditionalSaveData ShoulderEntityLeft/Right
    pub fn write_shoulder_nbt(&self, nbt: &mut NbtCompound) {
        let left = Self::lock_shoulder(&self.shoulder_entity_left).clone();
        if !left.is_empty() {
            nbt.put_compound("ShoulderEntityLeft", left);
        }
        let right = Self::lock_shoulder(&self.shoulder_entity_right).clone();
        if !right.is_empty() {
            nbt.put_compound("ShoulderEntityRight", right);
        }
    }

    pub fn read_shoulder_nbt(&self, nbt: &NbtCompound) {
        *Self::lock_shoulder(&self.shoulder_entity_left) = nbt
            .get_compound("ShoulderEntityLeft")
            .cloned()
            .unwrap_or_default();
        *Self::lock_shoulder(&self.shoulder_entity_right) = nbt
            .get_compound("ShoulderEntityRight")
            .cloned()
            .unwrap_or_default();
        self.sync_shoulder_variants();
    }
}
