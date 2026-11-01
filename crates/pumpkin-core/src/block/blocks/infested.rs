use std::sync::Arc;

use pumpkin_data::{
    Block, BlockId, BlockState, BlockStateId, Enchantment, Rotation,
    block_properties::{Axis, PaleOakWoodLikeProperties},
    entity::{EntityStatus, EntityType},
};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::{GameMode, math::position::BlockPos, math::vector3::Vector3};

use crate::{
    block::{BlockBehaviour, BlockMetadata, BrokenArgs, OnPlaceArgs},
    entity::r#type::from_type,
    world::World,
};

pub struct InfestedBlock;

// silverfish: host block mapping (InfestedBlock.BLOCK_BY_HOST_BLOCK)
const HOST_TO_INFESTED: &[(&Block, &Block)] = &[
    (&Block::STONE, &Block::INFESTED_STONE),
    (&Block::COBBLESTONE, &Block::INFESTED_COBBLESTONE),
    (&Block::STONE_BRICKS, &Block::INFESTED_STONE_BRICKS),
    (
        &Block::MOSSY_STONE_BRICKS,
        &Block::INFESTED_MOSSY_STONE_BRICKS,
    ),
    (
        &Block::CRACKED_STONE_BRICKS,
        &Block::INFESTED_CRACKED_STONE_BRICKS,
    ),
    (
        &Block::CHISELED_STONE_BRICKS,
        &Block::INFESTED_CHISELED_STONE_BRICKS,
    ),
    (&Block::DEEPSLATE, &Block::INFESTED_DEEPSLATE),
];

impl InfestedBlock {
    fn infested_by_host(host: &Block) -> Option<&'static Block> {
        HOST_TO_INFESTED
            .iter()
            .find(|(candidate, _)| candidate.id == host.id)
            .map(|(_, infested)| *infested)
    }

    #[must_use]
    pub fn is_compatible_host_block(state: &BlockState) -> bool {
        Self::infested_by_host(state.id.to_block()).is_some()
    }

    #[must_use]
    pub fn infested_state_by_host(state: &BlockState) -> Option<BlockStateId> {
        Some(copy_properties(
            state,
            Self::infested_by_host(state.id.to_block())?,
        ))
    }

    #[must_use]
    pub fn host_state_by_infested(state: &BlockState) -> Option<BlockStateId> {
        let infested_id = state.id.to_block_id();
        let host = HOST_TO_INFESTED
            .iter()
            .find(|(_, infested)| infested.id == infested_id)?
            .0;
        Some(copy_properties(state, host))
    }

    // silverfish: block infestation spawn (InfestedBlock.spawnInfestation)
    pub fn spawn_infestation(world: &Arc<World>, position: &BlockPos) {
        if !world.level_info.load().game_rules.block_drops {
            return;
        }
        let bottom = position.0.to_f64();
        let silverfish = from_type(
            &EntityType::SILVERFISH,
            Vector3::new(bottom.x + 0.5, bottom.y, bottom.z + 0.5),
            world,
            uuid::Uuid::new_v4(),
        );
        world.spawn_entity(silverfish.clone());
        world.send_entity_status(
            silverfish.get_entity(),
            EntityStatus::Poof,
            ActorEventID::None,
        );
    }
}

fn copy_properties(from: &BlockState, to: &'static Block) -> BlockStateId {
    from.id
        .to_block()
        .properties(from.id)
        .map_or(to.default_state.id, |properties| {
            to.from_properties(&properties.to_props()).to_state_id(to)
        })
}

impl BlockMetadata for InfestedBlock {
    fn ids() -> Box<[BlockId]> {
        [
            BlockId::INFESTED_STONE,
            BlockId::INFESTED_COBBLESTONE,
            BlockId::INFESTED_STONE_BRICKS,
            BlockId::INFESTED_MOSSY_STONE_BRICKS,
            BlockId::INFESTED_CRACKED_STONE_BRICKS,
            BlockId::INFESTED_CHISELED_STONE_BRICKS,
            BlockId::INFESTED_DEEPSLATE,
        ]
        .into()
    }
}

impl BlockBehaviour for InfestedBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        if args.block == &Block::INFESTED_DEEPSLATE {
            let mut props = PaleOakWoodLikeProperties::default(args.block);
            props.axis = args.direction.to_axis();
            props.to_state_id(args.block)
        } else {
            args.block.default_state.id
        }
    }

    fn rotate(
        &self,
        block: &Block,
        state_id: BlockStateId,
        rotation: Rotation,
    ) -> &'static BlockState {
        if block == &Block::INFESTED_DEEPSLATE {
            let mut props = PaleOakWoodLikeProperties::from_state_id(state_id);
            if matches!(
                rotation,
                Rotation::Clockwise90 | Rotation::CounterClockwise90
            ) {
                props.axis = match props.axis {
                    Axis::X => Axis::Z,
                    Axis::Z => Axis::X,
                    Axis::Y => Axis::Y,
                };
            }
            BlockState::from_id(props.to_state_id(block))
        } else {
            BlockState::from_id(state_id)
        }
    }

    fn broken(&self, args: BrokenArgs<'_>) {
        // TODO: ugly fix, use onStacksDropped
        if args.player.gamemode.load() == GameMode::Creative
            || !args.world.level_info.load().game_rules.block_drops
            || args
                .player
                .inventory()
                .held_item()
                .get_enchantment_level(&Enchantment::SILK_TOUCH)
                > 0
        {
            return;
        }
        Self::spawn_infestation(args.world, args.position);
    }
}
