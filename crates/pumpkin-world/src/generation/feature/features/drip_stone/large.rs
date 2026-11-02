use pumpkin_data::{Block, BlockId, tag};
use pumpkin_util::{
    math::{
        float_provider::FloatProvider, floor_i32, int_provider::IntProvider, position::BlockPos,
        vector3::Vector3,
    },
    random::{RandomGenerator, RandomImpl},
};

use crate::generation::proto_chunk::GenerationCache;

use super::{
    Column, is_base_or_lava, is_circle_mostly_embedded_in_stone, is_empty_or_water_or_lava,
    is_empty_or_water_state, scan_column, speleothem_height, state_at,
};

// dripstone: LargeDripstoneFeature
pub struct LargeDripstoneFeature {
    pub floor_to_ceiling_search_range: i32,
    pub column_radius: IntProvider,
    pub height_scale: FloatProvider,
    pub max_column_radius_to_cave_height_ratio: f32,
    pub stalactite_bluntness: FloatProvider,
    pub stalagmite_bluntness: FloatProvider,
    pub wind_speed: FloatProvider,
    pub min_radius_for_wind: i32,
    pub min_bluntness_for_wind: f32,
}

struct LargeDripstone {
    root: BlockPos,
    pointing_up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

// dripstone: LargeDripstoneFeature.WindOffsetter
struct WindOffsetter {
    origin_y: i32,
    wind: Option<(f64, f64)>,
    max_offset: i32,
}

impl WindOffsetter {
    fn new(
        origin_y: i32,
        random: &mut RandomGenerator,
        wind_speed: &FloatProvider,
        max_offset: i32,
    ) -> Self {
        let speed = wind_speed.get(random);
        let direction = random.next_f32() * std::f32::consts::PI;
        Self {
            origin_y,
            wind: Some((
                f64::from(pumpkin_util::math::cos(direction) * speed),
                f64::from(pumpkin_util::math::sin(direction) * speed),
            )),
            max_offset,
        }
    }

    const fn none() -> Self {
        Self {
            origin_y: 0,
            wind: None,
            max_offset: 0,
        }
    }

    fn offset(&self, pos: BlockPos) -> BlockPos {
        let Some((wx, wz)) = self.wind else {
            return pos;
        };
        let dy = f64::from(self.origin_y - pos.0.y);
        let dx = floor_i32(wx * dy).clamp(-self.max_offset, self.max_offset);
        let dz = floor_i32(wz * dy).clamp(-self.max_offset, self.max_offset);
        pos.offset(Vector3::new(dx, 0, dz))
    }
}

impl LargeDripstone {
    fn height_at_radius(&self, check_radius: f32) -> i32 {
        speleothem_height(
            f64::from(check_radius),
            f64::from(self.radius),
            self.scale,
            self.bluntness,
        ) as i32
    }

    fn move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary<T: GenerationCache>(
        &mut self,
        chunk: &T,
        wind: &WindOffsetter,
    ) -> bool {
        while self.radius > 1 {
            let mut new_root = self.root;
            let max_tries = 10.min(self.height_at_radius(0.0));
            for _ in 0..max_tries {
                if state_at(chunk, new_root).to_block_id() == BlockId::LAVA {
                    return false;
                }
                if is_circle_mostly_embedded_in_stone(chunk, wind.offset(new_root), self.radius) {
                    self.root = new_root;
                    return true;
                }
                new_root = if self.pointing_up {
                    new_root.down()
                } else {
                    new_root.up()
                };
            }
            self.radius /= 2;
        }
        false
    }

    fn place_blocks<T: GenerationCache>(
        &self,
        chunk: &mut T,
        random: &mut RandomGenerator,
        wind: &WindOffsetter,
    ) {
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let current_radius = ((dx * dx + dz * dz) as f32).sqrt();
                if current_radius > self.radius as f32 {
                    continue;
                }
                let mut height = self.height_at_radius(current_radius);
                if height <= 0 {
                    continue;
                }
                if f64::from(random.next_f32()) < 0.2 {
                    let factor = random.next_f32() * (1.0 - 0.8) + 0.8;
                    height = (height as f32 * factor) as i32;
                }
                let mut pos = self.root.offset(Vector3::new(dx, 0, dz));
                let mut has_been_out_of_stone = false;
                let max_y = if self.pointing_up {
                    chunk.top_block_wg_height_exclusive(pos.0.x, pos.0.z)
                } else {
                    i32::MAX
                };
                let mut i = 0;
                while i < height && pos.0.y < max_y {
                    let adjusted = wind.offset(pos);
                    let id = state_at(chunk, adjusted);
                    if is_empty_or_water_or_lava(id) {
                        has_been_out_of_stone = true;
                        chunk.set_block_state(&adjusted.0, Block::DRIPSTONE_BLOCK.default_state);
                    } else if has_been_out_of_stone
                        && id
                            .to_block_id()
                            .has_tag(tag::Block::MINECRAFT_BASE_STONE_OVERWORLD)
                    {
                        break;
                    }
                    pos = if self.pointing_up {
                        pos.up()
                    } else {
                        pos.down()
                    };
                    i += 1;
                }
            }
        }
    }

    fn is_suitable_for_wind(&self, min_radius: i32, min_bluntness: f32) -> bool {
        self.radius >= min_radius && self.bluntness >= f64::from(min_bluntness)
    }
}

impl LargeDripstoneFeature {
    pub fn generate<T: GenerationCache>(
        &self,
        chunk: &mut T,
        random: &mut RandomGenerator,
        origin: BlockPos,
    ) -> bool {
        if !is_empty_or_water_state(state_at(chunk, origin)) {
            return false;
        }
        let Some(column) = scan_column(
            chunk,
            origin,
            self.floor_to_ceiling_search_range,
            is_empty_or_water_state,
            is_base_or_lava,
        ) else {
            return false;
        };
        let Column {
            floor: Some(floor),
            ceiling: Some(ceiling),
        } = column
        else {
            return false;
        };
        let column_height = ceiling - floor - 1;
        if column_height < 4 {
            return false;
        }
        let max_radius_by_height =
            (column_height as f32 * self.max_column_radius_to_cave_height_ratio) as i32;
        let min_radius = self.column_radius.get_min();
        let max_radius = max_radius_by_height.clamp(min_radius, self.column_radius.get_max());
        let radius = random.next_inbetween_i32(min_radius, max_radius);
        let mut stalactite = self.make_dripstone(
            BlockPos::new(origin.0.x, ceiling - 1, origin.0.z),
            false,
            random,
            radius,
            &self.stalactite_bluntness,
        );
        let mut stalagmite = self.make_dripstone(
            BlockPos::new(origin.0.x, floor + 1, origin.0.z),
            true,
            random,
            radius,
            &self.stalagmite_bluntness,
        );
        let wind = if stalactite
            .is_suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
            && stalagmite
                .is_suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
        {
            WindOffsetter::new(origin.0.y, random, &self.wind_speed, 16 - radius)
        } else {
            WindOffsetter::none()
        };
        let stalactite_embedded = stalactite
            .move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary(chunk, &wind);
        let stalagmite_embedded = stalagmite
            .move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary(chunk, &wind);
        if stalactite_embedded {
            stalactite.place_blocks(chunk, random, &wind);
        }
        if stalagmite_embedded {
            stalagmite.place_blocks(chunk, random, &wind);
        }
        true
    }

    fn make_dripstone(
        &self,
        root: BlockPos,
        pointing_up: bool,
        random: &mut RandomGenerator,
        radius: i32,
        bluntness: &FloatProvider,
    ) -> LargeDripstone {
        let bluntness = f64::from(bluntness.get(random));
        let scale = f64::from(self.height_scale.get(random));
        LargeDripstone {
            root,
            pointing_up,
            radius,
            bluntness,
            scale,
        }
    }
}
