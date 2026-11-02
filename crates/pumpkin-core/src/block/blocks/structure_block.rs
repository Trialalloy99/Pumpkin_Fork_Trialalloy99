use std::sync::Arc;

use crate::block::entities::structure_block::StructureBlockBlockEntity;
use crate::block::registry::BlockActionResult;
use crate::block::{
    BlockBehaviour, NormalUseArgs, OnNeighborUpdateArgs, OnPlaceArgs, PlacedArgs, PlayerPlacedArgs,
};

use pumpkin_data::BlockStateId;
use pumpkin_data::block_properties::StructureBlockLikeProperties;
use pumpkin_macros::pumpkin_block;

use super::redstone::block_receives_redstone_power;

#[pumpkin_block("minecraft:structure_block")]
pub struct StructureBlock;

impl BlockBehaviour for StructureBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        let props = StructureBlockLikeProperties::default(args.block);
        props.to_state_id(args.block)
    }

    fn placed(&self, args: PlacedArgs<'_>) {
        if args.world.get_block_entity(args.position).is_none() {
            args.world
                .add_block_entity(Arc::new(StructureBlockBlockEntity::new(*args.position)));
        }
    }

    // structure block: StructureBlock.setPlacedBy sets the author
    fn player_placed(&self, args: PlayerPlacedArgs<'_>) {
        if let Some(block_entity) = args.world.get_block_entity(args.position)
            && let Some(structure) = block_entity
                .as_any()
                .downcast_ref::<StructureBlockBlockEntity>()
        {
            structure
                .author
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone_from(&args.player.gameprofile.name);
        }
    }

    // structure block: StructureBlockEntity.usedBy (game master blocks only)
    fn normal_use(&self, args: NormalUseArgs<'_>) -> BlockActionResult {
        if !args.player.can_use_game_master_blocks() {
            return BlockActionResult::Pass;
        }
        let Some(block_entity) = args.world.get_block_entity(args.position) else {
            return BlockActionResult::Pass;
        };
        args.world.update_block_entity(&block_entity);
        BlockActionResult::Success
    }

    // structure block: StructureBlock.neighborChanged, save and load on a rising edge
    fn on_neighbor_update(&self, args: OnNeighborUpdateArgs<'_>) {
        let Some(block_entity) = args.world.get_block_entity(args.position) else {
            return;
        };
        let Some(structure) = block_entity
            .as_any()
            .downcast_ref::<StructureBlockBlockEntity>()
        else {
            return;
        };
        let should_trigger = block_receives_redstone_power(args.world, args.position);
        let is_powered = *structure
            .powered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if should_trigger && !is_powered {
            *structure
                .powered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            match structure.mode_name().as_str() {
                "SAVE" => {
                    structure.save_structure(args.world, false);
                }
                "LOAD" => structure.place_structure(args.world),
                "CORNER" => structure.unload_structure(),
                _ => {}
            }
        } else if !should_trigger && is_powered {
            *structure
                .powered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        }
    }
}
