use pumpkin_data::{Block, BlockDirection, BlockId, tag};
use pumpkin_util::{
    math::{
        clamped_map,
        float_provider::{ClampedNormalFloatProvider, FloatProvider},
        int_provider::IntProvider,
        position::BlockPos,
        vector3::Vector3,
    },
    random::{RandomGenerator, RandomImpl},
};

use crate::generation::proto_chunk::GenerationCache;

const fn with_y(pos: BlockPos, y: i32) -> BlockPos {
    BlockPos(Vector3::new(pos.0.x, y, pos.0.z))
}

use super::{
    Column, gen_dripstone, grow_pointed_dripstone, is_empty_or_water_state,
    is_neither_empty_nor_water, scan_column, state_at,
};

// dripstone: SpeleothemClusterFeature
pub struct DripstoneClusterFeature {
    pub floor_to_ceiling_search_range: i32,
    pub height: IntProvider,
    pub radius: IntProvider,
    pub max_stalagmite_stalactite_height_diff: i32,
    pub height_deviation: i32,
    pub speleothem_block_layer_thickness: IntProvider,
    pub density: FloatProvider,
    pub wetness: FloatProvider,
    pub chance_of_speleothem_at_max_distance_from_center: f32,
    pub max_distance_from_edge_affecting_chance_of_speleothem: i32,
    pub max_distance_from_center_affecting_height_bias: i32,
}

impl DripstoneClusterFeature {
    pub fn generate<T: GenerationCache>(
        &self,
        chunk: &mut T,
        random: &mut RandomGenerator,
        origin: BlockPos,
    ) -> bool {
        if !is_empty_or_water_state(state_at(chunk, origin)) {
            return false;
        }
        let height = self.height.get(random);
        let wetness = self.wetness.get(random);
        let density = self.density.get(random);
        let x_radius = self.radius.get(random);
        let z_radius = self.radius.get(random);
        for dx in -x_radius..=x_radius {
            for dz in -z_radius..=z_radius {
                let chance = self.chance_of_stalagmite_or_stalactite(x_radius, z_radius, dx, dz);
                let pos = origin.offset(Vector3::new(dx, 0, dz));
                self.place_column(chunk, random, pos, dx, dz, wetness, chance, height, density);
            }
        }
        true
    }

    #[expect(clippy::too_many_arguments, clippy::too_many_lines)]
    fn place_column<T: GenerationCache>(
        &self,
        chunk: &mut T,
        random: &mut RandomGenerator,
        pos: BlockPos,
        dx: i32,
        dz: i32,
        chance_of_water: f32,
        chance_of_speleothem: f64,
        cluster_height: i32,
        density: f32,
    ) {
        let Some(base_column) = scan_column(
            chunk,
            pos,
            self.floor_to_ceiling_search_range,
            is_empty_or_water_state,
            is_neither_empty_nor_water,
        ) else {
            return;
        };
        let (ceiling, base_floor) = (base_column.ceiling, base_column.floor);
        if ceiling.is_none() && base_floor.is_none() {
            return;
        }
        let want_pool = random.next_f32() < chance_of_water;
        let mut column = base_column;
        if want_pool
            && let Some(base_floor_y) = base_floor
            && Self::can_place_pool(chunk, with_y(pos, base_floor_y))
        {
            column = Column {
                floor: Some(base_floor_y - 1),
                ceiling: base_column.ceiling,
            };
            chunk.set_block_state(&with_y(pos, base_floor_y).0, Block::WATER.default_state);
        }
        let floor = column.floor;
        let want_stalactite = random.next_f64() < chance_of_speleothem;
        let stalactite_height = if let Some(ceiling_y) = ceiling
            && want_stalactite
            && !Self::is_lava(chunk, with_y(pos, ceiling_y))
        {
            let thickness = self.speleothem_block_layer_thickness.get(random);
            Self::replace_blocks_with_base_blocks(
                chunk,
                with_y(pos, ceiling_y),
                thickness,
                BlockDirection::Up,
            );
            let max_height = floor.map_or(cluster_height, |floor_y| {
                cluster_height.min(ceiling_y - floor_y)
            });
            self.speleothem_height(random, dx, dz, density, max_height)
        } else {
            0
        };
        let want_stalagmite = random.next_f64() < chance_of_speleothem;
        let stalagmite_height = if let Some(floor_y) = floor
            && want_stalagmite
            && !Self::is_lava(chunk, with_y(pos, floor_y))
        {
            let thickness = self.speleothem_block_layer_thickness.get(random);
            Self::replace_blocks_with_base_blocks(
                chunk,
                with_y(pos, floor_y),
                thickness,
                BlockDirection::Down,
            );
            if ceiling.is_some() {
                let diff = self.max_stalagmite_stalactite_height_diff;
                0.max(stalactite_height + random.next_inbetween_i32(-diff, diff))
            } else {
                self.speleothem_height(random, dx, dz, density, cluster_height)
            }
        } else {
            0
        };
        let (actual_stalactite, actual_stalagmite) = if let (Some(ceiling_y), Some(floor_y)) =
            (ceiling, floor)
            && ceiling_y - stalactite_height <= floor_y + stalagmite_height
        {
            let lowest_bottom = (ceiling_y - stalactite_height).max(floor_y + 1);
            let highest_top = (floor_y + stalagmite_height).min(ceiling_y - 1);
            let bottom = random.next_inbetween_i32(lowest_bottom, highest_top + 1);
            (ceiling_y - bottom, bottom - 1 - floor_y)
        } else {
            (stalactite_height, stalagmite_height)
        };
        let merge_tips = random.next_bool()
            && actual_stalactite > 0
            && actual_stalagmite > 0
            && column
                .height()
                .is_some_and(|height| actual_stalactite + actual_stalagmite == height);
        if let Some(ceiling_y) = ceiling {
            grow_pointed_dripstone(
                chunk,
                with_y(pos, ceiling_y - 1),
                BlockDirection::Down,
                actual_stalactite,
                merge_tips,
            );
        }
        if let Some(floor_y) = floor {
            grow_pointed_dripstone(
                chunk,
                with_y(pos, floor_y + 1),
                BlockDirection::Up,
                actual_stalagmite,
                merge_tips,
            );
        }
    }

    fn is_lava<T: GenerationCache>(chunk: &T, pos: BlockPos) -> bool {
        state_at(chunk, pos).to_block_id() == BlockId::LAVA
    }

    fn speleothem_height(
        &self,
        random: &mut RandomGenerator,
        dx: i32,
        dz: i32,
        density: f32,
        max_height: i32,
    ) -> i32 {
        if random.next_f32() > density {
            return 0;
        }
        let distance = dx.abs() + dz.abs();
        let mean = clamped_map(
            f64::from(distance),
            0.0,
            f64::from(self.max_distance_from_center_affecting_height_bias),
            f64::from(max_height) / 2.0,
            0.0,
        ) as f32;
        ClampedNormalFloatProvider::new(mean, self.height_deviation as f32, 0.0, max_height as f32)
            .get(random) as i32
    }

    fn can_place_pool<T: GenerationCache>(chunk: &T, pos: BlockPos) -> bool {
        let id = state_at(chunk, pos).to_block_id();
        if id == BlockId::WATER
            || id == BlockId::DRIPSTONE_BLOCK
            || id == BlockId::POINTED_DRIPSTONE
        {
            return false;
        }
        if Self::is_water(chunk, pos.up()) {
            return false;
        }
        for direction in BlockDirection::horizontal_worldgen() {
            if !Self::can_be_adjacent_to_water(chunk, pos.offset(direction.to_offset())) {
                return false;
            }
        }
        Self::can_be_adjacent_to_water(chunk, pos.down())
    }

    fn is_water<T: GenerationCache>(chunk: &T, pos: BlockPos) -> bool {
        let id = state_at(chunk, pos);
        id.to_block_id() == BlockId::WATER || Block::from_state_id(id).is_waterlogged(id)
    }

    fn can_be_adjacent_to_water<T: GenerationCache>(chunk: &T, pos: BlockPos) -> bool {
        state_at(chunk, pos)
            .to_block_id()
            .has_tag(tag::Block::MINECRAFT_BASE_STONE_OVERWORLD)
            || Self::is_water(chunk, pos)
    }

    fn replace_blocks_with_base_blocks<T: GenerationCache>(
        chunk: &mut T,
        first: BlockPos,
        max_count: i32,
        direction: BlockDirection,
    ) {
        let mut pos = first;
        for _ in 0..max_count {
            if !gen_dripstone(chunk, pos) {
                return;
            }
            pos = pos.offset(direction.to_offset());
        }
    }

    fn chance_of_stalagmite_or_stalactite(
        &self,
        x_radius: i32,
        z_radius: i32,
        dx: i32,
        dz: i32,
    ) -> f64 {
        let distance_from_edge = (x_radius - dx.abs()).min(z_radius - dz.abs());
        f64::from(clamped_map(
            distance_from_edge as f32,
            0.0,
            self.max_distance_from_edge_affecting_chance_of_speleothem as f32,
            self.chance_of_speleothem_at_max_distance_from_center,
            1.0,
        ))
    }
}
