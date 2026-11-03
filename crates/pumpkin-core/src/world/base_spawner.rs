use std::sync::{Arc, Mutex, PoisonError};

use pumpkin_data::{
    entity::{EntityStatus, EntityType},
    world::WorldEvent,
};
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use rand::RngExt;

use crate::entity::{
    EntityBase,
    mob::spawn::{SpawnReason, finalize_spawn},
    r#type::{check_spawn_rules, load_entity},
};
use crate::world::{World, natural_spawner};

// base spawner: BaseSpawner defaults
const DEFAULT_SPAWN_DELAY: i32 = 20;
const DEFAULT_MIN_SPAWN_DELAY: i32 = 200;
const DEFAULT_MAX_SPAWN_DELAY: i32 = 800;
const DEFAULT_SPAWN_COUNT: i32 = 4;
const DEFAULT_MAX_NEARBY_ENTITIES: i32 = 6;
const DEFAULT_REQUIRED_PLAYER_RANGE: i32 = 16;
const DEFAULT_SPAWN_RANGE: i32 = 4;
// base spawner: new SpawnData() spawns a pig
const DEFAULT_ENTITY_ID: &str = "minecraft:pig";
/// Vanilla `BaseSpawner.EVENT_SPAWN`.
pub const EVENT_SPAWN: i32 = 1;

struct SpawnerState {
    spawn_delay: i32,
    /// `SpawnPotentials`: weight and `SpawnData` compound.
    spawn_potentials: Vec<(i32, NbtCompound)>,
    next_spawn_data: Option<NbtCompound>,
    min_spawn_delay: i32,
    max_spawn_delay: i32,
    spawn_count: i32,
    max_nearby_entities: i32,
    required_player_range: i32,
    spawn_range: i32,
}

/// Vanilla `BaseSpawner`: spawn timing, `SpawnData` and `SpawnPotentials` for a spawner that is
/// not tied to one block entity type.
pub struct BaseSpawner {
    state: Mutex<SpawnerState>,
}

impl Default for BaseSpawner {
    fn default() -> Self {
        Self::new()
    }
}

fn default_spawn_data() -> NbtCompound {
    let mut entity = NbtCompound::new();
    entity.put_string("id", DEFAULT_ENTITY_ID.to_string());
    let mut data = NbtCompound::new();
    data.put_compound("entity", entity);
    data
}

fn get_number(nbt: &NbtCompound, name: &str) -> Option<i32> {
    nbt.get_short(name)
        .map(i32::from)
        .or_else(|| nbt.get_int(name))
        .or_else(|| nbt.get_byte(name).map(i32::from))
}

// custom spawn rules: InclusiveRange as `[min, max]` or `{min_inclusive, max_inclusive}`
fn inclusive_range_contains(tag: &NbtTag, value: i32) -> bool {
    let (min, max) = if let Some([min, max]) = tag.extract_int_array() {
        (*min, *max)
    } else if let Some([min, max]) = tag.extract_list() {
        match (min, max) {
            (NbtTag::Int(min), NbtTag::Int(max)) => (*min, *max),
            _ => return true,
        }
    } else if let Some(range) = tag.extract_compound() {
        (
            range.get_int("min_inclusive").unwrap_or(0),
            range.get_int("max_inclusive").unwrap_or(15),
        )
    } else {
        return true;
    };
    (min..=max).contains(&value)
}

impl BaseSpawner {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SpawnerState {
                spawn_delay: DEFAULT_SPAWN_DELAY,
                spawn_potentials: Vec::new(),
                next_spawn_data: None,
                min_spawn_delay: DEFAULT_MIN_SPAWN_DELAY,
                max_spawn_delay: DEFAULT_MAX_SPAWN_DELAY,
                spawn_count: DEFAULT_SPAWN_COUNT,
                max_nearby_entities: DEFAULT_MAX_NEARBY_ENTITIES,
                required_player_range: DEFAULT_REQUIRED_PLAYER_RANGE,
                spawn_range: DEFAULT_SPAWN_RANGE,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SpawnerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // base spawner: setEntityId
    pub fn set_entity_id(&self, entity_type: &'static EntityType) {
        let mut state = self.lock();
        let data = Self::get_or_create_next_spawn_data(&mut state);
        let mut entity = data.get_compound("entity").cloned().unwrap_or_default();
        entity.put_string("id", format!("minecraft:{}", entity_type.resource_name));
        data.put_compound("entity", entity);
    }

    // base spawner: getOrCreateNextSpawnData
    fn get_or_create_next_spawn_data(state: &mut SpawnerState) -> &mut NbtCompound {
        if state.next_spawn_data.is_none() {
            let picked =
                Self::pick_potential(&state.spawn_potentials).unwrap_or_else(default_spawn_data);
            state.next_spawn_data = Some(picked);
        }
        state.next_spawn_data.get_or_insert_with(default_spawn_data)
    }

    // base spawner: WeightedList.getRandom
    fn pick_potential(potentials: &[(i32, NbtCompound)]) -> Option<NbtCompound> {
        let total: i32 = potentials.iter().map(|(weight, _)| (*weight).max(0)).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = rand::rng().random_range(0..total);
        for (weight, data) in potentials {
            let weight = (*weight).max(0);
            if roll < weight {
                return Some(data.clone());
            }
            roll -= weight;
        }
        None
    }

    // base spawner: isNearPlayer, hasNearbyAlivePlayer
    fn is_near_player(world: &World, pos: &BlockPos, range: i32) -> bool {
        let center = pos.to_centered_f64();
        let range_sq = f64::from(range) * f64::from(range);
        world.players.load().iter().any(|player| {
            let entity = player.get_entity();
            entity.is_alive()
                && !player.is_spectator()
                && entity.pos.load().squared_distance_to_vec(&center) < range_sq
        })
    }

    // base spawner: delay
    fn delay(&self, pos: &BlockPos, broadcast: &dyn Fn(i32)) {
        {
            let mut state = self.lock();
            state.spawn_delay = if state.max_spawn_delay <= state.min_spawn_delay {
                state.min_spawn_delay
            } else {
                state.min_spawn_delay
                    + rand::rng().random_range(0..state.max_spawn_delay - state.min_spawn_delay)
            };
            if let Some(next) = Self::pick_potential(&state.spawn_potentials) {
                state.next_spawn_data = Some(next);
            }
        }
        let _ = pos;
        broadcast(EVENT_SPAWN);
    }

    // base spawner: CustomSpawnRules.isValidPosition
    fn custom_rules_allow(rules: &NbtCompound, world: &World, pos: &BlockPos) -> bool {
        let block_light = i32::from(world.get_block_light_level(pos).unwrap_or(0));
        let sky_light = i32::from(world.get_sky_light_level(pos));
        rules
            .child_tags
            .get("block_light_limit")
            .is_none_or(|range| inclusive_range_contains(range, block_light))
            && rules
                .child_tags
                .get("sky_light_limit")
                .is_none_or(|range| inclusive_range_contains(range, sky_light))
    }

    // base spawner: serverTick, `broadcast` is BaseSpawner.broadcastEvent
    #[expect(clippy::too_many_lines)]
    pub fn server_tick(&self, world: &Arc<World>, pos: BlockPos, broadcast: &dyn Fn(i32)) {
        let range = self.lock().required_player_range;
        if !Self::is_near_player(world, &pos, range)
            || !world.level_info.load().game_rules.spawner_blocks_work
        {
            return;
        }
        if self.lock().spawn_delay == -1 {
            self.delay(&pos, broadcast);
        }
        {
            let mut state = self.lock();
            if state.spawn_delay > 0 {
                state.spawn_delay -= 1;
                return;
            }
        }

        let (next_data, spawn_count, max_nearby, spawn_range) = {
            let mut state = self.lock();
            let data = Self::get_or_create_next_spawn_data(&mut state).clone();
            (
                data,
                state.spawn_count,
                state.max_nearby_entities,
                state.spawn_range,
            )
        };
        let entity_nbt = next_data
            .get_compound("entity")
            .cloned()
            .unwrap_or_default();
        let custom_rules = next_data.get_compound("custom_spawn_rules");
        let mut delayed = false;
        for _ in 0..spawn_count {
            let Some(entity_type) = entity_nbt
                .get_string("id")
                .and_then(|id| EntityType::from_name(id.strip_prefix("minecraft:").unwrap_or(id)))
            else {
                self.delay(&pos, broadcast);
                return;
            };
            let spawn_pos = if let Some([NbtTag::Double(x), NbtTag::Double(y), NbtTag::Double(z)]) =
                entity_nbt.get_list("Pos")
            {
                Vector3::new(*x, *y, *z)
            } else {
                let mut rng = rand::rng();
                Vector3::new(
                    f64::from(pos.0.x)
                        + (rng.random::<f64>() - rng.random::<f64>()) * f64::from(spawn_range)
                        + 0.5,
                    f64::from(pos.0.y + rng.random_range(0..3) - 1),
                    f64::from(pos.0.z)
                        + (rng.random::<f64>() - rng.random::<f64>()) * f64::from(spawn_range)
                        + 0.5,
                )
            };
            if !world.is_space_empty(entity_type.get_spawn_bounding_box(
                spawn_pos.x,
                spawn_pos.y,
                spawn_pos.z,
            )) {
                continue;
            }
            let spawn_block = BlockPos::floored_v(spawn_pos);
            if let Some(rules) = custom_rules {
                if !entity_type.category.is_friendly
                    && world.level_info.load().difficulty == pumpkin_util::Difficulty::Peaceful
                {
                    continue;
                }
                if !Self::custom_rules_allow(rules, world, &spawn_block) {
                    continue;
                }
            } else if !natural_spawner::is_spawn_position_ok(world, &spawn_block, entity_type)
                || !check_spawn_rules(entity_type, world, &spawn_block, world.is_thundering())
            {
                continue;
            }

            let entity = load_entity(entity_type, spawn_pos, &entity_nbt, world);
            entity.get_entity().set_pos(spawn_pos);

            let area = BoundingBox::new(
                Vector3::new(
                    f64::from(pos.0.x) - f64::from(spawn_range),
                    f64::from(pos.0.y) - f64::from(spawn_range),
                    f64::from(pos.0.z) - f64::from(spawn_range),
                ),
                Vector3::new(
                    f64::from(pos.0.x) + 1.0 + f64::from(spawn_range),
                    f64::from(pos.0.y) + 1.0 + f64::from(spawn_range),
                    f64::from(pos.0.z) + 1.0 + f64::from(spawn_range),
                ),
            );
            let nearby = world
                .get_entities_at_box(&area)
                .iter()
                .filter(|other| {
                    other.get_entity().entity_type.id == entity_type.id && !other.is_spectator()
                })
                .count();
            if nearby as i32 >= max_nearby {
                self.delay(&pos, broadcast);
                return;
            }

            entity
                .get_entity()
                .set_rotation(rand::rng().random::<f32>() * 360.0, 0.0);
            if let Some(mob) = entity.get_mob() {
                if !mob.check_spawn_obstruction(world) {
                    continue;
                }
                // only a bare `{id}` entry gets the regular finalize step
                if entity_nbt.child_tags.len() == 1 {
                    finalize_spawn(&entity, world, None, SpawnReason::Spawner);
                }
            }

            let mut event =
                crate::plugin::api::events::entity::spawner_spawn::SpawnerSpawnEvent::new(
                    entity.get_entity().entity_id,
                    pos,
                );
            if let Some(server) = world.server.upgrade() {
                server.plugin_manager.fire_blocking(&server, &mut event);
            }
            if event.cancelled {
                delayed = true;
                continue;
            }

            if !world.spawn_entity(entity.clone()) {
                self.delay(&pos, broadcast);
                return;
            }
            world.sync_world_event(WorldEvent::ParticlesMobblockSpawn, pos, 0);
            world.emit_game_event("entity_place", spawn_block.to_centered_f64());
            if entity.get_mob().is_some() {
                // Mob.spawnAnim
                world.send_entity_status(
                    entity.get_entity(),
                    EntityStatus::SilverfishMergeAnim,
                    pumpkin_protocol::bedrock::server::actor_event::ActorEventID::None,
                );
            }
            delayed = true;
        }
        if delayed {
            self.delay(&pos, broadcast);
        }
    }

    // base spawner: onEventTriggered (client only changes the visual delay)
    #[must_use]
    pub const fn on_event_triggered(&self, id: i32) -> bool {
        id == EVENT_SPAWN
    }

    // base spawner: load
    pub fn load(&self, nbt: &NbtCompound) {
        let mut state = self.lock();
        state.spawn_delay = get_number(nbt, "Delay").unwrap_or(DEFAULT_SPAWN_DELAY);
        state.next_spawn_data = nbt.get_compound("SpawnData").cloned();
        state.spawn_potentials = nbt
            .get_list("SpawnPotentials")
            .map(|list| {
                list.iter()
                    .filter_map(|tag| {
                        let entry = tag.extract_compound()?;
                        let data = entry.get_compound("data")?.clone();
                        Some((get_number(entry, "weight").unwrap_or(1), data))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if state.spawn_potentials.is_empty() {
            let single = state
                .next_spawn_data
                .clone()
                .unwrap_or_else(default_spawn_data);
            state.spawn_potentials = vec![(1, single)];
        }
        state.min_spawn_delay = get_number(nbt, "MinSpawnDelay").unwrap_or(DEFAULT_MIN_SPAWN_DELAY);
        state.max_spawn_delay = get_number(nbt, "MaxSpawnDelay").unwrap_or(DEFAULT_MAX_SPAWN_DELAY);
        state.spawn_count = get_number(nbt, "SpawnCount").unwrap_or(DEFAULT_SPAWN_COUNT);
        state.max_nearby_entities =
            get_number(nbt, "MaxNearbyEntities").unwrap_or(DEFAULT_MAX_NEARBY_ENTITIES);
        state.required_player_range =
            get_number(nbt, "RequiredPlayerRange").unwrap_or(DEFAULT_REQUIRED_PLAYER_RANGE);
        state.spawn_range = get_number(nbt, "SpawnRange").unwrap_or(DEFAULT_SPAWN_RANGE);
    }

    // base spawner: save
    pub fn save(&self, nbt: &mut NbtCompound) {
        let state = self.lock();
        nbt.put_short("Delay", state.spawn_delay as i16);
        nbt.put_short("MinSpawnDelay", state.min_spawn_delay as i16);
        nbt.put_short("MaxSpawnDelay", state.max_spawn_delay as i16);
        nbt.put_short("SpawnCount", state.spawn_count as i16);
        nbt.put_short("MaxNearbyEntities", state.max_nearby_entities as i16);
        nbt.put_short("RequiredPlayerRange", state.required_player_range as i16);
        nbt.put_short("SpawnRange", state.spawn_range as i16);
        if let Some(next) = &state.next_spawn_data {
            nbt.put_compound("SpawnData", next.clone());
        }
        let potentials = state
            .spawn_potentials
            .iter()
            .map(|(weight, data)| {
                let mut entry = NbtCompound::new();
                entry.put_int("weight", *weight);
                entry.put_compound("data", data.clone());
                NbtTag::Compound(entry)
            })
            .collect();
        nbt.put_list("SpawnPotentials", potentials);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawner_data_round_trips_through_nbt() {
        let spawner = BaseSpawner::new();
        let mut source = NbtCompound::new();
        source.put_short("Delay", 7);
        source.put_short("SpawnCount", 2);
        source.put_compound("SpawnData", default_spawn_data());
        spawner.load(&source);
        let mut saved = NbtCompound::new();
        spawner.save(&mut saved);
        assert_eq!(saved.get_short("Delay"), Some(7));
        assert_eq!(saved.get_short("SpawnCount"), Some(2));
        assert_eq!(saved.get_short("MaxSpawnDelay"), Some(800));
        assert_eq!(saved.get_compound("SpawnData"), Some(&default_spawn_data()));
        assert_eq!(saved.get_list("SpawnPotentials").map(<[_]>::len), Some(1));
    }
}
