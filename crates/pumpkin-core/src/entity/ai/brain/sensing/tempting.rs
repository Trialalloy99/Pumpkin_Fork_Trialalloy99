use pumpkin_data::attributes::Attributes;
use pumpkin_data::item_stack::ItemStack;

use super::super::BrainTick;
use super::super::memory::{MemoryModuleId, types};
use super::Sensor;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::player::Player;

const REQUIRES: [MemoryModuleId; 1] = [types::TEMPTING_PLAYER.id()];

// sensor: TemptingSensor.forAnimal
pub struct TemptingSensor;

fn is_temptation(tick: &BrainTick<'_>, stack: &ItemStack) -> bool {
    !stack.is_empty()
        && tick
            .mob
            .as_animal()
            .is_some_and(|animal| animal.is_food(stack))
}

fn holding_temptation(tick: &BrainTick<'_>, player: &Player) -> bool {
    is_temptation(tick, &player.inventory().held_item())
        || is_temptation(tick, &player.inventory().off_hand_item())
}

impl Sensor for TemptingSensor {
    fn requires(&self) -> &'static [MemoryModuleId] {
        &REQUIRES
    }

    fn do_tick(&mut self, tick: &mut BrainTick<'_>) {
        let mob_entity = tick.mob.get_mob_entity();
        let range = mob_entity
            .living_entity
            .get_attribute_value(&Attributes::TEMPT_RANGE);
        let predicate = TargetPredicate::create_non_attackable()
            .ignore_visibility()
            .set_base_max_distance(range);
        let pos = mob_entity.living_entity.entity.pos.load();
        let player = tick.world.get_nearest_player(pos, range, |player| {
            !player.is_spectator()
                && holding_temptation(tick, player)
                && predicate.test(tick.world, Some(tick.mob), player.as_ref())
                && !mob_entity
                    .living_entity
                    .entity
                    .has_passenger(player.entity_id())
        });
        tick.state.set_optional(types::TEMPTING_PLAYER, player);
    }
}
