use std::sync::Arc;

use crate::block::registry::BlockActionResult;
use crate::block::{
    BlockBehaviour, BlockMetadata, GetComparatorOutputArgs, OnPlaceArgs, UseWithItemArgs,
};
use crate::entity::item::ItemEntity;
use crate::entity::{Entity, EntityBase};
use crate::world::World;
use pumpkin_data::block_properties::BeeNestLikeProperties;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::{BlockDirection, BlockId, BlockStateId};
use pumpkin_inventory::screen_handler::InventoryPlayer;
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

pub struct BeehiveBlock;

impl BlockMetadata for BeehiveBlock {
    fn ids() -> Box<[BlockId]> {
        [BlockId::BEEHIVE, BlockId::BEE_NEST].into()
    }
}

// beehive: check campfire smoke within 5 blocks below (CampfireBlock.isSmokeyPos)
pub fn is_smokey_pos(world: &World, pos: &BlockPos) -> bool {
    for i in 1..=5 {
        let check_pos = pos.offset(BlockDirection::Down.to_offset() * i);
        let state_id = world.get_block_state_id(&check_pos);
        let block_id = state_id.to_block_id();
        if block_id == BlockId::CAMPFIRE || block_id == BlockId::SOUL_CAMPFIRE {
            let lit =
                pumpkin_data::block_properties::CampfireLikeProperties::from_state_id(state_id).lit;
            if lit {
                return true;
            }
        }
        if world.get_block_state(&check_pos).is_solid() {
            return false;
        }
    }
    false
}

impl BlockBehaviour for BeehiveBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        let mut props = BeeNestLikeProperties::default(args.block);
        props.facing = args.player.get_entity().get_horizontal_facing().opposite();
        props.to_state_id(args.block)
    }

    fn get_comparator_output(&self, args: GetComparatorOutputArgs<'_>) -> Option<u8> {
        let state_id = args.world.get_block_state_id(args.position);
        let props = BeeNestLikeProperties::from_state_id(state_id);
        Some(props.honey_level)
    }

    fn use_with_item(&self, args: UseWithItemArgs<'_>) -> BlockActionResult {
        let state_id = args.world.get_block_state_id(args.position);
        let mut props = BeeNestLikeProperties::from_state_id(state_id);

        if props.honey_level >= 5 {
            let item_id = args.item_stack.item.id;
            let center = args.position.to_centered_f64();

            if item_id == Item::SHEARS.id {
                // beehive: shear 3 honeycombs (BeehiveBlock.useItemOn)
                let item_entity = ItemEntity::new(
                    Entity::new(args.world.clone(), center, &EntityType::ITEM),
                    ItemStack::new(3, &Item::HONEYCOMB),
                );
                args.world.spawn_entity(Arc::new(item_entity));

                args.world.play_sound_fine(
                    Sound::BlockBeehiveShear,
                    SoundCategory::Blocks,
                    &center,
                    1.0,
                    1.0,
                );

                if !args.player.has_infinite_materials() {
                    let damage = args.item_stack.get_damage() + 1;
                    if damage >= args.item_stack.get_max_damage().unwrap_or(0) {
                        *args.item_stack = ItemStack::new(0, &Item::AIR);
                    } else {
                        args.item_stack.set_damage(damage);
                    }
                }

                // beehive: anger nearby bees if not smokey (BeehiveBlock.useItemOn)
                let _ = is_smokey_pos(args.world, args.position);

                props.honey_level = 0;
                args.world.set_block_state(
                    args.position,
                    props.to_state_id(args.block),
                    BlockFlags::NOTIFY_ALL,
                );
                args.world.emit_game_event(GameEvent::Shear.name(), center);

                return BlockActionResult::SuccessServer;
            } else if item_id == Item::GLASS_BOTTLE.id {
                // beehive: fill honey bottle (BeehiveBlock.useItemOn)
                if !args.player.has_infinite_materials() {
                    args.item_stack.decrement(1);
                }

                args.world.play_sound_fine(
                    Sound::ItemBottleFill,
                    SoundCategory::Blocks,
                    &center,
                    1.0,
                    1.0,
                );

                let honey_bottle = ItemStack::new(1, &Item::HONEY_BOTTLE);
                if args.item_stack.is_empty() {
                    *args.item_stack = honey_bottle;
                } else {
                    args.player.drop_item(honey_bottle);
                }

                // beehive: anger nearby bees if not smokey (BeehiveBlock.useItemOn)
                let _ = is_smokey_pos(args.world, args.position);

                props.honey_level = 0;
                args.world.set_block_state(
                    args.position,
                    props.to_state_id(args.block),
                    BlockFlags::NOTIFY_ALL,
                );
                args.world
                    .emit_game_event(GameEvent::FluidPickup.name(), center);

                return BlockActionResult::SuccessServer;
            }
        }

        BlockActionResult::PassToDefaultBlockAction
    }
}
