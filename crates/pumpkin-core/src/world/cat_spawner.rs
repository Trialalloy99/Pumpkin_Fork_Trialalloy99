use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_data::entity::EntityType;
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    EntityBase,
    mob::spawn::{SpawnReason, finalize_spawn},
    r#type::from_type,
};
use crate::world::{World, natural_spawner, village::VillageOccupancy};

// cat spawner: TICK_DELAY (CatSpawner)
const TICK_DELAY: i64 = 1200;
const SPAWN_CHECK_RADIUS: i32 = 10;
const VILLAGE_RADIUS: i32 = 48;
const VILLAGE_HEIGHT: f64 = 8.0;
const VILLAGE_MAX_CATS: usize = 5;
const MIN_VILLAGE_HOMES: usize = 4;

impl World {
    // cat spawner: CatSpawner.tick, overworld villages
    pub fn tick_cat_spawner(self: &Arc<Self>) {
        if self.dimension != Dimension::OVERWORLD || self.get_world_age() % TICK_DELAY != 0 {
            return;
        }
        let players = self.players.load();
        if players.is_empty() {
            return;
        }
        let (player, x, z) = {
            let mut rng = rand::rng();
            let player = players[rng.random_range(0..players.len())].clone();
            let x = (8 + rng.random_range(0..24)) * if rng.random::<bool>() { -1 } else { 1 };
            let z = (8 + rng.random_range(0..24)) * if rng.random::<bool>() { -1 } else { 1 };
            (player, x, z)
        };
        let origin = player.get_entity().block_pos.load();
        let spawn_pos = BlockPos::new(origin.0.x + x, origin.0.y, origin.0.z + z);
        let corners = [
            (-SPAWN_CHECK_RADIUS, -SPAWN_CHECK_RADIUS),
            (SPAWN_CHECK_RADIUS, -SPAWN_CHECK_RADIUS),
            (-SPAWN_CHECK_RADIUS, SPAWN_CHECK_RADIUS),
            (SPAWN_CHECK_RADIUS, SPAWN_CHECK_RADIUS),
        ];
        if !corners.iter().all(|(dx, dz)| {
            self.is_loaded(&BlockPos::new(
                spawn_pos.0.x + dx,
                spawn_pos.0.y,
                spawn_pos.0.z + dz,
            ))
        }) {
            return;
        }
        if !natural_spawner::is_spawn_position_ok(self, &spawn_pos, &EntityType::CAT) {
            return;
        }
        if self.is_close_to_village(&spawn_pos, 2) {
            self.spawn_cat_in_village(&spawn_pos);
        }
    }

    // cat spawner: spawnInVillage, more than 4 occupied homes and fewer than 5 cats
    fn spawn_cat_in_village(self: &Arc<Self>, spawn_pos: &BlockPos) {
        let occupancy = VillageOccupancy::gather(self, *spawn_pos, VILLAGE_RADIUS);
        let mut homes = 0;
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(*spawn_pos, VILLAGE_RADIUS, |poi_pos, poi_type| {
                if poi_type == "minecraft:home" && occupancy.is_occupied(self, poi_pos, poi_type) {
                    homes += 1;
                }
            });
        if homes <= MIN_VILLAGE_HOMES {
            return;
        }
        let center = spawn_pos.to_centered_f64();
        let area = BoundingBox::new(
            Vector3::new(
                center.x - f64::from(VILLAGE_RADIUS),
                center.y - VILLAGE_HEIGHT,
                center.z - f64::from(VILLAGE_RADIUS),
            ),
            Vector3::new(
                center.x + f64::from(VILLAGE_RADIUS),
                center.y + VILLAGE_HEIGHT,
                center.z + f64::from(VILLAGE_RADIUS),
            ),
        );
        let cats = self
            .get_entities_at_box(&area)
            .iter()
            .filter(|entity| entity.get_entity().entity_type == &EntityType::CAT)
            .count();
        if cats < VILLAGE_MAX_CATS {
            self.spawn_cat(spawn_pos, false);
        }
    }

    // cat spawner: spawnCat
    fn spawn_cat(self: &Arc<Self>, spawn_pos: &BlockPos, make_persistent: bool) {
        let cat = from_type(
            &EntityType::CAT,
            spawn_pos.to_centered_f64(),
            self,
            Uuid::new_v4(),
        );
        finalize_spawn(&cat, self, None, SpawnReason::Natural);
        if make_persistent && let Some(mob) = cat.get_mob() {
            mob.get_mob_entity()
                .persistence_required
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.spawn_entity(cat);
    }
}
