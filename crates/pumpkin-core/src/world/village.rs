use pumpkin_data::entity::EntityType;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::World;

/// Vanilla `PoiManager.MAX_VILLAGE_DISTANCE`, in sections.
pub const MAX_VILLAGE_DISTANCE: i32 = 6;
/// Range in which villagers claim beds and meeting points (vanilla `AcquirePoi` range).
const VILLAGER_CLAIM_RANGE: f64 = 48.0;

// village: poi types in #minecraft:village (acquirable job sites + home + meeting)
#[must_use]
pub fn is_village_poi_type(poi_type: &str) -> bool {
    matches!(
        poi_type,
        "minecraft:home"
            | "minecraft:meeting"
            | "minecraft:armorer"
            | "minecraft:butcher"
            | "minecraft:cartographer"
            | "minecraft:cleric"
            | "minecraft:farmer"
            | "minecraft:fisherman"
            | "minecraft:fletcher"
            | "minecraft:leatherworker"
            | "minecraft:librarian"
            | "minecraft:mason"
            | "minecraft:shepherd"
            | "minecraft:toolsmith"
            | "minecraft:weaponsmith"
    )
}

/// Villagers near a spot, gathered once so occupancy checks need no further entity queries.
pub struct VillageOccupancy {
    /// Villager position and claimed home.
    villagers: Vec<(Vector3<f64>, Option<BlockPos>)>,
}

impl VillageOccupancy {
    // village: villagers that could hold a poi around `center` within `radius` blocks
    #[must_use]
    pub fn gather(world: &World, center: BlockPos, radius: i32) -> Self {
        let reach = f64::from(radius) + VILLAGER_CLAIM_RANGE;
        let center = center.to_centered_f64();
        let area = BoundingBox::new(
            Vector3::new(center.x - reach, center.y - reach, center.z - reach),
            Vector3::new(center.x + reach, center.y + reach, center.z + reach),
        );
        let villagers = world
            .get_entities_at_box(&area)
            .into_iter()
            .filter(|entity| entity.get_entity().entity_type == &EntityType::VILLAGER)
            .map(|entity| (entity.get_entity().pos.load(), entity.get_home_pos()))
            .collect();
        Self { villagers }
    }

    // village: poi has a claimed ticket (PoiRecord.isOccupied); bells count once a villager lives in reach
    #[must_use]
    pub fn is_occupied(&self, world: &World, pos: BlockPos, poi_type: &str) -> bool {
        match poi_type {
            "minecraft:home" => self.villagers.iter().any(|(_, home)| *home == Some(pos)),
            "minecraft:meeting" => {
                let bell = pos.to_centered_f64();
                self.villagers.iter().any(|(villager, _)| {
                    villager.squared_distance_to_vec(&bell) <= VILLAGER_CLAIM_RANGE.powi(2)
                })
            }
            _ => world
                .villager_poi
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_claimed(pos),
        }
    }
}

impl World {
    // village: section distance to the nearest occupied village poi, capped (PoiManager.sectionsToVillage)
    #[must_use]
    pub fn sections_to_village(&self, pos: &BlockPos) -> i32 {
        let (sx, sy, sz) = (pos.0.x >> 4, pos.0.y >> 4, pos.0.z >> 4);
        let center = BlockPos::new(sx * 16 + 8, pos.0.y, sz * 16 + 8);
        let radius = MAX_VILLAGE_DISTANCE * 16 + 8;
        let mut candidates: Vec<(i32, BlockPos, &'static str)> = Vec::new();
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(center, radius, |poi_pos, poi_type| {
                if !is_village_poi_type(poi_type) {
                    return;
                }
                let distance = (poi_pos.0.x >> 4)
                    .abs_diff(sx)
                    .max((poi_pos.0.y >> 4).abs_diff(sy))
                    .max((poi_pos.0.z >> 4).abs_diff(sz)) as i32;
                if distance <= MAX_VILLAGE_DISTANCE {
                    candidates.push((distance, poi_pos, static_poi_type(poi_type)));
                }
            });
        if candidates.is_empty() {
            return MAX_VILLAGE_DISTANCE + 1;
        }
        candidates.sort_unstable_by_key(|(distance, _, _)| *distance);
        let occupancy = VillageOccupancy::gather(self, center, radius);
        candidates
            .into_iter()
            .find(|(_, poi_pos, poi_type)| occupancy.is_occupied(self, *poi_pos, poi_type))
            .map_or(MAX_VILLAGE_DISTANCE + 1, |(distance, _, _)| distance)
    }

    // village: occupied village poi sections near `pos`, one scan for many lookups (PoiManager.sectionsToVillage)
    #[must_use]
    pub fn occupied_village_sections(&self, pos: &BlockPos, reach_sections: i32) -> Vec<[i32; 3]> {
        let center = BlockPos::new(
            (pos.0.x >> 4) * 16 + 8,
            pos.0.y,
            (pos.0.z >> 4) * 16 + 8,
        );
        let radius = (reach_sections + MAX_VILLAGE_DISTANCE) * 16 + 8;
        let mut candidates: Vec<(BlockPos, &'static str)> = Vec::new();
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(center, radius, |poi_pos, poi_type| {
                if is_village_poi_type(poi_type) {
                    candidates.push((poi_pos, static_poi_type(poi_type)));
                }
            });
        if candidates.is_empty() {
            return Vec::new();
        }
        let occupancy = VillageOccupancy::gather(self, center, radius);
        candidates
            .into_iter()
            .filter(|(poi_pos, poi_type)| occupancy.is_occupied(self, *poi_pos, poi_type))
            .map(|(poi_pos, _)| [poi_pos.0.x >> 4, poi_pos.0.y >> 4, poi_pos.0.z >> 4])
            .collect()
    }

    // village: every occupied poi within `radius` of `center` (PoiManager.getInRange, IS_OCCUPIED)
    #[must_use]
    pub fn occupied_pois_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        let mut candidates: Vec<(BlockPos, &'static str)> = Vec::new();
        let radius_sq = i64::from(radius).pow(2);
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(center, radius, |poi_pos, poi_type| {
                let (dx, dy, dz) = (
                    i64::from(poi_pos.0.x - center.0.x),
                    i64::from(poi_pos.0.y - center.0.y),
                    i64::from(poi_pos.0.z - center.0.z),
                );
                if dx * dx + dy * dy + dz * dz <= radius_sq {
                    candidates.push((poi_pos, static_poi_type(poi_type)));
                }
            });
        if candidates.is_empty() {
            return Vec::new();
        }
        let occupancy = VillageOccupancy::gather(self, center, radius);
        candidates
            .into_iter()
            .filter(|(poi_pos, poi_type)| occupancy.is_occupied(self, *poi_pos, poi_type))
            .map(|(poi_pos, _)| poi_pos)
            .collect()
    }

    // village: within `range` sections of a village (ServerLevel.isCloseToVillage)
    #[must_use]
    pub fn is_close_to_village(&self, pos: &BlockPos, range: i32) -> bool {
        self.sections_to_village(pos) <= range
    }

    // village: in a village section or next to one (ServerLevel.isVillage)
    #[must_use]
    pub fn is_village(&self, pos: &BlockPos) -> bool {
        self.is_close_to_village(pos, 1)
    }

    // village: nearest occupied village poi within `radius` passing `filter` (PoiManager.find, IS_OCCUPIED)
    pub fn find_occupied_village_poi(
        &self,
        center: BlockPos,
        radius: i32,
        occupancy: &VillageOccupancy,
        filter: impl Fn(&BlockPos) -> bool,
    ) -> Option<BlockPos> {
        let radius_sq = i64::from(radius).pow(2);
        let mut candidates: Vec<(i64, BlockPos, &'static str)> = Vec::new();
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(center, radius, |poi_pos, poi_type| {
                if !is_village_poi_type(poi_type) {
                    return;
                }
                let delta = poi_pos.0 - center.0;
                let distance_sq = i64::from(delta.x).pow(2)
                    + i64::from(delta.y).pow(2)
                    + i64::from(delta.z).pow(2);
                if distance_sq <= radius_sq {
                    candidates.push((distance_sq, poi_pos, static_poi_type(poi_type)));
                }
            });
        candidates.sort_unstable_by_key(|(distance, _, _)| *distance);
        candidates
            .into_iter()
            .find(|(_, poi_pos, poi_type)| {
                filter(poi_pos) && occupancy.is_occupied(self, *poi_pos, poi_type)
            })
            .map(|(_, poi_pos, _)| poi_pos)
    }
}

fn static_poi_type(poi_type: &str) -> &'static str {
    match poi_type {
        "minecraft:home" => "minecraft:home",
        "minecraft:meeting" => "minecraft:meeting",
        _ => "minecraft:job_site",
    }
}

// village: sections to village from a pre-scanned list (PoiManager.sectionsToVillage)
#[must_use]
pub fn sections_to_village_in(sections: &[[i32; 3]], section: [i32; 3]) -> i32 {
    sections
        .iter()
        .map(|poi| {
            (poi[0] - section[0])
                .abs()
                .max((poi[1] - section[1]).abs())
                .max((poi[2] - section[2]).abs())
        })
        .filter(|distance| *distance <= MAX_VILLAGE_DISTANCE)
        .min()
        .unwrap_or(MAX_VILLAGE_DISTANCE + 1)
}
