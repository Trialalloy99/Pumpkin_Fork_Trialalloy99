use pumpkin_util::math::position::BlockPos;

use super::World;
use super::villager_poi::max_tickets_for_type;

/// Vanilla `PoiManager.Occupancy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PoiOccupancy {
    HasSpace,
    IsOccupied,
    Any,
}

impl World {
    // poi: PoiManager.getType
    #[must_use]
    pub fn poi_type_at(&self, pos: &BlockPos) -> Option<String> {
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_type(pos)
            .map(str::to_owned)
    }

    // poi: PoiManager.exists
    #[must_use]
    pub fn poi_exists(&self, pos: &BlockPos, matches: impl Fn(&str) -> bool) -> bool {
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_type(pos)
            .is_some_and(matches)
    }

    // poi: PoiManager.findAllClosestFirstWithType, spherical filter over the chebyshev gather
    #[must_use]
    pub fn poi_find_all_closest_first(
        &self,
        center: BlockPos,
        radius: i32,
        matches: impl Fn(&str) -> bool,
        occupancy: PoiOccupancy,
    ) -> Vec<(BlockPos, String)> {
        let radius_sq = i64::from(radius).pow(2);
        let mut found: Vec<(i64, BlockPos, String)> = Vec::new();
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_entry_in_square(center, radius, |entry| {
                if !matches(&entry.poi_type) {
                    return;
                }
                let max = max_tickets_for_type(&entry.poi_type);
                let wanted = match occupancy {
                    PoiOccupancy::HasSpace => entry.free_tickets > 0,
                    PoiOccupancy::IsOccupied => entry.free_tickets < max,
                    PoiOccupancy::Any => true,
                };
                if !wanted {
                    return;
                }
                let dx = i64::from(entry.x - center.0.x);
                let dy = i64::from(entry.y - center.0.y);
                let dz = i64::from(entry.z - center.0.z);
                let distance_sq = dx * dx + dy * dy + dz * dz;
                if distance_sq <= radius_sq {
                    found.push((distance_sq, entry.pos(), entry.poi_type.clone()));
                }
            });
        found.sort_unstable_by_key(|(distance, ..)| *distance);
        found
            .into_iter()
            .map(|(_, pos, poi_type)| (pos, poi_type))
            .collect()
    }

    // poi: PoiManager.take, one ticket of the poi at `pos`
    pub fn poi_take(&self, pos: &BlockPos) -> bool {
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take_ticket(pos)
    }

    // poi: PoiManager.release
    pub fn poi_release(&self, pos: &BlockPos) -> bool {
        let mut poi = self
            .portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(max) = poi.get_type(pos).map(max_tickets_for_type) else {
            return false;
        };
        poi.release_ticket(pos, max)
    }

    // poi: PoiRecord.isOccupied
    #[must_use]
    pub fn poi_is_occupied(&self, pos: &BlockPos) -> bool {
        let mut poi = self
            .portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(max) = poi.get_type(pos).map(max_tickets_for_type) else {
            return false;
        };
        poi.free_tickets(pos).is_some_and(|free| free < max)
    }
}

impl World {
    // poi: closest poi of the village tag, the center a raid is created around
    #[must_use]
    pub fn poi_closest_village_poi(&self, center: BlockPos, radius: i32) -> Option<BlockPos> {
        self.poi_find_all_closest_first(
            center,
            radius,
            super::village::is_village_poi_type,
            PoiOccupancy::Any,
        )
        .first()
        .map(|(pos, _)| *pos)
    }
}
