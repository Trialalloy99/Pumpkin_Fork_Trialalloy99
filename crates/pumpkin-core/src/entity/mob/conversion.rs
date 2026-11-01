use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;

use crate::entity::EntityBase;
use crate::entity::mob::Mob;
use crate::entity::mob::spawn::SpawnReason;
use crate::entity::mob::zombie::zombie_base_of;
use crate::entity::r#type::from_type;

const ALL_SLOTS: [EquipmentSlot; 8] = [
    EquipmentSlot::MAIN_HAND,
    EquipmentSlot::OFF_HAND,
    EquipmentSlot::FEET,
    EquipmentSlot::LEGS,
    EquipmentSlot::CHEST,
    EquipmentSlot::HEAD,
    EquipmentSlot::BODY,
    EquipmentSlot::SADDLE,
];

// mob conversion: replace `from` with a new `to_type` mob (Mob.convertTo, ConversionType.SINGLE)
pub fn convert_single(
    from: &dyn Mob,
    to_type: &'static EntityType,
    keep_equipment: bool,
    preserve_can_pick_up_loot: bool,
    after: impl FnOnce(&Arc<dyn EntityBase>),
) -> Option<Arc<dyn EntityBase>> {
    let entity = from.get_entity();
    if entity.is_removed() {
        return None;
    }
    let world = entity.world.load_full();
    let converted = from_type(to_type, entity.pos.load(), &world, uuid::Uuid::new_v4());
    let to_entity = converted.get_entity();
    let root_passenger = entity
        .passengers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .first()
        .cloned();
    let vehicle = entity.get_vehicle();
    let leash_holder = entity
        .leashed_to
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    to_entity.set_rotation(entity.yaw.load(), entity.pitch.load());
    to_entity.head_yaw.store(entity.head_yaw.load());
    to_entity.body_yaw.store(entity.body_yaw.load());
    to_entity.velocity.store(entity.velocity.load());
    to_entity
        .on_ground
        .store(entity.on_ground.load(Ordering::Relaxed), Ordering::Relaxed);

    let from_mob = from.get_mob_entity();
    let from_living = &from_mob.living_entity;
    if let Some(to_mob) = converted.get_mob() {
        let to_mob_entity = to_mob.get_mob_entity();
        to_mob_entity
            .spawn_reason
            .store(SpawnReason::Conversion as u8, Ordering::Relaxed);
        let to_living = &to_mob_entity.living_entity;
        if keep_equipment {
            let mut from_equipment = from_living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for slot in &ALL_SLOTS {
                let stack = from_equipment.put(slot, ItemStack::EMPTY.clone());
                if !stack.is_empty() {
                    to_mob_entity.set_item_slot(slot, stack);
                    to_mob_entity.set_drop_chance(slot, from_mob.drop_chance(slot));
                }
            }
        }
        to_living
            .fall_distance
            .store(from_living.fall_distance.load());
        to_living.last_hurt_by_player_memory_time.store(
            from_living
                .last_hurt_by_player_memory_time
                .load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        to_living.set_absorption(from_living.get_absorption());
        to_living.hurt_cooldown.store(
            from_living.hurt_cooldown.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        for effect in from_living
            .active_effects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            to_living.add_effect(effect.clone());
        }
        if from_living.is_baby()
            || from
                .as_ageable()
                .is_some_and(crate::entity::ageable::AgeableMob::is_baby)
        {
            to_mob.spawn_as_baby();
        }
        if preserve_can_pick_up_loot {
            to_mob_entity.set_can_pick_up_loot(from_mob.can_pick_up_loot());
        }
        to_mob_entity.set_left_handed(from_mob.is_left_handed());
        to_mob_entity.set_no_ai(from_mob.is_no_ai());
        if from_mob.persistence_required.load(Ordering::Relaxed) {
            to_mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
    }

    if let Some(name) = &**entity.custom_name.load() {
        to_entity.set_custom_name(name.clone());
    }
    to_entity.set_custom_name_visible(entity.custom_name_visible.load(Ordering::Relaxed));
    to_entity
        .fire_ticks
        .store(entity.fire_ticks.load(Ordering::Relaxed), Ordering::Relaxed);
    to_entity.invulnerable.store(
        entity.invulnerable.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    to_entity.set_has_no_gravity(entity.has_no_gravity());
    to_entity.portal_cooldown.store(
        entity.portal_cooldown.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    to_entity.set_silent(entity.is_silent());
    to_entity
        .scoreboard_tags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(
            entity
                .scoreboard_tags
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned(),
        );
    let from_base: &dyn EntityBase = from;
    if let (Some(from_zombie), Some(to_zombie)) = (
        zombie_base_of(from_base),
        zombie_base_of(converted.as_ref()),
    ) && from_zombie.can_break_doors()
    {
        to_zombie.set_can_break_doors(true, to_zombie);
    }
    if let Some(team) = from_base.get_team_name() {
        to_entity.join_team(&team);
    }

    after(&converted);
    world.spawn_entity(converted.clone());
    // conversion: hand over rider, vehicle and leash (ConversionType.SINGLE)
    if let Some(passenger) = root_passenger {
        entity.remove_passenger(passenger.get_entity().entity_id);
        to_entity.add_passenger(converted.clone(), passenger);
    }
    if let Some(vehicle) = vehicle {
        vehicle.get_entity().remove_passenger(entity.entity_id);
        vehicle
            .get_entity()
            .add_passenger(vehicle.clone(), converted.clone());
    }
    if let Some(holder) = leash_holder {
        to_entity.attach_leash(holder);
    }
    entity.remove();
    Some(converted)
}
