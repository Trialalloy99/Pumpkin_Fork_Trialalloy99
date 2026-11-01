use crate::block::{
    BlockBehaviour, CanPlaceAtArgs, GetStateForNeighborUpdateArgs, OnPlaceArgs,
    OnScheduledTickArgs, PathComputationType,
};
use crate::world::World;
use pumpkin_data::{
    Block, BlockDirection, BlockState, BlockStateId,
    game_event::GameEvent,
    tag::{self, Taggable},
};
use pumpkin_macros::pumpkin_block;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::tick::TickPriority;
use pumpkin_world::world::BlockAccessor;
use pumpkin_world::world::BlockFlags;

#[pumpkin_block("minecraft:dirt_path")]
pub struct DirtPathBlock;

impl BlockBehaviour for DirtPathBlock {
    fn on_scheduled_tick(&self, args: OnScheduledTickArgs<'_>) {
        push_entities_up(args.world, args.position);
        args.world.set_block_state(
            args.position,
            Block::DIRT.default_state.id,
            BlockFlags::NOTIFY_ALL,
        );
        args.world.emit_game_event(
            GameEvent::BlockChange.name(),
            args.position.to_centered_f64(),
        );
    }

    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        if !can_place_at(args.world, args.position) {
            push_entities_up(args.world, args.position);
            return Block::DIRT.default_state.id;
        }

        args.block.default_state.id
    }

    fn get_state_for_neighbor_update(
        &self,
        args: GetStateForNeighborUpdateArgs<'_>,
    ) -> BlockStateId {
        if args.direction == BlockDirection::Up && !can_place_at(args.world, args.position) {
            args.world
                .schedule_block_tick(args.block, *args.position, 1, TickPriority::Normal);
        }
        args.state_id
    }

    fn can_place_at(&self, args: CanPlaceAtArgs<'_>) -> bool {
        can_place_at(args.block_accessor, args.position)
    }

    fn is_pathfindable(&self, _state: &BlockState, _computation_type: PathComputationType) -> bool {
        false
    }
}

fn can_place_at(world: &dyn BlockAccessor, block_pos: &BlockPos) -> bool {
    let above_pos = block_pos.up();
    let state = world.get_block_state(&above_pos);
    !state.is_solid()
        || world
            .get_block(&above_pos)
            .has_tag(&tag::Block::MINECRAFT_FENCE_GATES)
}

// dirt path: push up entities when turning into dirt (Block.pushEntitiesUp)
fn push_entities_up(world: &World, block_pos: &BlockPos) {
    let check_bb = BoundingBox::new(
        Vector3::new(
            f64::from(block_pos.0.x),
            f64::from(block_pos.0.y) + 15.0 / 16.0,
            f64::from(block_pos.0.z),
        ),
        Vector3::new(
            f64::from(block_pos.0.x + 1),
            f64::from(block_pos.0.y + 1),
            f64::from(block_pos.0.z + 1),
        ),
    );
    for entity_base in world.entities.load().iter() {
        let entity = entity_base.get_entity();
        if check_bb.intersects(&entity.bounding_box.load()) {
            let mut pos = entity.pos.load();
            pos.y = f64::from(block_pos.0.y + 1);
            entity.teleport(pos, None, None, world);
        }
    }
}
