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
