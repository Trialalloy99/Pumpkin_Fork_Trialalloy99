use pumpkin_data::structures::{
    ConcentricRingsStructurePlacement, RandomSpreadStructurePlacement, StructurePlacement,
    StructurePlacementType, StructureSet,
};
use pumpkin_util::math::{floor_div, position::BlockPos};

use crate::generation::structure::placement::{
    GlobalStructureCache, StrongholdLayout, get_structure_chunk_in_region,
};

use super::WorldGenerator;

/// Block-level position of a found structure plus squared distance from the
/// search origin, used internally to track the running nearest candidate.
#[derive(Debug, Clone)]
pub struct FoundStructure {
    pub pos: BlockPos,
    pub distance_sq: f64,
}

/// Finds the block position of the nearest structure whose placement is listed
/// in `placements`, within `max_search_radius` chunk-region rings.
///
/// Mirrors the two-pass logic in vanilla's
/// `ChunkGenerator.findNearestMapStructure`:
///
/// 1. **Concentric-rings** placements (strongholds) are resolved in one pass
///    from the pre-computed [`GlobalStructureCache`].
/// 2. **Random-spread** placements are searched ring-by-ring outward, stopping
///    at the first radius that produces any result.
///
/// The best candidate from both passes is returned.
///
/// Returns [`StructureSearch::Pending`] while the stronghold positions are still
/// being computed, so the caller can retry instead of reporting a wrong result.
pub fn find_nearest_structure(
    origin: BlockPos,
    placements: &[&StructurePlacement],
    max_search_radius: i32,
    world_seed: i64,
    global_cache: &GlobalStructureCache,
) -> StructureSearch {
    if placements.is_empty() {
        return StructureSearch::NotFound;
    }

    let mut nearest: Option<FoundStructure> = None;

    // ── Pass 1: Concentric-rings (strongholds) ──────────────────────────────
    for p in placements {
        if let StructurePlacementType::ConcentricRings(rings) = &p.placement_type {
            match find_nearest_concentric(origin, rings, global_cache) {
                ConcentricSearch::Pending => return StructureSearch::Pending,
                ConcentricSearch::Done(Some(found))
                    if nearest
                        .as_ref()
                        .is_none_or(|n| found.distance_sq < n.distance_sq) =>
                {
                    nearest = Some(found);
                }
                ConcentricSearch::Done(_) => {}
            }
        }
    }

    let random_spread: Vec<(&RandomSpreadStructurePlacement, u32)> = placements
        .iter()
        .filter_map(|p| {
            if let StructurePlacementType::RandomSpread(r) = &p.placement_type {
                Some((r, p.salt))
            } else {
                None
            }
        })
        .collect();

    if !random_spread.is_empty() {
        let chunk_origin_x = origin.0.x >> 4;
        let chunk_origin_z = origin.0.z >> 4;

        'radius: for radius in 0..=max_search_radius {
            for (placement, salt) in &random_spread {
                if let Some(found) = find_nearest_random_spread_at_radius(
                    origin,
                    chunk_origin_x,
                    chunk_origin_z,
                    radius,
                    world_seed,
                    placement,
                    *salt,
                ) {
                    if nearest
                        .as_ref()
                        .is_none_or(|n| found.distance_sq < n.distance_sq)
                    {
                        nearest = Some(found);
                    }
                    break 'radius;
                }
            }
        }
    }

    nearest.map_or(StructureSearch::NotFound, |f| StructureSearch::Found(f.pos))
}

/// Result of [`find_nearest_structure`].
#[derive(Debug, Clone)]
pub enum StructureSearch {
    Found(BlockPos),
    NotFound,
    Pending,
}

/// Finds the nearest candidate that actually produces one of `target_structures`.
///
/// Returns the position along with which of the targets was found. Explorer maps
/// and `/locate structure` use this instead of pointing at a placement-only
/// candidate whose biome may reject the requested structure.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn find_nearest_structure_start(
    origin: BlockPos,
    structure_set: &StructureSet,
    target_structures: &[pumpkin_data::structures::StructureKeys],
    max_search_radius: i32,
    generator: &WorldGenerator,
) -> Option<(BlockPos, pumpkin_data::structures::StructureKeys)> {
    use crate::{
        biome::{BiomeSupplier, MultiNoiseBiomeSupplier},
        generation::{
            noise::router::multi_noise_sampler::MultiNoiseSampler,
            structure::{
                height_sampler::NoiseHeightSampler,
                lazily_generate_structure,
                placement::should_generate_structure,
                structures::{StructureGeneratorContext, create_chunk_random},
            },
        },
    };
    use pumpkin_data::structures::Structure;

    let WorldGenerator::Noise(noise_generator) = generator else {
        return None;
    };
    let StructurePlacementType::RandomSpread(placement) = &structure_set.placement.placement_type
    else {
        return None;
    };

    let chunk_origin_x = origin.0.x >> 4;
    let chunk_origin_z = origin.0.z >> 4;
    let region_origin_x = floor_div(chunk_origin_x, placement.spacing);
    let region_origin_z = floor_div(chunk_origin_z, placement.spacing);
    let world_seed = noise_generator.random_config.seed as i64;
    let global_cache = &noise_generator.global_structure_cache;

    for radius in 0..=max_search_radius {
        let mut nearest: Option<(FoundStructure, pumpkin_data::structures::StructureKeys)> = None;
        for region_x_offset in -radius..=radius {
            for region_z_offset in -radius..=radius {
                if region_x_offset.abs() != radius && region_z_offset.abs() != radius {
                    continue;
                }
                let (chunk_x, chunk_z) = get_structure_chunk_in_region(
                    placement,
                    world_seed,
                    region_origin_x + region_x_offset,
                    region_origin_z + region_z_offset,
                    structure_set.placement.salt,
                );
                if !should_generate_structure(
                    &structure_set.placement,
                    &noise_generator.structure_calculator,
                    chunk_x,
                    chunk_z,
                    global_cache,
                ) {
                    continue;
                }

                for &key in target_structures {
                    let start =
                        global_cache.get_or_compute_structure_start(key, chunk_x, chunk_z, || {
                            let settings = noise_generator.settings;
                            // locate: same height sampler as chunk generation (cached starts are shared)
                            let mut height_sampler = NoiseHeightSampler::new(noise_generator);
                            let mut biome_sampler = MultiNoiseSampler::generate(
                                &noise_generator.base_router.multi_noise,
                            );
                            let biome_supplier: &dyn BiomeSupplier =
                                &MultiNoiseBiomeSupplier::OVERWORLD;
                            let context = StructureGeneratorContext {
                                seed: world_seed,
                                chunk_x,
                                chunk_z,
                                random: create_chunk_random(world_seed, chunk_x, chunk_z),
                                sea_level: settings.sea_level,
                                min_y: (settings.shape.min_y as i32)
                                    .max(noise_generator.dimension.min_y),
                                height: settings
                                    .shape
                                    .height
                                    .min(noise_generator.dimension.height as u16),
                                height_sampler: Some(&mut height_sampler),
                                structure_key: Some(key),
                            };
                            lazily_generate_structure(
                                &key,
                                Structure::get(&key),
                                context,
                                biome_supplier,
                                &mut biome_sampler,
                            )
                        });
                    let Some(_start) = start else {
                        continue;
                    };
                    // Vanilla getLocatePos: chunkPos.getMinBlockX(), 0, chunkPos.getMinBlockZ()
                    let locate_x = chunk_x << 4;
                    let locate_z = chunk_z << 4;
                    let position = BlockPos::new(locate_x, 0, locate_z);
                    let dx = f64::from(position.0.x - origin.0.x);
                    let dz = f64::from(position.0.z - origin.0.z);
                    let found = FoundStructure {
                        pos: position,
                        distance_sq: dx * dx + dz * dz,
                    };
                    if nearest
                        .as_ref()
                        .is_none_or(|(current, _)| found.distance_sq < current.distance_sq)
                    {
                        nearest = Some((found, key));
                    }
                }
            }
        }
        if let Some((found, key)) = nearest {
            return Some((found.pos, key));
        }
    }
    None
}

fn find_nearest_concentric(
    origin: BlockPos,
    // Kept for potential future bounds / distance validation.
    _rings: &ConcentricRingsStructurePlacement,
    global_cache: &GlobalStructureCache,
) -> ConcentricSearch {
    // No block, callers run in server tasks and the tick, which shutdown waits on
    let strongholds = match global_cache.try_get_stronghold_chunks() {
        StrongholdLayout::Ready(strongholds) => strongholds,
        StrongholdLayout::Pending => return ConcentricSearch::Pending,
        StrongholdLayout::Failed => return ConcentricSearch::Done(None),
    };

    let ox = origin.0.x as f64;
    let oz = origin.0.z as f64;

    strongholds
        .iter()
        .map(|(cx, cz)| {
            // Vanilla getLocatePos: chunkPos.getMinBlockX(), 0, chunkPos.getMinBlockZ()
            let bx = cx << 4;
            let bz = cz << 4;
            let dx = bx as f64 - ox;
            let dz = bz as f64 - oz;
            FoundStructure {
                pos: BlockPos::new(bx, 0, bz),
                distance_sq: dx * dx + dz * dz,
            }
        })
        .min_by(|a, b| a.distance_sq.total_cmp(&b.distance_sq))
        .map_or(ConcentricSearch::Done(None), |found| {
            ConcentricSearch::Done(Some(found))
        })
}

/// Outcome of a stronghold lookup: the ring positions may still be computing.
enum ConcentricSearch {
    Pending,
    Done(Option<FoundStructure>),
}

fn find_nearest_random_spread_at_radius(
    origin: BlockPos,
    chunk_origin_x: i32,
    chunk_origin_z: i32,
    radius: i32,
    world_seed: i64,
    placement: &RandomSpreadStructurePlacement,
    salt: u32,
) -> Option<FoundStructure> {
    let spacing = placement.spacing;
    let ox = origin.0.x as f64;
    let oz = origin.0.z as f64;

    let mut best: Option<FoundStructure> = None;

    for rx_off in -radius..=radius {
        for rz_off in -radius..=radius {
            if rx_off.abs() != radius && rz_off.abs() != radius {
                continue;
            }

            let rx = floor_div(chunk_origin_x, spacing) + rx_off;
            let rz = floor_div(chunk_origin_z, spacing) + rz_off;

            let (struct_cx, struct_cz) =
                get_structure_chunk_in_region(placement, world_seed, rx, rz, salt);

            // Vanilla getLocatePos: chunkPos.getMinBlockX(), 0, chunkPos.getMinBlockZ()
            let bx = struct_cx << 4;
            let bz = struct_cz << 4;
            let dx = bx as f64 - ox;
            let dz = bz as f64 - oz;
            let dist_sq = dx * dx + dz * dz;

            if best.as_ref().is_none_or(|b| dist_sq < b.distance_sq) {
                best = Some(FoundStructure {
                    pos: BlockPos::new(bx, 0, bz),
                    distance_sq: dist_sq,
                });
            }
        }
    }

    best
}

// spawn: sets with a structure that overrides spawns (StructureSpawnOverride)
fn spawn_override_sets() -> &'static [usize] {
    static SETS: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
    SETS.get_or_init(|| {
        StructureSet::ALL
            .iter()
            .enumerate()
            .filter(|(_, set)| {
                matches!(
                    set.placement.placement_type,
                    StructurePlacementType::RandomSpread(_)
                ) && set.structures.iter().any(|entry| {
                    !pumpkin_data::structures::Structure::get(&entry.structure)
                        .spawn_overrides
                        .is_empty()
                })
            })
            .map(|(index, _)| index)
            .collect()
    })
}

/// Spawn list of the structure at `pos` for `category`, `None` when no structure overrides it
/// (vanilla `ChunkGenerator.getMobsAt` structure part).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn structure_spawns_at(
    generator: &WorldGenerator,
    pos: &BlockPos,
    category: &str,
) -> Option<&'static [pumpkin_data::structures::SpawnEntry]> {
    use crate::{
        biome::BiomeSupplier,
        generation::{
            noise::router::multi_noise_sampler::MultiNoiseSampler,
            structure::{
                height_sampler::NoiseHeightSampler,
                lazily_generate_structure,
                placement::should_generate_structure,
                structures::{StructureGeneratorContext, create_chunk_random},
            },
        },
    };
    use pumpkin_data::structures::{BoundingBoxType, Structure};

    let WorldGenerator::Noise(generator) = generator else {
        return None;
    };
    let seed = generator.random_config.seed as i64;
    let global_cache = &generator.global_structure_cache;
    let (pos_chunk_x, pos_chunk_z) = (pos.0.x >> 4, pos.0.z >> 4);

    for &set_index in spawn_override_sets() {
        if !generator.enabled_structure_sets.contains(&set_index) {
            continue;
        }
        let set = &StructureSet::ALL[set_index];
        let StructurePlacementType::RandomSpread(spread) = &set.placement.placement_type else {
            continue;
        };
        let min_rx = floor_div(pos_chunk_x - 8, spread.spacing);
        let max_rx = floor_div(pos_chunk_x + 8, spread.spacing);
        let min_rz = floor_div(pos_chunk_z - 8, spread.spacing);
        let max_rz = floor_div(pos_chunk_z + 8, spread.spacing);
        for rx in min_rx..=max_rx {
            for rz in min_rz..=max_rz {
                let (chunk_x, chunk_z) =
                    get_structure_chunk_in_region(spread, seed, rx, rz, set.placement.salt);
                if (chunk_x - pos_chunk_x).abs() > 8
                    || (chunk_z - pos_chunk_z).abs() > 8
                    || !should_generate_structure(
                        &set.placement,
                        &generator.structure_calculator,
                        chunk_x,
                        chunk_z,
                        global_cache,
                    )
                {
                    continue;
                }
                let entries = if set.structures.len() == 1 {
                    set.structures.to_vec()
                } else {
                    crate::generation::proto_chunk::ProtoChunk::weighted_structure_order(
                        set.structures,
                        generator.random_config.seed,
                        chunk_x,
                        chunk_z,
                    )
                };
                for entry in entries {
                    let key = entry.structure;
                    let start =
                        global_cache.get_or_compute_structure_start(key, chunk_x, chunk_z, || {
                            let settings = generator.settings;
                            let mut height_sampler = NoiseHeightSampler::new(generator);
                            let mut biome_sampler =
                                MultiNoiseSampler::generate(&generator.base_router.multi_noise);
                            let biome_supplier: &dyn BiomeSupplier = &generator.biome_supplier;
                            let context = StructureGeneratorContext {
                                seed,
                                chunk_x,
                                chunk_z,
                                random: create_chunk_random(seed, chunk_x, chunk_z),
                                sea_level: settings.sea_level,
                                min_y: (settings.shape.min_y as i32).max(generator.dimension.min_y),
                                height: settings
                                    .shape
                                    .height
                                    .min(generator.dimension.height as u16),
                                height_sampler: Some(&mut height_sampler),
                                structure_key: Some(key),
                            };
                            lazily_generate_structure(
                                &key,
                                Structure::get(&key),
                                context,
                                biome_supplier,
                                &mut biome_sampler,
                            )
                        });
                    let Some(start) = start else {
                        continue;
                    };
                    let structure = Structure::get(&key);
                    if let Some(spawn_override) = structure
                        .spawn_overrides
                        .iter()
                        .find(|spawn_override| spawn_override.category == category)
                    {
                        let inside = match spawn_override.bounding_box {
                            BoundingBoxType::Full => start.get_bounding_box().contains_pos(&pos.0),
                            BoundingBoxType::Piece => start
                                .collector
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .pieces
                                .iter()
                                .any(|piece| {
                                    piece
                                        .get_structure_piece()
                                        .bounding_box
                                        .contains_pos(&pos.0)
                                }),
                        };
                        if inside {
                            return Some(spawn_override.spawns);
                        }
                    }
                    break;
                }
            }
        }
    }
    None
}
