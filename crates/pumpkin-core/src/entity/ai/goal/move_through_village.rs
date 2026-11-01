use pumpkin_util::math::position::BlockPos;

use super::{Controls, Goal};
use crate::block::blocks::doors::DoorBlock;
use crate::entity::ai::pathfinder::path::Path;
use crate::entity::ai::util::{default_random_pos, land_random_pos};
use crate::entity::mob::Mob;
use crate::world::village::VillageOccupancy;

const VILLAGE_SECTION_RANGE: i32 = 6;
const POI_SEARCH_RADIUS: i32 = 10;
const MAX_VISITED: usize = 15;

// move through village: walk to occupied village pois at night (MoveThroughVillageGoal)
pub struct MoveThroughVillageGoal {
    speed: f64,
    only_at_night: bool,
    distance_to_poi: f64,
    can_deal_with_doors: fn(&dyn Mob) -> bool,
    path: Option<Path>,
    poi_pos: BlockPos,
    visited: Vec<BlockPos>,
}

impl MoveThroughVillageGoal {
    #[must_use]
    pub fn new(
        speed: f64,
        only_at_night: bool,
        distance_to_poi: i32,
        can_deal_with_doors: fn(&dyn Mob) -> bool,
    ) -> Box<Self> {
        Box::new(Self {
            speed,
            only_at_night,
            distance_to_poi: f64::from(distance_to_poi),
            can_deal_with_doors,
            path: None,
            poi_pos: BlockPos::ZERO,
            visited: Vec::with_capacity(MAX_VISITED + 1),
        })
    }

    fn has_not_visited(&self, poi: &BlockPos) -> bool {
        !self.visited.contains(poi)
    }

    fn create_path(&self, mob: &dyn Mob, destination: BlockPos) -> Option<Path> {
        let steered = mob.get_mob_entity().steered();
        let mut navigator = steered.navigator();
        navigator.set_can_open_doors((self.can_deal_with_doors)(mob));
        let path = navigator.create_path(
            &steered.mob_entity().living_entity,
            destination.to_centered_f64(),
            0,
        );
        navigator.set_can_open_doors(true);
        path
    }

    fn closer_to_center_than(&self, mob: &dyn Mob, distance: f64) -> bool {
        self.poi_pos
            .to_centered_f64()
            .squared_distance_to_vec(&mob.get_entity().pos.load())
            < distance * distance
    }
}

impl Goal for MoveThroughVillageGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if self.visited.len() > MAX_VISITED {
            self.visited.remove(0);
        }
        let world = mob.get_entity().world.load_full();
        if self.only_at_night && world.is_bright_outside() {
            return false;
        }
        let pos = mob.get_entity().block_pos.load();
        if !world.is_close_to_village(&pos, VILLAGE_SECTION_RANGE) {
            return false;
        }
        let occupancy = VillageOccupancy::gather(&world, pos, 15 + POI_SEARCH_RADIUS);
        let land_pos = land_random_pos::get_pos_weighted(mob, 15, 7, |candidate| {
            if !world.is_village(candidate) {
                return f64::NEG_INFINITY;
            }
            world
                .find_occupied_village_poi(*candidate, POI_SEARCH_RADIUS, &occupancy, |poi| {
                    self.has_not_visited(poi)
                })
                .map_or(f64::NEG_INFINITY, |poi| {
                    let delta = poi.0 - pos.0;
                    -(f64::from(delta.x).powi(2)
                        + f64::from(delta.y).powi(2)
                        + f64::from(delta.z).powi(2))
                })
        });
        let Some(land_pos) = land_pos else {
            return false;
        };
        let Some(target) = world.find_occupied_village_poi(
            land_pos.to_block_pos(),
            POI_SEARCH_RADIUS,
            &occupancy,
            |poi| self.has_not_visited(poi),
        ) else {
            return false;
        };
        self.poi_pos = target;
        let mut path = self.create_path(mob, target);
        if path.is_none() {
            let partial_step = default_random_pos::get_pos_towards(
                mob,
                10,
                7,
                pumpkin_util::math::vector3::Vector3::new(
                    f64::from(target.0.x) + 0.5,
                    f64::from(target.0.y),
                    f64::from(target.0.z) + 0.5,
                ),
                std::f64::consts::FRAC_PI_2,
            );
            let Some(partial_step) = partial_step else {
                return false;
            };
            path = self.create_path(mob, partial_step.to_block_pos());
        }
        let Some(found) = path else {
            return false;
        };
        let door_node = found.get_nodes().iter().find(|node| {
            DoorBlock::is_wooden_door(&world, &node.pos.up())
        });
        self.path = match door_node {
            Some(node) => {
                let node_pos = node.pos;
                let steered = mob.get_mob_entity().steered();
                steered.navigator().create_path(
                    &steered.mob_entity().living_entity,
                    node_pos.to_centered_f64(),
                    0,
                )
            }
            None => Some(found),
        };
        self.path.is_some()
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        if mob.is_navigator_idle() {
            return false;
        }
        let width = f64::from(mob.get_entity().width());
        !self.closer_to_center_than(mob, width + self.distance_to_poi)
    }

    fn start(&mut self, mob: &dyn Mob) {
        let steered = mob.get_mob_entity().steered();
        steered.navigator().move_to_path(
            self.path.take(),
            self.speed,
            &steered.mob_entity().living_entity,
        );
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if mob.is_navigator_idle() || self.closer_to_center_than(mob, self.distance_to_poi) {
            self.visited.push(self.poi_pos);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}
