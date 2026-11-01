use pumpkin_data::Enchantment;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::{
    AttributeModifiersImpl, EnchantmentsImpl, EquipmentSlot, EquippableImpl, Operation,
};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::Taggable;
use rand::RngExt;

use crate::entity::EntityBase;
use crate::entity::living::attribute_modifier_slot_matches;
use crate::entity::mob::Mob;

// mob looting: pick up + equip ground items each tick (Mob.aiStep "looting", pickUpItem)
pub fn tick_looting(mob: &dyn Mob) {
    let mob_entity = mob.get_mob_entity();
    let living = &mob_entity.living_entity;
    let entity = &living.entity;
    if !mob_entity.can_pick_up_loot()
        || !mob.uses_mob_looting()
        || !entity.is_alive()
        || living.dead.load(std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    let world = entity.world.load();
    if !world.level_info.load().game_rules.mob_griefing {
        return;
    }
    // vanilla ITEM_PICKUP_REACH (1, 0, 1)
    let reach = entity.bounding_box.load().expand(1.0, 0.0, 1.0);
    for candidate in world.get_entities_at_box(&reach) {
        let Some(item) = candidate.get_item_entity() else {
            continue;
        };
        if item.get_entity().is_removed() || item.get_pickup_delay() > 0 {
            continue;
        }
        let stack = item
            .get_item_stack()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if stack.is_empty() || !mob.mob_wants_to_pick_up(&stack) {
            continue;
        }
        let equipped = equip_item_if_possible(mob, stack);
        if equipped.is_empty() || !living.pickup(item.get_entity(), u32::from(equipped.item_count))
        {
            continue;
        }
        let emptied = {
            let mut ground = item
                .get_item_stack()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ground.item_count = ground.item_count.saturating_sub(equipped.item_count);
            ground.is_empty()
        };
        if emptied {
            item.get_entity().remove();
        } else {
            item.init_data_tracker();
        }
    }
}

// mob looting: slot from the equippable component (LivingEntity.getEquipmentSlotForItem)
fn slot_for_item(stack: &ItemStack) -> EquipmentSlot {
    stack
        .get_data_component::<EquippableImpl>()
        .map_or(EquipmentSlot::MAIN_HAND, |equippable| {
            equippable.slot.clone()
        })
}

// mob looting: equippable here, allowed entity type (LivingEntity.isEquippableInSlot)
fn is_equippable_in_slot(mob: &dyn Mob, stack: &ItemStack, slot: &EquipmentSlot) -> bool {
    stack.get_data_component::<EquippableImpl>().map_or_else(
        || *slot == EquipmentSlot::MAIN_HAND,
        |equippable| {
            equippable.slot == slot
                && equippable
                    .allowed_entities
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(mob.get_entity().entity_type))
        },
    )
}

const fn is_armor_slot(slot: &EquipmentSlot) -> bool {
    matches!(
        slot,
        EquipmentSlot::Head(_)
            | EquipmentSlot::Chest(_)
            | EquipmentSlot::Legs(_)
            | EquipmentSlot::Feet(_)
    )
}

// mob looting: equip if better, drop the old item by drop chance (Mob.equipItemIfPossible)
pub fn equip_item_if_possible(mob: &dyn Mob, stack: ItemStack) -> ItemStack {
    let mob_entity = mob.get_mob_entity();
    let mut slot = slot_for_item(&stack);
    if !is_equippable_in_slot(mob, &stack, &slot) {
        return ItemStack::EMPTY.clone();
    }
    let equipment = &mob_entity.living_entity.entity_equipment;
    let get = |slot: &EquipmentSlot| {
        equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot)
    };
    let mut current = get(&slot);
    let mut can_replace = can_replace_current_item(mob, &stack, &current, &slot);
    if is_armor_slot(&slot) && !can_replace {
        slot = EquipmentSlot::MAIN_HAND;
        current = get(&slot);
        can_replace = current.is_empty();
    }
    if !can_replace || !mob.can_hold_item(&stack) {
        return ItemStack::EMPTY.clone();
    }
    let drop_chance = mob_entity.drop_chance(&slot);
    if !current.is_empty() && (rand::rng().random::<f32>() - 0.1).max(0.0) < drop_chance {
        mob.get_entity().spawn_at_location(current);
    }
    // vanilla EquipmentSlot.limit: one item for armor, body and saddle slots
    let mut to_equip = stack;
    if !matches!(slot, EquipmentSlot::MainHand(_) | EquipmentSlot::OffHand(_)) {
        to_equip.item_count = to_equip.item_count.min(1);
    }
    mob_entity.set_item_slot_and_drop_when_killed(&slot, to_equip.clone());
    mob_entity
        .persistence_required
        .store(true, std::sync::atomic::Ordering::Relaxed);
    to_equip
}

// mob looting: better armor / weapon (Mob.canReplaceCurrentItem)
fn can_replace_current_item(
    mob: &dyn Mob,
    new_stack: &ItemStack,
    current: &ItemStack,
    slot: &EquipmentSlot,
) -> bool {
    if current.is_empty() {
        return true;
    }
    if is_armor_slot(slot) {
        if current.get_enchantment_level(&Enchantment::BINDING_CURSE) > 0 {
            return false;
        }
        let new_defense = approximate_attribute_with(mob, new_stack, &Attributes::ARMOR, slot);
        let old_defense = approximate_attribute_with(mob, current, &Attributes::ARMOR, slot);
        if new_defense != old_defense {
            return new_defense > old_defense;
        }
        let new_tough =
            approximate_attribute_with(mob, new_stack, &Attributes::ARMOR_TOUGHNESS, slot);
        let old_tough =
            approximate_attribute_with(mob, current, &Attributes::ARMOR_TOUGHNESS, slot);
        if new_tough != old_tough {
            return new_tough > old_tough;
        }
        return can_replace_equal_item(new_stack, current);
    }
    if *slot != EquipmentSlot::MAIN_HAND {
        return false;
    }
    if let Some(preferred) = mob.preferred_weapon_type() {
        let current_preferred = current.item.has_tag(preferred);
        let new_preferred = new_stack.item.has_tag(preferred);
        if current_preferred && !new_preferred {
            return false;
        }
        if !current_preferred && new_preferred {
            return true;
        }
    }
    let new_damage = approximate_attribute_with(mob, new_stack, &Attributes::ATTACK_DAMAGE, slot);
    let old_damage = approximate_attribute_with(mob, current, &Attributes::ATTACK_DAMAGE, slot);
    if new_damage != old_damage {
        return new_damage > old_damage;
    }
    can_replace_equal_item(new_stack, current)
}

// mob looting: base value + item modifiers for the slot (ItemAttributeModifiers.compute)
fn approximate_attribute_with(
    mob: &dyn Mob,
    stack: &ItemStack,
    attribute: &Attributes,
    slot: &EquipmentSlot,
) -> f64 {
    let base = mob
        .get_mob_entity()
        .living_entity
        .get_attribute_base(attribute);
    let Some(modifiers) = stack.get_data_component::<AttributeModifiersImpl>() else {
        return base;
    };
    let matching = || {
        modifiers.attribute_modifiers.iter().filter(|modifier| {
            modifier.r#type == attribute && attribute_modifier_slot_matches(&modifier.slot, slot)
        })
    };
    let mut value = base;
    for modifier in matching() {
        if modifier.operation == Operation::AddValue {
            value += modifier.amount;
        }
    }
    let mut total = value;
    for modifier in matching() {
        if modifier.operation == Operation::AddMultipliedBase {
            total += value * modifier.amount;
        }
    }
    for modifier in matching() {
        if modifier.operation == Operation::AddMultipliedTotal {
            total *= 1.0 + modifier.amount;
        }
    }
    total
}

// mob looting: more enchantments, less damage, then custom name (Mob.canReplaceEqualItem)
fn can_replace_equal_item(new_stack: &ItemStack, current: &ItemStack) -> bool {
    let enchant_count = |stack: &ItemStack| {
        stack
            .get_data_component::<EnchantmentsImpl>()
            .map_or(0, |enchantments| enchantments.enchantment.len())
    };
    let (new_enchants, current_enchants) = (enchant_count(new_stack), enchant_count(current));
    if new_enchants != current_enchants {
        return new_enchants > current_enchants;
    }
    let (new_damage, current_damage) = (new_stack.get_damage(), current.get_damage());
    if new_damage != current_damage {
        return new_damage < current_damage;
    }
    new_stack.has_custom_name() && !current.has_custom_name()
}
