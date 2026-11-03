use pumpkin_data::item_stack::ItemStack;
use rand::RngExt;

use crate::entity::EntityBase;
use crate::plugin::api::events::entity::entity_drop_item::EntityDropItemEvent;
use crate::world::loot::{LootContextParameters, generate_loot_from_handle};

/// Vanilla `Mob.dropFromGiftLootTable` / `dropFromEntityInteractLootTable`: rolls `table` for
/// `entity` and spawns every drop at its position. `false` when nothing dropped.
pub fn drop_from_loot_table(
    entity: &dyn EntityBase,
    table: &str,
    interacting: Option<&dyn EntityBase>,
    tool: Option<ItemStack>,
) -> bool {
    let base = entity.get_entity();
    let world = base.world.load();
    let Some(table) = world.get_loot_table(table) else {
        return false;
    };
    let params = LootContextParameters {
        world: Some(&world),
        position: Some(base.pos.load()),
        this_entity: Some(entity),
        killer_entity: interacting,
        tool,
        ..Default::default()
    };
    let drops = generate_loot_from_handle(&table, rand::rng().random(), &params);
    let mut dropped = false;
    for stack in drops {
        let mut drop_event = EntityDropItemEvent::new(
            base.entity_id,
            format!("minecraft:{}", stack.item.registry_key),
            stack.item_count,
        );
        if let Some(server) = world.server.upgrade() {
            server
                .plugin_manager
                .fire_blocking(&server, &mut drop_event);
        }
        if drop_event.cancelled {
            continue;
        }
        base.spawn_at_location(stack);
        dropped = true;
    }
    dropped
}
