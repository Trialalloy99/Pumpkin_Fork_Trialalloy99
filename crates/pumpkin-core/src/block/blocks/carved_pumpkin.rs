use std::sync::Arc;

use pumpkin_data::{
    Block, BlockDirection, BlockId, BlockStateId,
    block_properties::{ChestLikeProperties, WallTorchLikeProperties},
    entity::EntityType,
    world::WorldEvent,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

use crate::{
    block::{
        BlockBehaviour, BlockMetadata, OnPlaceArgs, PlacedArgs, PlayerPlacedArgs,
        blocks::weathering_copper::WeatherState,
    },
    entity::{
        Entity, EntityBase,
        passive::{
            copper_golem::{CopperGolemEntity, WeatherState as CopperWeatherState},
            iron_golem::IronGolemEntity,
            snow_golem::SnowGolemEntity,
        },
    },
    world::World,
};

pub struct GolemPattern {
    entity_type: &'static EntityType,
    blocks: Vec<BlockPos>,
    base: BlockPos,
}

// Mojang uses some BlockPattern magic, way too complex tbh
#[must_use]
pub fn find_golem_pattern(world: &Arc<World>, pos: &BlockPos) -> Option<GolemPattern> {
    let down_pos = pos.down();
    let upper = world.get_block(&down_pos);
    let lower = world.get_block(&down_pos.down());

    if upper == &Block::SNOW_BLOCK && lower == &Block::SNOW_BLOCK {
        return Some(GolemPattern {
            entity_type: &EntityType::SNOW_GOLEM,
            blocks: vec![*pos, down_pos, down_pos.down()],
            base: down_pos.down(),
        });
    }

    if upper != &Block::IRON_BLOCK || lower != &Block::IRON_BLOCK {
        // carved pumpkin: copperGolemFull, a pumpkin on a copper block
        return copper_golem_chest(upper).map(|_| GolemPattern {
            entity_type: &EntityType::COPPER_GOLEM,
            blocks: vec![*pos, down_pos],
            base: *pos,
        });
    }

    for dir in [BlockDirection::North, BlockDirection::West] {
        let arm1 = down_pos.offset(dir.to_offset());
        let arm2 = down_pos.offset(dir.opposite().to_offset());
        let above_arm1 = arm1.up();
        let above_arm2 = arm2.up();
        let below_arm1 = arm1.down();
        let below_arm2 = arm2.down();

        if world.get_block(&arm1) == &Block::IRON_BLOCK
            && world.get_block(&arm2) == &Block::IRON_BLOCK
            && world.get_block_state(&above_arm1).is_air()
            && world.get_block_state(&above_arm2).is_air()
            && world.get_block_state(&below_arm1).is_air()
            && world.get_block_state(&below_arm2).is_air()
        {
            return Some(GolemPattern {
                entity_type: &EntityType::IRON_GOLEM,
                blocks: vec![*pos, down_pos, down_pos.down(), arm1, arm2],
                base: down_pos.down(),
            });
        }
    }

    None
}

// carved pumpkin: BlockTags.COPPER blocks with the weather state and the chest that replaces them
fn copper_golem_chest(block: &Block) -> Option<(WeatherState, &'static Block)> {
    Some(match block {
        b if b == &Block::COPPER_BLOCK => (WeatherState::Unaffected, &Block::COPPER_CHEST),
        b if b == &Block::EXPOSED_COPPER => (WeatherState::Exposed, &Block::EXPOSED_COPPER_CHEST),
        b if b == &Block::WEATHERED_COPPER => {
            (WeatherState::Weathered, &Block::WEATHERED_COPPER_CHEST)
        }
        b if b == &Block::OXIDIZED_COPPER => {
            (WeatherState::Oxidized, &Block::OXIDIZED_COPPER_CHEST)
        }
        b if b == &Block::WAXED_COPPER_BLOCK => {
            (WeatherState::Unaffected, &Block::WAXED_COPPER_CHEST)
        }
        b if b == &Block::WAXED_EXPOSED_COPPER => {
            (WeatherState::Exposed, &Block::WAXED_EXPOSED_COPPER_CHEST)
        }
        b if b == &Block::WAXED_WEATHERED_COPPER => (
            WeatherState::Weathered,
            &Block::WAXED_WEATHERED_COPPER_CHEST,
        ),
        b if b == &Block::WAXED_OXIDIZED_COPPER => {
            (WeatherState::Oxidized, &Block::WAXED_OXIDIZED_COPPER_CHEST)
        }
        _ => return None,
    })
}

fn spawn_golem(world: &Arc<World>, pattern: &GolemPattern) {
    let copper_block = pattern
        .blocks
        .get(1)
        .filter(|_| pattern.entity_type == &EntityType::COPPER_GOLEM)
        .map(|pos| (*pos, world.get_block(pos)));
    let pumpkin_state = world.get_block_state_id(&pattern.blocks[0]);
    for &pos in &pattern.blocks {
        let state_id = world.get_block_state_id(&pos);
        world.set_block_state(
            &pos,
            Block::AIR.default_state.id,
            BlockFlags::NOTIFY_LISTENERS,
        );
        world.sync_world_event(
            WorldEvent::ParticlesDestroyBlock,
            pos,
            i32::from(state_id.as_u16()),
        );
    }

    let mut spawn_pos = pattern.base.to_centered_f64();
    spawn_pos.y = f64::from(pattern.base.0.y) + 0.05;
    let entity = Entity::new(world.clone(), spawn_pos, pattern.entity_type);
    let (golem, copper_golem): (Arc<dyn EntityBase>, Option<Arc<CopperGolemEntity>>) =
        if pattern.entity_type == &EntityType::COPPER_GOLEM {
            let copper_golem = CopperGolemEntity::new(entity);
            (copper_golem.clone(), Some(copper_golem))
        } else if pattern.entity_type == &EntityType::SNOW_GOLEM {
            (SnowGolemEntity::new(entity), None)
        } else {
            (IronGolemEntity::new(entity), None)
        };
    world.spawn_entity(golem);

    for &pos in &pattern.blocks {
        world.update_neighbors(&pos, None);
    }

    // carved pumpkin: replaceCopperBlockWithChest, facing like the pumpkin
    if let (Some(copper_golem), Some((copper_pos, copper_block))) = (copper_golem, copper_block)
        && let Some((weather_state, chest)) = copper_golem_chest(copper_block)
    {
        let facing = WallTorchLikeProperties::from_state_id(pumpkin_state).facing;
        let mut chest_props = ChestLikeProperties::default(chest);
        chest_props.facing = facing;
        world.set_block_state(
            &copper_pos,
            chest_props.to_state_id(chest),
            BlockFlags::NOTIFY_LISTENERS,
        );
        copper_golem.spawn(CopperWeatherState::from_block_state(weather_state));
    }
}

pub struct CarvedPumpkinBlock;

impl BlockMetadata for CarvedPumpkinBlock {
    fn ids() -> Box<[BlockId]> {
        [BlockId::JACK_O_LANTERN, BlockId::CARVED_PUMPKIN].into()
    }
}

impl BlockBehaviour for CarvedPumpkinBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        let mut props = WallTorchLikeProperties::default(args.block);
        props.facing = args
            .player
            .living_entity
            .entity
            .get_horizontal_facing()
            .opposite();
        props.to_state_id(args.block)
    }

    fn placed(&self, args: PlacedArgs<'_>) {
        if Block::from_state_id(args.old_state_id) == args.block {
            return;
        }
        if let Some(pattern) = find_golem_pattern(args.world, args.position) {
            spawn_golem(args.world, &pattern);
        }
    }

    fn player_placed(&self, args: PlayerPlacedArgs<'_>) {
        if let Some(pattern) = find_golem_pattern(args.world, args.position)
            && pattern.entity_type == &EntityType::IRON_GOLEM
        {
            args.player.trigger_advancement(
                crate::entity::player::advancement::trigger::AdvancementTrigger::SummonedEntity {
                    entity_type: "minecraft:iron_golem".to_string(),
                },
            );
        }
    }
}
