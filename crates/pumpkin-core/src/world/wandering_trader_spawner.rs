use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_data::entity::EntityType;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::HeightMap;
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::chunk::ChunkHeightmapType;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    EntityBase,
    mob::spawn::{SpawnReason, finalize_spawn},
    passive::wandering_trader::WanderingTraderEntity,
    r#type::from_type,
};
use crate::world::{World, natural_spawner};

// wandering trader spawner: WanderingTraderSpawner constants
const DEFAULT_TICK_DELAY: i32 = 1200;
const DEFAULT_SPAWN_DELAY: i32 = 24000;
const MIN_SPAWN_CHANCE: i32 = 25;
const MAX_SPAWN_CHANCE: i32 = 75;
const SPAWN_CHANCE_INCREASE: i32 = 25;
const SPAWN_ONE_IN_X_CHANCE: i32 = 10;
const NUMBER_OF_SPAWN_ATTEMPTS: i32 = 10;
const SPAWN_RADIUS: i32 = 48;
const TRADER_DESPAWN_DELAY: i32 = 48000;
const TRADER_HOME_RADIUS: i32 = 16;
const LLAMA_RADIUS: i32 = 4;
const LLAMA_COUNT: i32 = 2;

/// Spawner timers, saved in `wandering_trader.dat`.
pub struct SpawnerState {
    tick_delay: i32,
    pub spawn_delay: i32,
    pub spawn_chance: i32,
}

impl SpawnerState {
    #[must_use]
    pub const fn new(spawn_delay: i32, spawn_chance: i32) -> Self {
        Self {
            tick_delay: DEFAULT_TICK_DELAY,
            spawn_delay,
            spawn_chance,
        }
    }
}

impl World {
    // wandering trader spawner: WanderingTraderSpawner.tick, overworld only
    pub fn tick_wandering_trader_spawner(self: &Arc<Self>) {
        if self.dimension != Dimension::OVERWORLD
            || !self.level_info.load().game_rules.spawn_wandering_traders
        {
            return;
        }
        let chance = {
            let mut state = self
                .wandering_trader_spawner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.tick_delay -= 1;
            if state.tick_delay > 0 {
                return;
            }
            state.tick_delay = DEFAULT_TICK_DELAY;
            state.spawn_delay -= DEFAULT_TICK_DELAY;
            if state.spawn_delay > 0 {
                return;
            }
            state.spawn_delay = DEFAULT_SPAWN_DELAY;
            let chance = state.spawn_chance;
            state.spawn_chance =
                (chance + SPAWN_CHANCE_INCREASE).clamp(MIN_SPAWN_CHANCE, MAX_SPAWN_CHANCE);
            chance
        };
        if rand::rng().random_range(0..100) <= chance && self.spawn_wandering_trader() {
            self.wandering_trader_spawner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .spawn_chance = MIN_SPAWN_CHANCE;
        }
    }

    // wandering trader spawner: WanderingTraderSpawner.spawn
    fn spawn_wandering_trader(self: &Arc<Self>) -> bool {
        let players = self.players.load();
        if players.is_empty() {
            return true;
        }
        let player = {
            let mut rng = rand::rng();
            if rng.random_range(0..SPAWN_ONE_IN_X_CHANCE) != 0 {
                return false;
            }
            players[rng.random_range(0..players.len())].clone()
        };
        let player_pos = player.get_entity().block_pos.load();
        let reference = self.find_meeting_point(player_pos).unwrap_or(player_pos);
        let Some(spawn_position) = self.find_spawn_position_near(&reference, SPAWN_RADIUS) else {
            return false;
        };
        if !self.has_enough_space(&spawn_position) {
            return false;
        }
        if self
            .get_biome(&spawn_position)
            .has_tag(&tag::WorldgenBiome::MINECRAFT_WITHOUT_WANDERING_TRADER_SPAWNS)
        {
            return false;
        }
        let trader = from_type(
            &EntityType::WANDERING_TRADER,
            spawn_position.to_centered_f64(),
            self,
            Uuid::new_v4(),
        );
        finalize_spawn(&trader, self, None, SpawnReason::Event);
        let Some(trader_mob) = trader.get_mob() else {
            return false;
        };
        let Some(trader_entity) = trader_mob
            .cast_any()
            .downcast_ref::<WanderingTraderEntity>()
        else {
            return false;
        };
        trader_entity.set_despawn_delay(TRADER_DESPAWN_DELAY);
        trader_entity.set_wander_target(Some(reference));
        let mob_entity = trader_mob.get_mob_entity();
        mob_entity.position_target.store(reference);
        mob_entity
            .position_target_range
            .store(TRADER_HOME_RADIUS, std::sync::atomic::Ordering::Relaxed);
        self.spawn_entity(trader.clone());
        for _ in 0..LLAMA_COUNT {
            self.try_to_spawn_llama_for(&trader, LLAMA_RADIUS);
        }
        true
    }

    // wandering trader spawner: poiManager.find(MEETING, ANY)
    fn find_meeting_point(&self, origin: BlockPos) -> Option<BlockPos> {
        let mut best: Option<(i32, BlockPos)> = None;
        self.portal_poi
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .for_each_in_square(origin, SPAWN_RADIUS, |poi_pos, poi_type| {
                if poi_type == "minecraft:meeting" {
                    let distance = origin.squared_distance(&poi_pos);
                    if best.is_none_or(|(current, _)| distance < current) {
                        best = Some((distance, poi_pos));
                    }
                }
            });
        best.map(|(_, pos)| pos)
    }

    // wandering trader spawner: tryToSpawnLlamaFor
    fn try_to_spawn_llama_for(self: &Arc<Self>, trader: &Arc<dyn EntityBase>, radius: i32) {
        let Some(spawn_position) =
            self.find_spawn_position_near(&trader.get_entity().block_pos.load(), radius)
        else {
            return;
        };
        let llama = from_type(
            &EntityType::TRADER_LLAMA,
            spawn_position.to_centered_f64(),
            self,
            Uuid::new_v4(),
        );
        finalize_spawn(&llama, self, None, SpawnReason::Event);
        if self.spawn_entity(llama.clone()) {
            llama.get_entity().leash_to(trader.clone());
        }
    }

    // wandering trader spawner: findSpawnPositionNear
    fn find_spawn_position_near(
        self: &Arc<Self>,
        reference: &BlockPos,
        radius: i32,
    ) -> Option<BlockPos> {
        let mut rng = rand::rng();
        (0..NUMBER_OF_SPAWN_ATTEMPTS).find_map(|_| {
            let x = reference.0.x + rng.random_range(0..radius * 2) - radius;
            let z = reference.0.z + rng.random_range(0..radius * 2) - radius;
            let heightmap = match EntityType::WANDERING_TRADER.spawn_restriction.heightmap {
                HeightMap::WorldSurface | HeightMap::WorldSurfaceWg => {
                    ChunkHeightmapType::WorldSurface
                }
                HeightMap::MotionBlocking => ChunkHeightmapType::MotionBlocking,
                _ => ChunkHeightmapType::MotionBlockingNoLeaves,
            };
            let y = self.get_heightmap_height(heightmap, x, z);
            let pos = BlockPos::new(x, y, z);
            natural_spawner::is_spawn_position_ok(self, &pos, &EntityType::WANDERING_TRADER)
                .then_some(pos)
        })
    }

    // wandering trader spawner: hasEnoughSpace, 2x3x2 without collision
    fn has_enough_space(&self, spawn_pos: &BlockPos) -> bool {
        (0..=1).all(|dx| {
            (0..=2).all(|dy| {
                (0..=1).all(|dz| {
                    let pos =
                        BlockPos::new(spawn_pos.0.x + dx, spawn_pos.0.y + dy, spawn_pos.0.z + dz);
                    self.get_block_state(&pos).collision_shapes.is_empty()
                })
            })
        })
    }
}
