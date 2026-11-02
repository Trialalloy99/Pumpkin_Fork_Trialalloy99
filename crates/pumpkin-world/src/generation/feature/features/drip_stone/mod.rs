use crate::generation::proto_chunk::GenerationCache;
use pumpkin_data::block_properties::{
    PointedDripstoneLikeProperties, SpeleothemThickness, VerticalDirection,
};
use pumpkin_data::tag;
use pumpkin_data::{Block, BlockDirection, BlockId, BlockState, BlockStateId};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

pub mod cluster;
pub mod large;
pub mod small;

pub(super) fn can_replace(id: BlockId) -> bool {
    id == BlockId::DRIPSTONE_BLOCK || id.has_tag(tag::Block::MINECRAFT_DRIPSTONE_REPLACEABLE_BLOCKS)
}

pub(super) fn gen_dripstone<T: GenerationCache>(chunk: &mut T, pos: BlockPos) -> bool {
    let block = GenerationCache::get_block_state(chunk, &pos.0).to_block_id();
    if block.has_tag(tag::Block::MINECRAFT_DRIPSTONE_REPLACEABLE_BLOCKS) {
        chunk.set_block_state(&pos.0, Block::DRIPSTONE_BLOCK.default_state);
        return true;
    }
    false
}

pub(super) fn is_empty_or_water<T: GenerationCache>(chunk: &T, pos: BlockPos) -> bool {
    let block = GenerationCache::get_block_state(chunk, &pos.0).to_block_id();
    block == BlockId::AIR
        || block == BlockId::CAVE_AIR
        || block == BlockId::VOID_AIR
        || block == BlockId::WATER
}

pub(super) fn grow_pointed_dripstone<T: GenerationCache>(
    chunk: &mut T,
    start_pos: BlockPos,
    tip_direction: BlockDirection,
    height: i32,
    merged_tip: bool,
) {
    // speleothem: needs a base behind the start (SpeleothemUtils.growSpeleothem)
    let behind = start_pos.offset(tip_direction.opposite().to_offset());
    if !can_replace(state_at(chunk, behind).to_block_id()) {
        return;
    }
    let mut cur = start_pos;
    let vert_dir = match tip_direction {
        BlockDirection::Down => VerticalDirection::Down,
        _ => VerticalDirection::Up,
    };

    let mut thicknesses = Vec::with_capacity(height as usize);
    if height >= 3 {
        thicknesses.push(SpeleothemThickness::Base);
        for _ in 0..(height - 3) {
            thicknesses.push(SpeleothemThickness::Middle);
        }
    }
    if height >= 2 {
        thicknesses.push(SpeleothemThickness::Frustum);
    }
    if height >= 1 {
        thicknesses.push(if merged_tip {
            SpeleothemThickness::TipMerge
        } else {
            SpeleothemThickness::Tip
        });
    }

    for thickness in thicknesses {
        let is_water =
            GenerationCache::get_block_state(chunk, &cur.0).to_block_id() == BlockId::WATER;
        let mut props = PointedDripstoneLikeProperties::default(&Block::POINTED_DRIPSTONE);
        props.thickness = thickness;
        props.vertical_direction = vert_dir;
        props.waterlogged = is_water;
        let state_id = props.to_state_id(&Block::POINTED_DRIPSTONE);
        chunk.set_block_state(&cur.0, BlockState::from_id(state_id));
        cur = cur.offset(tip_direction.to_offset());
    }
}

// speleothem: out of world reads as air
pub(super) fn state_at<T: GenerationCache>(chunk: &T, pos: BlockPos) -> BlockStateId {
    if chunk.out_of_height(pos.0.y as i16) {
        return Block::AIR.default_state.id;
    }
    GenerationCache::get_block_state(chunk, &pos.0)
}

pub(super) fn is_empty_or_water_state(id: BlockStateId) -> bool {
    id.to_state().is_air() || id.to_block_id() == BlockId::WATER
}

pub(super) fn is_neither_empty_nor_water(id: BlockStateId) -> bool {
    !is_empty_or_water_state(id)
}

pub(super) fn is_empty_or_water_or_lava(id: BlockStateId) -> bool {
    is_empty_or_water_state(id) || id.to_block_id() == BlockId::LAVA
}

pub(super) fn is_base_or_lava(id: BlockStateId) -> bool {
    can_replace(id.to_block_id()) || id.to_block_id() == BlockId::LAVA
}

// Column: floor and ceiling found by Column.scan
#[derive(Clone, Copy)]
pub(super) struct Column {
    pub floor: Option<i32>,
    pub ceiling: Option<i32>,
}

impl Column {
    pub(super) fn height(self) -> Option<i32> {
        Some(self.ceiling? - self.floor? - 1)
    }
}

pub(super) fn scan_column<T: GenerationCache>(
    chunk: &T,
    pos: BlockPos,
    search_range: i32,
    inside: fn(BlockStateId) -> bool,
    valid_edge: fn(BlockStateId) -> bool,
) -> Option<Column> {
    if !inside(state_at(chunk, pos)) {
        return None;
    }
    let scan = |direction: BlockDirection| {
        let mut cur = pos;
        let mut i = 1;
        while i < search_range && inside(state_at(chunk, cur)) {
            cur = cur.offset(direction.to_offset());
            i += 1;
        }
        valid_edge(state_at(chunk, cur)).then_some(cur.0.y)
    };
    Some(Column {
        ceiling: scan(BlockDirection::Up),
        floor: scan(BlockDirection::Down),
    })
}

// speleothem: SpeleothemUtils.getSpeleothemHeight
pub(super) fn speleothem_height(xz_distance: f64, radius: f64, scale: f64, bluntness: f64) -> f64 {
    let xz_distance = xz_distance.max(bluntness);
    let r = xz_distance / radius * 0.384;
    let part1 = 0.75 * r.powf(1.333_333_333_333_333_3);
    let part2 = r.powf(0.666_666_666_666_666_6);
    let part3 = 0.333_333_333_333_333_3 * r.ln();
    let height = (scale * (part1 - part2 - part3)).max(0.0);
    height / 0.384 * radius
}

pub(super) fn is_circle_mostly_embedded_in_stone<T: GenerationCache>(
    chunk: &T,
    center: BlockPos,
    xz_radius: i32,
) -> bool {
    if is_empty_or_water_or_lava(state_at(chunk, center)) {
        return false;
    }
    let increment = 6.0f32 / xz_radius as f32;
    let mut angle = 0.0f32;
    while angle < std::f32::consts::PI * 2.0 {
        let dx = (pumpkin_util::math::cos(angle) * xz_radius as f32) as i32;
        let dz = (pumpkin_util::math::sin(angle) * xz_radius as f32) as i32;
        if is_empty_or_water_or_lava(state_at(chunk, center.offset(Vector3::new(dx, 0, dz)))) {
            return false;
        }
        angle += increment;
    }
    true
}
