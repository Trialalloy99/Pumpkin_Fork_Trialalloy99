use crate::generation::{
    generator::{WorldGenerator, structure_finder::find_nearest_structure_start},
    get_world_gen,
};
use pumpkin_data::{
    dimension::Dimension,
    structures::{StructureKeys, StructureSet},
};
use pumpkin_util::{math::position::BlockPos, world_seed::Seed};

// locate: cached village start must equal the generation start (vanilla bb min_y 50 at chunk -9,-52)
#[test]
fn locate_caches_generation_height_village_start() {
    let world_gen = get_world_gen(
        Seed(12345),
        Dimension::OVERWORLD,
        false,
        Vec::new(),
        String::new(),
    );
    let (pos, key) = find_nearest_structure_start(
        BlockPos::new(-144, 64, -832),
        &StructureSet::VILLAGES,
        &[StructureKeys::VillagePlains],
        8,
        &world_gen,
    )
    .expect("village near (-9,-52)");
    assert_eq!((pos.0.x >> 4, pos.0.z >> 4), (-9, -52));

    let WorldGenerator::Noise(generator) = &*world_gen else {
        unreachable!()
    };
    let start = generator
        .global_structure_cache
        .get_or_compute_structure_start(key, -9, -52, || panic!("start was not cached"))
        .expect("cached start");
    let bb = start.get_adjusted_bounding_box(&key);
    assert_eq!(
        [bb.min.x, bb.min.y, bb.min.z, bb.max.x, bb.max.y, bb.max.z],
        [-203, 50, -899, -126, 100, -776]
    );
}

// spawn: structure spawn override applies inside its bounding box only (ChunkGenerator.getMobsAt)
#[test]
fn structure_spawn_override_applies_inside_swamp_hut_pieces() {
    use crate::generation::generator::structure_finder::structure_spawns_at;

    let world_gen = get_world_gen(
        Seed(12345),
        Dimension::OVERWORLD,
        false,
        Vec::new(),
        String::new(),
    );
    let (pos, key) = find_nearest_structure_start(
        BlockPos::new(0, 64, 0),
        &StructureSet::SWAMP_HUTS,
        &[StructureKeys::SwampHut],
        60,
        &world_gen,
    )
    .expect("swamp hut within 60 regions");
    let WorldGenerator::Noise(generator) = &*world_gen else {
        unreachable!()
    };
    let (cx, cz) = (pos.0.x >> 4, pos.0.z >> 4);
    let start = generator
        .global_structure_cache
        .get_or_compute_structure_start(key, cx, cz, || panic!("start was not cached"))
        .expect("cached start");
    let bb = start.get_bounding_box();
    let inside = BlockPos::new(
        i32::midpoint(bb.min.x, bb.max.x),
        i32::midpoint(bb.min.y, bb.max.y),
        i32::midpoint(bb.min.z, bb.max.z),
    );
    let monsters = structure_spawns_at(&world_gen, &inside, "monster").expect("witch list");
    assert_eq!(monsters.len(), 1);
    assert_eq!(monsters[0].entity_type, "minecraft:witch");
    let creatures = structure_spawns_at(&world_gen, &inside, "creature").expect("cat list");
    assert_eq!(creatures[0].entity_type, "minecraft:cat");
    let outside = BlockPos::new(bb.min.x - 40, inside.0.y, bb.min.z - 40);
    assert!(structure_spawns_at(&world_gen, &outside, "monster").is_none());
}

// locate: structures are validated with the dimension's own biome source (end cities only in end highlands/midlands)
#[test]
fn locate_finds_end_city_in_the_end() {
    let world_gen = get_world_gen(
        Seed(12345),
        Dimension::THE_END,
        false,
        Vec::new(),
        String::new(),
    );
    let found = find_nearest_structure_start(
        BlockPos::new(0, 64, 0),
        &StructureSet::END_CITIES,
        &[StructureKeys::EndCity],
        12,
        &world_gen,
    );
    assert!(found.is_some());
}
