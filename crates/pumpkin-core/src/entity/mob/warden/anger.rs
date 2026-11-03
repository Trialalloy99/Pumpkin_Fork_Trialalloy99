//! Ported from `net.minecraft.world.entity.monster.warden.AngerLevel` and `AngerManagement`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use pumpkin_data::sound::Sound;
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use uuid::Uuid;

use crate::entity::{EntityBase, RemovalReason};
use crate::world::World;

const MAX_ANGER: i32 = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AngerLevel {
    Calm,
    Agitated,
    Angry,
}

impl AngerLevel {
    #[must_use]
    pub const fn minimum_anger(self) -> i32 {
        match self {
            Self::Calm => 0,
            Self::Agitated => 40,
            Self::Angry => 80,
        }
    }

    #[must_use]
    pub const fn ambient_sound(self) -> Sound {
        match self {
            Self::Calm => Sound::EntityWardenAmbient,
            Self::Agitated => Sound::EntityWardenAgitated,
            Self::Angry => Sound::EntityWardenAngry,
        }
    }

    #[must_use]
    pub const fn listening_sound(self) -> Sound {
        match self {
            Self::Calm => Sound::EntityWardenListening,
            Self::Agitated | Self::Angry => Sound::EntityWardenListeningAngry,
        }
    }

    #[must_use]
    pub const fn by_anger(anger: i32) -> Self {
        if anger >= Self::Angry.minimum_anger() {
            Self::Angry
        } else if anger >= Self::Agitated.minimum_anger() {
            Self::Agitated
        } else {
            Self::Calm
        }
    }

    #[must_use]
    pub const fn is_angry(self) -> bool {
        matches!(self, Self::Angry)
    }
}

/// Vanilla `AngerManagement`: who a warden is angry at and how much.
pub struct AngerManagement {
    conversion_delay: i32,
    highest_anger: i32,
    suspects: Vec<Arc<dyn EntityBase>>,
    anger_by_suspect: HashMap<Uuid, i32>,
    anger_by_uuid: HashMap<Uuid, i32>,
}

impl Default for AngerManagement {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl AngerManagement {
    #[must_use]
    pub fn new(anger_by_uuid: Vec<(Uuid, i32)>) -> Self {
        Self {
            conversion_delay: rand::random_range(0..=2),
            highest_anger: 0,
            suspects: Vec::new(),
            anger_by_suspect: HashMap::new(),
            anger_by_uuid: anger_by_uuid.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn read_nbt(nbt: &NbtCompound) -> Self {
        let pairs = nbt
            .get_list("suspects")
            .map(|list| {
                list.iter()
                    .filter_map(|tag| {
                        let compound = tag.extract_compound()?;
                        Some((
                            compound.get_uuid("uuid")?,
                            compound.get_int("anger")?.max(0),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::new(pairs)
    }

    #[must_use]
    pub fn write_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        let suspects = self
            .anger_by_suspect
            .iter()
            .chain(self.anger_by_uuid.iter())
            .map(|(uuid, anger)| {
                let mut entry = NbtCompound::new();
                entry.put_uuid("uuid", *uuid);
                entry.put_int("anger", *anger);
                NbtTag::Compound(entry)
            })
            .collect();
        nbt.put_list("suspects", suspects);
        nbt
    }

    pub fn tick(&mut self, world: &World, valid_entity: impl Fn(&dyn EntityBase) -> bool) {
        self.conversion_delay -= 1;
        if self.conversion_delay <= 0 {
            self.convert_from_uuids(world);
            self.conversion_delay = 2;
        }

        self.anger_by_uuid.retain(|_, anger| {
            if *anger <= 1 {
                false
            } else {
                *anger -= 1;
                true
            }
        });

        let suspects = std::mem::take(&mut self.suspects);
        for entity in suspects {
            let uuid = entity.get_entity().entity_uuid;
            let Some(anger) = self.anger_by_suspect.get(&uuid).copied() else {
                continue;
            };
            let removal_reason = entity.get_entity().removal_reason.load();
            if anger > 1 && valid_entity(entity.as_ref()) && removal_reason.is_none() {
                self.anger_by_suspect.insert(uuid, anger - 1);
                self.suspects.push(entity);
            } else {
                self.anger_by_suspect.remove(&uuid);
                if anger > 1
                    && matches!(
                        removal_reason,
                        Some(
                            RemovalReason::ChangedDimension
                                | RemovalReason::UnloadedToChunk
                                | RemovalReason::UnloadedWithPlayer
                        )
                    )
                {
                    self.anger_by_uuid.insert(uuid, anger - 1);
                }
            }
        }
        self.sort_and_update_highest_anger();
    }

    // vanilla AngerManagement.Sorter: angry first, players first, then by anger
    fn sort_and_update_highest_anger(&mut self) {
        let anger_by_suspect = &self.anger_by_suspect;
        let anger = |entity: &Arc<dyn EntityBase>| {
            anger_by_suspect
                .get(&entity.get_entity().entity_uuid)
                .copied()
                .unwrap_or(0)
        };
        self.suspects.sort_by(|a, b| {
            if a.get_entity().entity_uuid == b.get_entity().entity_uuid {
                return Ordering::Equal;
            }
            let (anger_a, anger_b) = (anger(a), anger(b));
            let (angry_a, angry_b) = (
                AngerLevel::by_anger(anger_a).is_angry(),
                AngerLevel::by_anger(anger_b).is_angry(),
            );
            if angry_a != angry_b {
                return if angry_a {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            let (player_a, player_b) = (a.get_player().is_some(), b.get_player().is_some());
            if player_a != player_b {
                return if player_a {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            anger_b.cmp(&anger_a)
        });
        self.highest_anger = self.suspects.iter().map(anger).max().unwrap_or(0);
    }

    fn convert_from_uuids(&mut self, world: &World) {
        let known: Vec<Uuid> = self.anger_by_uuid.keys().copied().collect();
        for uuid in known {
            if let Some(entity) = world.get_entity_by_uuid(uuid) {
                let anger = self.anger_by_uuid.remove(&uuid).unwrap_or(0);
                self.anger_by_suspect.insert(uuid, anger);
                self.suspects.push(entity);
            }
        }
    }

    pub fn increase_anger(&mut self, entity: &Arc<dyn EntityBase>, increment: i32) -> i32 {
        let uuid = entity.get_entity().entity_uuid;
        let new_suspect = !self.anger_by_suspect.contains_key(&uuid);
        let mut current =
            (self.anger_by_suspect.get(&uuid).copied().unwrap_or(0) + increment).min(MAX_ANGER);
        self.anger_by_suspect.insert(uuid, current);
        if new_suspect {
            current += self.anger_by_uuid.remove(&uuid).unwrap_or(0);
            self.anger_by_suspect.insert(uuid, current);
            self.suspects.push(entity.clone());
        }
        self.sort_and_update_highest_anger();
        current
    }

    pub fn clear_anger(&mut self, entity: &dyn EntityBase) {
        let uuid = entity.get_entity().entity_uuid;
        self.anger_by_suspect.remove(&uuid);
        self.suspects
            .retain(|suspect| suspect.get_entity().entity_uuid != uuid);
        self.sort_and_update_highest_anger();
    }

    #[must_use]
    pub fn active_anger(&self, current_target: Option<&dyn EntityBase>) -> i32 {
        current_target.map_or(self.highest_anger, |target| {
            self.anger_by_suspect
                .get(&target.get_entity().entity_uuid)
                .copied()
                .unwrap_or(0)
        })
    }

    /// Top suspect that passes `filter` and is a living entity.
    #[must_use]
    pub fn active_entity(
        &self,
        filter: impl Fn(&dyn EntityBase) -> bool,
    ) -> Option<Arc<dyn EntityBase>> {
        self.suspects
            .iter()
            .find(|suspect| filter(suspect.as_ref()))
            .filter(|suspect| suspect.get_living_entity().is_some())
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_minimum_anger() {
        assert_eq!(AngerLevel::by_anger(0), AngerLevel::Calm);
        assert_eq!(AngerLevel::by_anger(39), AngerLevel::Calm);
        assert_eq!(AngerLevel::by_anger(40), AngerLevel::Agitated);
        assert_eq!(AngerLevel::by_anger(79), AngerLevel::Agitated);
        assert_eq!(AngerLevel::by_anger(80), AngerLevel::Angry);
        assert!(AngerLevel::by_anger(150).is_angry());
    }

    #[test]
    fn anger_that_is_not_loaded_decays_and_persists() {
        let id = Uuid::from_u128(5);
        let mut management = AngerManagement::new(vec![(id, 3), (Uuid::from_u128(6), 1)]);
        let nbt = management.write_nbt();
        assert_eq!(nbt.get_list("suspects").map(<[NbtTag]>::len), Some(2));

        management.anger_by_uuid.retain(|_, anger| {
            if *anger <= 1 {
                false
            } else {
                *anger -= 1;
                true
            }
        });
        assert_eq!(management.anger_by_uuid.get(&id), Some(&2));
        assert_eq!(management.anger_by_uuid.len(), 1);

        let reloaded = AngerManagement::read_nbt(&nbt);
        assert_eq!(reloaded.anger_by_uuid.get(&id), Some(&3));
    }
}
