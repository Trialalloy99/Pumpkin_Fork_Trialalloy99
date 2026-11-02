use std::sync::Arc;

use crate::block::entities::sculk_catalyst::SculkCatalystBlockEntity;
use crate::block::{BlockBehaviour, BlockMetadata, OnPlaceArgs, OnScheduledTickArgs, PlacedArgs};
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::{BlockId, BlockStateId, block_properties::SculkCatalystLikeProperties};
use pumpkin_util::math::{position::BlockPos, vector2::Vector2};
use pumpkin_world::tick::TickPriority;
use pumpkin_world::world::BlockFlags;

use crate::block::BrokenArgs;
use crate::block::entities::BlockEntity;
use crate::entity::EntityBase;
use crate::world::World;

pub struct SculkCatalystBlock;

impl BlockMetadata for SculkCatalystBlock {
    fn ids() -> Box<[BlockId]> {
        [BlockId::SCULK_CATALYST].into()
    }
}

impl BlockBehaviour for SculkCatalystBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        let mut props = SculkCatalystLikeProperties::default(args.block);
        props.bloom = false;
        props.to_state_id(args.block)
    }

    // sculk_catalyst: register block entity on placed (SculkCatalystBlock.newBlockEntity)
    fn placed(&self, args: PlacedArgs<'_>) {
        let block_entity = SculkCatalystBlockEntity::new(*args.position);
        args.world.add_block_entity(Arc::new(block_entity));
    }

    // catalyst: SculkCatalystBlock.spawnAfterBreak drops 5 experience unless silk touch
    fn broken(&self, args: BrokenArgs<'_>) {
        if args.player.gamemode.load() != pumpkin_util::GameMode::Creative
            && args.world.level_info.load().game_rules.block_drops
            && args
                .player
                .inventory()
                .held_item()
                .get_enchantment_level(&pumpkin_data::Enchantment::SILK_TOUCH)
                == 0
        {
            crate::entity::experience_orb::ExperienceOrbEntity::spawn(
                args.world,
                args.position.to_centered_f64(),
                5,
            );
        }
    }

    fn on_scheduled_tick(&self, args: OnScheduledTickArgs<'_>) {
        let state_id = args.world.get_block_state_id(args.position);
        let mut props = SculkCatalystLikeProperties::from_state_id(state_id);
        if props.bloom {
            props.bloom = false;
            args.world.set_block_state(
                args.position,
                props.to_state_id(args.block),
                BlockFlags::NOTIFY_ALL,
            );
        }
    }
}

const LISTENER_RADIUS: f64 = 8.0;
const PULSE_TICKS: u8 = 8;

// catalyst: CatalystListener.handleGameEvent for ENTITY_DIE; true when the experience was consumed
pub fn on_entity_death(
    world: &Arc<World>,
    dead: &dyn EntityBase,
    killer: Option<&dyn EntityBase>,
) -> bool {
    let position = dead.get_entity().pos.load();
    let center = BlockPos::floored_v(position);
    let reach = LISTENER_RADIUS as i32;
    let mut catalysts: Vec<(f64, BlockPos, Arc<dyn BlockEntity>)> = Vec::new();
    for chunk_x in ((center.0.x - reach) >> 4)..=((center.0.x + reach) >> 4) {
        for chunk_z in ((center.0.z - reach) >> 4)..=((center.0.z + reach) >> 4) {
            let Some(chunk) = world.block_entities.get(&Vector2::new(chunk_x, chunk_z)) else {
                continue;
            };
            for (pos, block_entity) in chunk.value() {
                if block_entity.resource_location() != SculkCatalystBlockEntity::ID {
                    continue;
                }
                let pos = *pos;
                let distance_sq = pos.to_centered_f64().squared_distance_to_vec(&position);
                if distance_sq <= LISTENER_RADIUS * LISTENER_RADIUS {
                    catalysts.push((distance_sq, pos, block_entity.clone()));
                }
            }
        }
    }
    // DeliveryMode.BY_DISTANCE: the nearest catalyst consumes the experience
    let Some((_, pos, block_entity)) = catalysts.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))
    else {
        return false;
    };
    if let Some(catalyst) = block_entity
        .as_any()
        .downcast_ref::<SculkCatalystBlockEntity>()
    {
        let experience = dead.get_experience_reward(killer);
        let drops_experience = dead
            .get_living_entity()
            .is_some_and(crate::entity::living::LivingEntity::should_drop_experience);
        if drops_experience && experience > 0 {
            catalyst
                .spreader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .add_cursors(
                    BlockPos::floored_v(position.add_raw(0.0, 0.5, 0.0)),
                    experience as i32,
                );
        }
    }
    bloom(world, &pos);
    true
}

// catalyst: CatalystListener.bloom
fn bloom(world: &Arc<World>, pos: &BlockPos) {
    let (block, state_id) = world.get_block_and_state_id(pos);
    let mut props = SculkCatalystLikeProperties::from_state_id(state_id);
    props.bloom = true;
    world.set_block_state(pos, props.to_state_id(block), BlockFlags::NOTIFY_ALL);
    world.schedule_block_tick(block, *pos, PULSE_TICKS, TickPriority::Normal);
    world.play_sound_fine(
        Sound::BlockSculkCatalystBloom,
        SoundCategory::Blocks,
        &pos.to_centered_f64(),
        2.0,
        0.6 + rand::random::<f32>() * 0.4,
    );
}
