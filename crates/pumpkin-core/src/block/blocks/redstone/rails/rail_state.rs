use std::sync::Arc;

use pumpkin_data::block_properties::RailShape;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::{Block, BlockStateId};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

use crate::world::World;

use super::RailProperties;

// rail: exact port of vanilla RailState (connection and shape placement)
pub(super) struct RailState<'a> {
    world: &'a Arc<World>,
    pos: BlockPos,
    block: &'static Block,
    props: RailProperties,
    is_straight: bool,
    connections: Vec<BlockPos>,
}

fn is_rail(world: &World, pos: &BlockPos) -> bool {
    world.get_block(pos).has_tag(&tag::Block::MINECRAFT_RAILS)
}

impl<'a> RailState<'a> {
    pub(super) fn new(
        world: &'a Arc<World>,
        pos: BlockPos,
        block: &'static Block,
        state_id: BlockStateId,
    ) -> Self {
        let props = RailProperties::new(state_id, block);
        let mut state = Self {
            world,
            pos,
            block,
            props,
            is_straight: block != &Block::RAIL,
            connections: Vec::with_capacity(2),
        };
        state.update_connections(state.props.shape());
        state
    }

    fn update_connections(&mut self, shape: RailShape) {
        let p = self.pos;
        self.connections.clear();
        let (a, b) = match shape {
            RailShape::NorthSouth => (p.north(), p.south()),
            RailShape::EastWest => (p.west(), p.east()),
            RailShape::AscendingEast => (p.west(), p.east().up()),
            RailShape::AscendingWest => (p.west().up(), p.east()),
            RailShape::AscendingNorth => (p.north().up(), p.south()),
            RailShape::AscendingSouth => (p.north(), p.south().up()),
            RailShape::SouthEast => (p.east(), p.south()),
            RailShape::SouthWest => (p.west(), p.south()),
            RailShape::NorthWest => (p.west(), p.north()),
            RailShape::NorthEast => (p.east(), p.north()),
        };
        self.connections.push(a);
        self.connections.push(b);
    }

    fn remove_soft_connections(&mut self) {
        let mut i = 0;
        while i < self.connections.len() {
            let target = self.connections[i];
            match self.get_rail(&target) {
                Some(rail) if rail.connects_to(self) => {
                    self.connections[i] = rail.pos;
                    i += 1;
                }
                _ => {
                    self.connections.remove(i);
                }
            }
        }
    }

    fn get_rail(&self, pos: &BlockPos) -> Option<Self> {
        for test in [*pos, pos.up(), pos.down()] {
            let (block, state_id) = self.world.get_block_and_state_id(&test);
            if block.has_tag(&tag::Block::MINECRAFT_RAILS) {
                return Some(RailState::new(self.world, test, block, state_id));
            }
        }
        None
    }

    fn connects_to(&self, rail: &RailState<'_>) -> bool {
        self.has_connection(&rail.pos)
    }

    fn has_connection(&self, rail_pos: &BlockPos) -> bool {
        self.connections
            .iter()
            .any(|pos| pos.0.x == rail_pos.0.x && pos.0.z == rail_pos.0.z)
    }

    fn can_connect_to(&self, rail: &RailState<'_>) -> bool {
        self.connects_to(rail) || self.connections.len() != 2
    }

    fn apply_ascending(&self, mut shape: Option<RailShape>) -> Option<RailShape> {
        let (north, south, west, east) = (
            self.pos.north(),
            self.pos.south(),
            self.pos.west(),
            self.pos.east(),
        );
        if shape == Some(RailShape::NorthSouth) {
            if is_rail(self.world, &north.up()) {
                shape = Some(RailShape::AscendingNorth);
            }
            if is_rail(self.world, &south.up()) {
                shape = Some(RailShape::AscendingSouth);
            }
        }
        if shape == Some(RailShape::EastWest) {
            if is_rail(self.world, &east.up()) {
                shape = Some(RailShape::AscendingEast);
            }
            if is_rail(self.world, &west.up()) {
                shape = Some(RailShape::AscendingWest);
            }
        }
        shape
    }

    fn connect_to(&mut self, rail: &RailState<'_>) {
        self.connections.push(rail.pos);
        let n = self.has_connection(&self.pos.north());
        let s = self.has_connection(&self.pos.south());
        let w = self.has_connection(&self.pos.west());
        let e = self.has_connection(&self.pos.east());
        let mut shape = if w || e {
            Some(RailShape::EastWest)
        } else {
            (n || s).then_some(RailShape::NorthSouth)
        };
        if !self.is_straight {
            if s && e && !n && !w {
                shape = Some(RailShape::SouthEast);
            }
            if s && w && !n && !e {
                shape = Some(RailShape::SouthWest);
            }
            if n && w && !s && !e {
                shape = Some(RailShape::NorthWest);
            }
            if n && e && !s && !w {
                shape = Some(RailShape::NorthEast);
            }
        }
        let shape = self.apply_ascending(shape).unwrap_or(RailShape::NorthSouth);
        self.props.set_shape(shape);
        self.world.set_block_state(
            &self.pos,
            self.props.to_state_id(self.block),
            BlockFlags::NOTIFY_ALL,
        );
    }

    fn has_neighbor_rail(&self, rail_pos: &BlockPos) -> bool {
        let Some(mut neighbor) = self.get_rail(rail_pos) else {
            return false;
        };
        neighbor.remove_soft_connections();
        neighbor.can_connect_to(self)
    }

    // RailState.place
    pub(super) fn place(&mut self, has_signal: bool, first: bool, default_shape: RailShape) {
        let n = self.has_neighbor_rail(&self.pos.north());
        let s = self.has_neighbor_rail(&self.pos.south());
        let w = self.has_neighbor_rail(&self.pos.west());
        let e = self.has_neighbor_rail(&self.pos.east());
        let mut shape = None;
        let north_or_south = n || s;
        let west_or_east = w || e;
        if north_or_south && !west_or_east {
            shape = Some(RailShape::NorthSouth);
        }
        if west_or_east && !north_or_south {
            shape = Some(RailShape::EastWest);
        }
        let (south_east, south_west, north_east, north_west) = (s && e, s && w, n && e, n && w);
        if !self.is_straight {
            if south_east && !n && !w {
                shape = Some(RailShape::SouthEast);
            }
            if south_west && !n && !e {
                shape = Some(RailShape::SouthWest);
            }
            if north_west && !s && !e {
                shape = Some(RailShape::NorthWest);
            }
            if north_east && !s && !w {
                shape = Some(RailShape::NorthEast);
            }
        }
        if shape.is_none() {
            if north_or_south && west_or_east {
                shape = Some(default_shape);
            } else if north_or_south {
                shape = Some(RailShape::NorthSouth);
            } else if west_or_east {
                shape = Some(RailShape::EastWest);
            }
            if !self.is_straight {
                if has_signal {
                    if south_east {
                        shape = Some(RailShape::SouthEast);
                    }
                    if south_west {
                        shape = Some(RailShape::SouthWest);
                    }
                    if north_east {
                        shape = Some(RailShape::NorthEast);
                    }
                    if north_west {
                        shape = Some(RailShape::NorthWest);
                    }
                } else {
                    if north_west {
                        shape = Some(RailShape::NorthWest);
                    }
                    if north_east {
                        shape = Some(RailShape::NorthEast);
                    }
                    if south_west {
                        shape = Some(RailShape::SouthWest);
                    }
                    if south_east {
                        shape = Some(RailShape::SouthEast);
                    }
                }
            }
        }
        let shape = self.apply_ascending(shape).unwrap_or(default_shape);
        self.update_connections(shape);
        self.props.set_shape(shape);
        let state_id = self.props.to_state_id(self.block);
        if first || self.world.get_block_state_id(&self.pos) != state_id {
            self.world
                .set_block_state(&self.pos, state_id, BlockFlags::NOTIFY_ALL);
            for i in 0..self.connections.len() {
                let target = self.connections[i];
                if let Some(mut neighbor) = self.get_rail(&target) {
                    neighbor.remove_soft_connections();
                    if neighbor.can_connect_to(self) {
                        neighbor.connect_to(self);
                    }
                }
            }
        }
    }
}
