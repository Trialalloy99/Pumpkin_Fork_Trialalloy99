use std::sync::Arc;

use crate::block::entities::beehive::{BeeReleaseStatus, BeehiveBlockEntity};
use crate::block::registry::BlockActionResult;
use crate::block::{
    BlockBehaviour, BlockMetadata, ExplodeArgs, GetComparatorOutputArgs, OnPlaceArgs,
    UseWithItemArgs,
};
use crate::entity::item::ItemEntity;
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::world::World;
use pumpkin_data::Block;
use pumpkin_data::block_properties::BeeNestLikeProperties;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::{BlockDirection, BlockId, BlockStateId};
use pumpkin_inventory::screen_handler::InventoryPlayer;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::{boundingbox::BoundingBox, vector3::Vector3};
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

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

// beehive: BeehiveBlock.angerNearbyBees, bees within 8x6x8 pick a random nearby player
pub fn anger_nearby_bees(world: &Arc<World>, pos: &BlockPos) {
    let center = pos.to_centered_f64();
    let area = BoundingBox::new(
        Vector3::new(center.x - 8.5, center.y - 6.5, center.z - 8.5),
        Vector3::new(center.x + 8.5, center.y + 6.5, center.z + 8.5),
    );
    let entities = world.get_all_at_box(&area);
    let bees: Vec<_> = entities
        .iter()
        .filter(|entity| entity.get_entity().entity_type == &EntityType::BEE)
        .collect();
    if bees.is_empty() {
        return;
    }
    let players: Vec<_> = entities
        .iter()
        .filter(|entity| entity.get_entity().entity_type == &EntityType::PLAYER)
        .collect();
    if players.is_empty() {
        return;
    }
    for bee in bees {
        let Some(mob) = bee.get_mob() else { continue };
        if mob.get_mob_entity().get_target().is_none() {
            let target = players[rand::rng().random_range(0..players.len())];
            mob.get_mob_entity().set_target(Some(target.clone()));
        }
    }
}

// beehive: BeehiveBlock.releaseBeesAndResetHoneyLevel
pub fn release_bees_and_reset_honey_level(
    world: &Arc<World>,
    pos: &BlockPos,
    block: &Block,
    player: Option<&Arc<Player>>,
    status: BeeReleaseStatus,
) {
    let state = world.get_block_state(pos);
    reset_honey_level(world, pos, block);
    if let Some(block_entity) = world.get_block_entity(pos)
        && let Some(hive) = block_entity.as_any().downcast_ref::<BeehiveBlockEntity>()
    {
        hive.empty_all_living_from_hive(world, player, state, status);
    }
}

// beehive: BeehiveBlock.resetHoneyLevel
pub fn reset_honey_level(world: &Arc<World>, pos: &BlockPos, block: &Block) {
    let state_id = world.get_block_state_id(pos);
    let mut props = BeeNestLikeProperties::from_state_id(state_id);
    props.honey_level = 0;
    world.set_block_state(pos, props.to_state_id(block), BlockFlags::NOTIFY_ALL);
}

// beehive: emptyAllLivingFromHive(player, EMERGENCY) then angerNearbyBees (BeehiveBlock.playerDestroy)
pub fn release_bees_for_player_break(world: &Arc<World>, pos: &BlockPos, player: &Arc<Player>) {
    if let Some(block_entity) = world.get_block_entity(pos)
        && let Some(hive) = block_entity.as_any().downcast_ref::<BeehiveBlockEntity>()
    {
        hive.empty_all_living_from_hive(
            world,
            Some(player),
            world.get_block_state(pos),
            BeeReleaseStatus::Emergency,
        );
        anger_nearby_bees(world, pos);
    }
}

fn hive_contains_bees(world: &World, pos: &BlockPos) -> bool {
    world.get_block_entity(pos).is_some_and(|block_entity| {
        block_entity
            .as_any()
            .downcast_ref::<BeehiveBlockEntity>()
            .is_some_and(|hive| !hive.is_empty())
    })
}

impl BeehiveBlock {
    // beehive: smoke keeps the bees calm, otherwise they come out angry (BeehiveBlock.useItemOn)
    fn after_harvest(args: &UseWithItemArgs<'_>) {
        if is_smokey_pos(args.world, args.position) {
            reset_honey_level(args.world, args.position, args.block);
            return;
        }
        if hive_contains_bees(args.world, args.position) {
            anger_nearby_bees(args.world, args.position);
        }
        release_bees_and_reset_honey_level(
            args.world,
            args.position,
            args.block,
            Some(args.player),
            BeeReleaseStatus::Emergency,
        );
    }
}

impl BlockBehaviour for BeehiveBlock {
    // beehive: BeehiveBlock.onExplosionHit and the TNT/creeper getDrops release
    fn explode(&self, args: ExplodeArgs<'_>) {
        if let Some(block_entity) = args.world.get_block_entity(args.position)
            && let Some(hive) = block_entity.as_any().downcast_ref::<BeehiveBlockEntity>()
        {
            hive.empty_all_living_from_hive(
                args.world,
                None,
                args.world.get_block_state(args.position),
                BeeReleaseStatus::Emergency,
            );
        }
        anger_nearby_bees(args.world, args.position);
    }

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
        let props = BeeNestLikeProperties::from_state_id(state_id);

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

                args.world.emit_game_event(GameEvent::Shear.name(), center);
                Self::after_harvest(&args);

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

                args.world
                    .emit_game_event(GameEvent::FluidPickup.name(), center);
                Self::after_harvest(&args);

                return BlockActionResult::SuccessServer;
            }
        }

        BlockActionResult::PassToDefaultBlockAction
    }
}
