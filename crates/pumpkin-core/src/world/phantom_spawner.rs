use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::entity::EntityType;
use pumpkin_data::fluid::Fluid;
use pumpkin_data::statistic::CustomStatistic;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    EntityBase,
    mob::{
        equipment::RegionalDifficulty,
        spawn::{SpawnGroupData, SpawnReason, finalize_spawn},
    },
    r#type::from_type,
};
use crate::world::{World, natural_spawner};

// phantom spawner: PhantomSpawner.tick
const MIN_SKY_DARKEN: i32 = 5;
const TIME_SINCE_REST_THRESHOLD: i32 = 72000;

impl World {
    // phantom spawner: PhantomSpawner.tick
    pub fn tick_phantom_spawner(self: &Arc<Self>, spawn_enemies: bool) {
        if !spawn_enemies || !self.level_info.load().game_rules.spawn_phantoms {
            return;
        }
        let next = self
            .phantom_spawner_next_tick
            .fetch_sub(1, Ordering::Relaxed)
            - 1;
        if next > 0 {
            return;
        }
        let delay = (60 + rand::rng().random_range(0..60)) * 20;
        self.phantom_spawner_next_tick
            .store(next + delay, Ordering::Relaxed);
        let has_sky_light = self.dimension.has_skylight;
        if self.get_sky_darken() < MIN_SKY_DARKEN && has_sky_light {
            return;
        }
        let players = self.players.load();
        for player in players.iter() {
            if player.get_entity().is_spectator() {
                continue;
            }
            let player_pos = player.get_entity().block_pos.load();
            if has_sky_light && (player_pos.0.y < self.sea_level || !self.can_see_sky(&player_pos))
            {
                continue;
            }
            self.try_spawn_phantoms_for(
                player_pos,
                player.get_custom_stat(CustomStatistic::TimeSinceRest),
            );
        }
    }

    fn try_spawn_phantoms_for(self: &Arc<Self>, player_pos: BlockPos, time_since_rest: i32) {
        let difficulty = RegionalDifficulty::at(
            self,
            Vector3::new(
                f64::from(player_pos.0.x),
                f64::from(player_pos.0.y),
                f64::from(player_pos.0.z),
            ),
        );
        let (harder, rest_roll, spawn_pos, group_size) = {
            let mut rng = rand::rng();
            let harder = difficulty.is_harder_than(rng.random::<f32>() * 3.0);
            let rest_roll = rng.random_range(0..time_since_rest.max(1));
            let spawn_pos = player_pos.add(
                -10 + rng.random_range(0..21),
                20 + rng.random_range(0..15),
                -10 + rng.random_range(0..21),
            );
            let group_size = 1 + rng.random_range(0..=difficulty.difficulty_id());
            (harder, rest_roll, spawn_pos, group_size)
        };
        if !harder || rest_roll < TIME_SINCE_REST_THRESHOLD {
            return;
        }
        let state = self.get_block_state(&spawn_pos);
        if !natural_spawner::is_valid_empty_spawn_block(state, &EntityType::PHANTOM)
            || *self.get_fluid(&spawn_pos) != Fluid::EMPTY
        {
            return;
        }
        let mut group_data: Option<SpawnGroupData> = None;
        for _ in 0..group_size {
            let phantom = from_type(
                &EntityType::PHANTOM,
                Vector3::new(
                    f64::from(spawn_pos.0.x) + 0.5,
                    f64::from(spawn_pos.0.y),
                    f64::from(spawn_pos.0.z) + 0.5,
                ),
                self,
                Uuid::new_v4(),
            );
            group_data = finalize_spawn(&phantom, self, group_data, SpawnReason::Natural);
            self.spawn_entity(phantom as Arc<dyn EntityBase>);
        }
    }
}
