use std::sync::Arc;

use crate::block::{
    BlockBehaviour, CanPlaceAtArgs, GetStateForNeighborUpdateArgs, OnLandedUponArgs, OnPlaceArgs,
    OnScheduledTickArgs, PathComputationType, RandomTickArgs,
};
use crate::world::World;
use pumpkin_data::block_properties::FarmlandLikeProperties;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::tag;
use pumpkin_data::tag::Taggable;
use pumpkin_data::{Block, BlockDirection, BlockState, BlockStateId};
use pumpkin_macros::pumpkin_block;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::tick::TickPriority;
use pumpkin_world::world::BlockAccessor;
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

type FarmlandProperties = FarmlandLikeProperties;

#[pumpkin_block("minecraft:farmland")]
pub struct FarmlandBlock;

impl BlockBehaviour for FarmlandBlock {
    fn on_scheduled_tick(&self, args: OnScheduledTickArgs<'_>) {
        if !can_place_at(args.world.as_ref(), args.position) {
            turn_to_dirt(args.world, args.position);
        }
    }

    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        if !can_place_at(args.world, args.position) {
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

    fn on_landed_upon(&self, args: OnLandedUponArgs<'_>) {
        if let Some(living) = args.entity.get_living_entity() {
            let entity = args.entity.get_entity();
            let pos = entity.get_supporting_block_pos().unwrap_or_else(|| {
                let entity_pos = entity.pos.load();
                BlockPos(Vector3::new(
                    entity_pos.x.floor() as i32,
                    (entity_pos.y - 0.2).floor() as i32,
                    entity_pos.z.floor() as i32,
                ))
            });
            if args.world.get_block(&pos) == &Block::FARMLAND
                && rand::rng().random::<f32>() < args.fall_distance - 0.5
            {
                let can_trample = args.entity.get_player().map_or_else(
                    || args.world.level_info.load().game_rules.mob_griefing,
                    |player| !args.world.is_in_spawn_protection(player, &pos),
                );
                let dim = entity.entity_dimension.load();
                if can_trample && dim.width * dim.width * dim.height > 0.512 {
                    turn_to_dirt(args.world, &pos);
                }
            }
            living.handle_fall_damage(args.entity, args.fall_distance, 1.0);
        }
    }

    fn random_tick(&self, args: RandomTickArgs<'_>) {
        let state_id = args.world.get_block_state_id(args.position);
        let mut props = FarmlandProperties::from_state_id(state_id);
        if args.world.is_raining_at(&args.position.up())
            || is_water_nearby(args.world, args.position)
        {
            if props.moisture < 7 {
                let mut new_moisture = 7;
                if let Some(server) = args.world.server.upgrade() {
                    let mut event =
                        crate::plugin::api::events::block::moisture_change::MoistureChangeEvent::new(
                            *args.position,
                            args.world.clone(),
                            new_moisture,
                        );
                    server.plugin_manager.fire_blocking(&server, &mut event);
                    if event.cancelled {
                        return;
                    }
                    new_moisture = event.new_moisture;
                }
                props.moisture = new_moisture.clamp(0, 7) as u8;
                args.world.set_block_state(
                    args.position,
                    props.to_state_id(args.block),
                    BlockFlags::NOTIFY_LISTENERS,
                );
            }
        } else if props.moisture == 0 {
            if !args
                .world
                .get_block(&args.position.up())
                .has_tag(&tag::Block::MINECRAFT_MAINTAINS_FARMLAND)
            {
                turn_to_dirt(args.world, args.position);
            }
        } else {
            let mut new_moisture = (props.moisture as i32 - 1).clamp(0, 7);
            if let Some(server) = args.world.server.upgrade() {
                let mut event =
                    crate::plugin::api::events::block::moisture_change::MoistureChangeEvent::new(
                        *args.position,
                        args.world.clone(),
                        new_moisture,
                    );
                server.plugin_manager.fire_blocking(&server, &mut event);
                if event.cancelled {
                    return;
                }
                new_moisture = event.new_moisture;
            }
            props.moisture = new_moisture.clamp(0, 7) as u8;
            args.world.set_block_state(
                args.position,
                props.to_state_id(args.block),
                BlockFlags::NOTIFY_LISTENERS,
            );
        }
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
            .has_tag(&tag::Block::MINECRAFT_MAINTAINS_FARMLAND)
}

fn is_water_nearby(world: &Arc<World>, block_pos: &BlockPos) -> bool {
    for dx in -4..=4 {
        for dy in 0..=1 {
            for dz in -4..=4 {
                let check_pos = block_pos.offset(Vector3 {
                    x: dx,
                    y: dy,
                    z: dz,
                });
                if world
                    .get_fluid(&check_pos)
                    .has_tag(&tag::Fluid::MINECRAFT_WATER)
                {
                    return true;
                }
            }
        }
    }
    false
}

// farmland: push entities up and emit block change event (FarmlandBlock.turnToBaseBlock)
fn turn_to_dirt(world: &Arc<World>, pos: &BlockPos) {
    push_entities_up(world, pos);
    world.set_block_state(pos, Block::DIRT.default_state.id, BlockFlags::NOTIFY_ALL);
    world.emit_game_event(GameEvent::BlockChange.name(), pos.to_centered_f64());
}

fn push_entities_up(world: &World, block_pos: &BlockPos) {
    let check_bb = BoundingBox::from_block(block_pos);
    for entity in world.entities.load().iter() {
        let entity_base = entity.get_entity();
        let entity_bb = entity_base.bounding_box.load();
        if check_bb.intersects(&entity_bb) {
            let current_pos = entity_base.pos.load();
            let new_pos = Vector3::new(current_pos.x, current_pos.y + 1.0 / 16.0, current_pos.z);
            entity_base.set_pos(new_pos);
        }
    }
}
