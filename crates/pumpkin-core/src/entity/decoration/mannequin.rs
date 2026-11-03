use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use pumpkin_data::data_component_impl::{DataComponentImpl, ProfileImpl};
use pumpkin_data::entity::EntityPose;
use pumpkin_data::tracked_data::mannequin as tracked;
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use pumpkin_util::text::TextComponent;

use crate::entity::{Entity, EntityBase, living::LivingEntity};

// mannequin: Avatar.DEFAULT_MAIN_HAND = HumanoidArm.RIGHT
const MAIN_HAND_LEFT: u8 = 0;
const MAIN_HAND_RIGHT: u8 = 1;
// mannequin: ALL_LAYERS, every PlayerModelPart mask
const ALL_LAYERS: u8 = 0x7F;
// mannequin: PlayerModelPart serialized names by mask
const MODEL_PARTS: [(&str, u8); 7] = [
    ("cape", 0x01),
    ("jacket", 0x02),
    ("left_sleeve", 0x04),
    ("right_sleeve", 0x08),
    ("left_pants_leg", 0x10),
    ("right_pants_leg", 0x20),
    ("hat", 0x40),
];
// mannequin: VALID_POSES and their serialized names
const VALID_POSES: [(&str, EntityPose); 5] = [
    ("standing", EntityPose::Standing),
    ("crouching", EntityPose::Crouching),
    ("swimming", EntityPose::Swimming),
    ("fall_flying", EntityPose::FallFlying),
    ("sleeping", EntityPose::Sleeping),
];

/// Vanilla `Mannequin`: a player shaped `Avatar` with a synced profile, an optional label
/// and an immovable flag.
pub struct MannequinEntity {
    living_entity: LivingEntity,
    profile: Mutex<ProfileImpl>,
    immovable: AtomicBool,
    description: Mutex<TextComponent>,
    hide_description: AtomicBool,
    hidden_layers_mask: AtomicU8,
    main_hand_left: AtomicBool,
}

fn default_description() -> TextComponent {
    TextComponent::translate("entity.minecraft.mannequin.label", [])
}

impl MannequinEntity {
    pub fn new(entity: Entity) -> Self {
        let mannequin = Self {
            living_entity: LivingEntity::new(entity),
            profile: Mutex::new(ProfileImpl::default()),
            immovable: AtomicBool::new(false),
            description: Mutex::new(default_description()),
            hide_description: AtomicBool::new(false),
            hidden_layers_mask: AtomicU8::new(ALL_LAYERS),
            main_hand_left: AtomicBool::new(false),
        };
        mannequin.sync_defaults();
        mannequin
    }

    fn sync_defaults(&self) {
        let entity = self.get_entity();
        entity.set_synced_data(
            tracked::PLAYER_MODE_CUSTOMISATION,
            self.hidden_layers_mask.load(Ordering::Relaxed),
        );
        entity.set_synced_data(tracked::PLAYER_MAIN_HAND, MAIN_HAND_RIGHT);
        entity.set_synced_data(tracked::DATA_PROFILE, ProfileImpl::default());
        entity.set_synced_data(tracked::DATA_IMMOVABLE, false);
        self.update_description();
    }

    fn set_profile(&self, profile: ProfileImpl) {
        self.get_entity()
            .set_synced_data(tracked::DATA_PROFILE, profile.clone());
        *self
            .profile
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = profile;
    }

    fn set_immovable(&self, immovable: bool) {
        self.immovable.store(immovable, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked::DATA_IMMOVABLE, immovable);
    }

    // mannequin: updateDescription
    fn update_description(&self) {
        let description = (!self.hide_description.load(Ordering::Relaxed)).then(|| {
            self.description
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        self.get_entity()
            .set_synced_data(tracked::DATA_DESCRIPTION, description);
    }

    // mannequin: LAYERS_CODEC, the list names the hidden parts
    fn read_hidden_layers(nbt: &NbtCompound) -> u8 {
        let Some(list) = nbt.get_list("hidden_layers") else {
            return ALL_LAYERS;
        };
        list.iter()
            .filter_map(|tag| match tag {
                NbtTag::String(name) => MODEL_PARTS
                    .iter()
                    .find(|(part, _)| *part == &**name)
                    .map(|(_, mask)| *mask),
                _ => None,
            })
            .fold(ALL_LAYERS, |mask, hidden| mask & !hidden)
    }

    fn write_hidden_layers(mask: u8) -> Vec<NbtTag> {
        MODEL_PARTS
            .iter()
            .filter(|(_, part_mask)| mask & part_mask == 0)
            .map(|(name, _)| NbtTag::String((*name).into()))
            .collect()
    }
}

impl EntityBase for MannequinEntity {
    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put(
            "profile",
            self.profile
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .write_data(),
        );
        nbt.put_list(
            "hidden_layers",
            Self::write_hidden_layers(self.hidden_layers_mask.load(Ordering::Relaxed)),
        );
        let left = self.main_hand_left.load(Ordering::Relaxed);
        nbt.put_string("main_hand", if left { "left" } else { "right" }.to_string());
        let pose = self.get_entity().pose.load();
        if let Some((name, _)) = VALID_POSES.iter().find(|(_, valid)| *valid == pose) {
            nbt.put_string("pose", (*name).to_string());
        }
        nbt.put_bool("immovable", self.immovable.load(Ordering::Relaxed));
        if self.hide_description.load(Ordering::Relaxed) {
            nbt.put_bool("hide_description", true);
        } else {
            let description = self
                .description
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if description != default_description() {
                nbt.put(
                    "description",
                    description.to_nbt_tag_for_version(
                        &pumpkin_util::version::JavaMinecraftVersion::V_26_3,
                    ),
                );
            }
        }
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(profile) = nbt.get("profile").and_then(ProfileImpl::read_data) {
            self.set_profile(profile);
        }
        let mask = Self::read_hidden_layers(nbt);
        self.hidden_layers_mask.store(mask, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked::PLAYER_MODE_CUSTOMISATION, mask);
        let left = nbt.get_string("main_hand") == Some("left");
        self.get_entity().set_synced_data(
            tracked::PLAYER_MAIN_HAND,
            if left {
                MAIN_HAND_LEFT
            } else {
                MAIN_HAND_RIGHT
            },
        );
        self.main_hand_left.store(left, Ordering::Relaxed);
        let pose = nbt
            .get_string("pose")
            .and_then(|name| VALID_POSES.iter().find(|(valid, _)| *valid == name))
            .map_or(EntityPose::Standing, |(_, pose)| *pose);
        self.get_entity().set_pose(pose);
        self.set_immovable(nbt.get_bool("immovable").unwrap_or(false));
        self.hide_description.store(
            nbt.get_bool("hide_description").unwrap_or(false),
            Ordering::Relaxed,
        );
        *self
            .description
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = nbt
            .get("description")
            .map_or_else(default_description, TextComponent::from_nbt);
        self.update_description();
    }

    // mannequin: isImmobile
    fn is_immobile(&self) -> bool {
        self.immovable.load(Ordering::Relaxed) || self.living_entity.is_dead_or_dying()
    }

    fn get_entity(&self) -> &Entity {
        &self.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.living_entity)
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
