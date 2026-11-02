use std::sync::Arc;

use pumpkin_data::Block;
use pumpkin_data::block_properties::CampfireLikeProperties;
use pumpkin_data::entity::EntityType;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;

use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::Sensor;
use crate::entity::EntityBase;
use crate::entity::mob::piglin::PiglinEntity;
use crate::entity::mob::piglin_ai::PiglinAi;
use crate::entity::player::Player;

const REPELLENT_HORIZONTAL: i32 = 8;
const REPELLENT_VERTICAL: i32 = 4;

const REQUIRES: [MemoryModuleId; 12] = [
    types::NEAREST_VISIBLE_LIVING_ENTITIES.id(),
    types::NEAREST_LIVING_ENTITIES.id(),
    types::NEAREST_VISIBLE_NEMESIS.id(),
    types::NEAREST_TARGETABLE_PLAYER_NOT_WEARING_GOLD.id(),
    types::NEAREST_PLAYER_HOLDING_WANTED_ITEM.id(),
    types::NEAREST_VISIBLE_HUNTABLE_HOGLIN.id(),
    types::NEAREST_VISIBLE_BABY_HOGLIN.id(),
    types::NEAREST_VISIBLE_ADULT_PIGLINS.id(),
    types::NEARBY_ADULT_PIGLINS.id(),
    types::VISIBLE_ADULT_PIGLIN_COUNT.id(),
    types::VISIBLE_ADULT_HOGLIN_COUNT.id(),
    types::NEAREST_REPELLENT.id(),
];

// sensor: PiglinSpecificSensor
pub struct PiglinSpecificSensor;

// piglin: AbstractPiglin.isAdult for piglins and brutes
#[must_use]
pub fn is_adult_piglin(entity: &dyn EntityBase) -> bool {
    let entity_type = entity.get_entity().entity_type;
    if entity_type == &EntityType::PIGLIN_BRUTE {
        return true;
    }
    entity_type == &EntityType::PIGLIN
        && entity
            .cast_any()
            .downcast_ref::<PiglinEntity>()
            .is_some_and(PiglinEntity::is_adult)
}

fn is_valid_repellent(block: &Block, state_id: pumpkin_data::BlockStateId) -> bool {
    block.has_tag(&tag::Block::MINECRAFT_PIGLIN_REPELLENTS)
        && (block.id != Block::SOUL_CAMPFIRE.id
            || CampfireLikeProperties::from_state_id(state_id).lit)
}

impl Sensor for PiglinSpecificSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    #[expect(clippy::too_many_lines)]
    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        // sensor: findNearestRepellent, first in the box
        let origin = tick.mob.get_entity().block_pos.load();
        let mut repellent: Option<BlockPos> = None;
        'scan: for dy in -REPELLENT_VERTICAL..=REPELLENT_VERTICAL {
            for dx in -REPELLENT_HORIZONTAL..=REPELLENT_HORIZONTAL {
                for dz in -REPELLENT_HORIZONTAL..=REPELLENT_HORIZONTAL {
                    let pos = BlockPos::new(origin.0.x + dx, origin.0.y + dy, origin.0.z + dz);
                    let (block, state_id) = tick.world.get_block_and_state_id(&pos);
                    if is_valid_repellent(block, state_id) {
                        repellent = Some(pos);
                        break 'scan;
                    }
                }
            }
        }
        tick.state.set_optional(types::NEAREST_REPELLENT, repellent);

        let mut nemesis: Option<Arc<dyn EntityBase>> = None;
        let mut huntable_hoglin: Option<Arc<dyn EntityBase>> = None;
        let mut baby_hoglin: Option<Arc<dyn EntityBase>> = None;
        let mut zombified: Option<Arc<dyn EntityBase>> = None;
        let mut not_wearing_gold: Option<Arc<Player>> = None;
        let mut holding_wanted_item: Option<Arc<Player>> = None;
        let mut visible_adult_hoglin_count = 0;
        let mut visible_adult_piglins: Vec<Arc<dyn EntityBase>> = Vec::new();
        {
            let ctx = tick.visibility();
            if let Some(visible) = tick.state.get(types::NEAREST_VISIBLE_LIVING_ENTITIES) {
                for entity in visible.find_all(&ctx, |_| true) {
                    let entity_type = entity.get_entity().entity_type;
                    let is_baby = entity
                        .get_living_entity()
                        .is_some_and(crate::entity::living::LivingEntity::is_baby);
                    if entity_type == &EntityType::HOGLIN {
                        if is_baby && baby_hoglin.is_none() {
                            baby_hoglin = Some(entity.clone());
                        } else if !is_baby {
                            visible_adult_hoglin_count += 1;
                            if huntable_hoglin.is_none()
                                && entity.get_mob().is_some_and(|mob| mob.can_be_hunted())
                            {
                                huntable_hoglin = Some(entity.clone());
                            }
                        }
                    } else if entity_type == &EntityType::PIGLIN_BRUTE {
                        visible_adult_piglins.push(entity.clone());
                    } else if entity_type == &EntityType::PIGLIN {
                        if !is_baby {
                            visible_adult_piglins.push(entity.clone());
                        }
                    } else if let Some(player) = entity.get_player() {
                        if not_wearing_gold.is_none()
                            && !PiglinAi::is_wearing_safe_armor(&player.living_entity)
                            && tick.mob.can_attack(entity.as_ref())
                        {
                            not_wearing_gold =
                                tick.world.get_player_by_id(entity.get_entity().entity_id);
                        }
                        if holding_wanted_item.is_none()
                            && !player.is_spectator()
                            && PiglinAi::is_player_holding_loved_item(player)
                        {
                            holding_wanted_item =
                                tick.world.get_player_by_id(entity.get_entity().entity_id);
                        }
                    } else if nemesis.is_none()
                        && (entity_type == &EntityType::WITHER_SKELETON
                            || entity_type == &EntityType::WITHER)
                    {
                        nemesis = Some(entity.clone());
                    } else if zombified.is_none() && PiglinAi::is_zombified(entity_type) {
                        zombified = Some(entity.clone());
                    }
                }
            }
        }

        let nearby_adults: Vec<Arc<dyn EntityBase>> = tick
            .state
            .get(types::NEAREST_LIVING_ENTITIES)
            .map(|entities| {
                entities
                    .iter()
                    .filter(|entity| {
                        let entity_type = entity.get_entity().entity_type;
                        (entity_type == &EntityType::PIGLIN
                            || entity_type == &EntityType::PIGLIN_BRUTE)
                            && is_adult_piglin(entity.as_ref())
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        tick.state
            .set_optional(types::NEAREST_VISIBLE_NEMESIS, nemesis);
        tick.state
            .set_optional(types::NEAREST_VISIBLE_HUNTABLE_HOGLIN, huntable_hoglin);
        tick.state
            .set_optional(types::NEAREST_VISIBLE_BABY_HOGLIN, baby_hoglin);
        tick.state
            .set_optional(types::NEAREST_VISIBLE_ZOMBIFIED, zombified);
        tick.state.set_optional(
            types::NEAREST_TARGETABLE_PLAYER_NOT_WEARING_GOLD,
            not_wearing_gold,
        );
        tick.state.set_optional(
            types::NEAREST_PLAYER_HOLDING_WANTED_ITEM,
            holding_wanted_item,
        );
        tick.state.set(types::NEARBY_ADULT_PIGLINS, nearby_adults);
        let visible_count = i32::try_from(visible_adult_piglins.len()).unwrap_or(i32::MAX);
        tick.state
            .set(types::NEAREST_VISIBLE_ADULT_PIGLINS, visible_adult_piglins);
        tick.state
            .set(types::VISIBLE_ADULT_PIGLIN_COUNT, visible_count);
        tick.state.set(
            types::VISIBLE_ADULT_HOGLIN_COUNT,
            visible_adult_hoglin_count,
        );
    }
}
