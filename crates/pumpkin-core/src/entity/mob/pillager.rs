use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag;
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;

use crate::entity::ai::behavior::fleeing::{self, Flee};
use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        look_at_entity::LookAtEntityGoal, ranged_crossbow_attack::RangedCrossbowAttackGoal,
        swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    mob::{
        Mob, MobEntity,
        crossbow_attack_mob::CrossbowAttackMob,
        equipment::{RegionalDifficulty, apply_pillager_crossbow_enchantments},
        patrol::{LongDistancePatrolGoal, PatrolData, PatrollingMonster},
        raider::{
            self, HoldGroundAttackGoal, ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider,
            RaiderCelebrationGoal, RaiderData, RaiderMoveThroughVillageGoal,
        },
    },
};
use crate::world::World;

const PILLAGER_FLEES: &[Flee] = &[raider::flee_creaking(1, 1.0, 1.2)];

const PILLAGER_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
];

pub struct PillagerEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    pub is_charging_crossbow: AtomicBool,
    pub inventory: Mutex<Vec<ItemStack>>,
}

impl PillagerEntity {
    pub const INVENTORY_SIZE: usize = 5;

    #[must_use]
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_arc = Arc::new(Self {
            mob_entity: MobEntity::new(entity),
            raider_data: RaiderData::default(),
            is_charging_crossbow: AtomicBool::new(false),
            inventory: Mutex::new(Vec::new()),
        });

        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(ObtainRaidLeaderBannerGoal));
            fleeing::apply(goal_selector, PILLAGER_FLEES);
            goal_selector.add_goal(2, Box::new(HoldGroundAttackGoal::new(10.0)));
            goal_selector.add_goal(3, Box::new(RangedCrossbowAttackGoal::new(1.0, 8.0)));
            goal_selector.add_goal(4, Box::new(LongDistancePatrolGoal::new(0.7, 0.595)));
            goal_selector.add_goal(4, Box::new(RaiderMoveThroughVillageGoal::new(1.05)));
            goal_selector.add_goal(4, Box::new(PathfindToRaidGoal::default()));
            goal_selector.add_goal(5, Box::new(RaiderCelebrationGoal));
            goal_selector.add_goal(8, Box::new(WanderAroundGoal::new(0.6)));
            goal_selector.add_goal(
                9,
                Box::new(LookAtEntityGoal::new(
                    mob_weak.clone(),
                    &EntityType::PLAYER,
                    15.0,
                    1.0,
                    false,
                )),
            );
            goal_selector.add_goal(10, LookAtEntityGoal::any_mob(mob_weak, 15.0, 0.02));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, raider::revenge_goal());
            hunting::apply(target_selector, &mob_arc.mob_entity, PILLAGER_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn is_charging_crossbow(&self) -> bool {
        self.is_charging_crossbow.load(Ordering::Relaxed)
    }

    pub fn set_charging_crossbow(&self, is_charging: bool) {
        self.is_charging_crossbow
            .store(is_charging, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(tracked_data::pillager::IS_CHARGING_CROSSBOW, is_charging);
    }

    pub fn add_to_inventory(&self, item: ItemStack) -> Option<ItemStack> {
        let mut inv = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inv.len() < Self::INVENTORY_SIZE {
            inv.push(item);
            None
        } else {
            Some(item)
        }
    }

    pub fn drop_inventory(&self) {
        let items = std::mem::take(
            &mut *self
                .inventory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load();
        let pos = entity.pos.load();
        for item in items {
            if !item.is_empty() {
                let item_entity = crate::entity::item::ItemEntity::new(
                    Entity::new(world.clone(), pos, &EntityType::ITEM),
                    item,
                );
                world.spawn_entity(Arc::new(item_entity));
            }
        }
    }
}

impl Mob for PillagerEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_patrolling_monster(&self) -> Option<&dyn PatrollingMonster> {
        Some(self)
    }

    fn as_raider(&self) -> Option<&dyn Raider> {
        Some(self)
    }

    fn as_crossbow_attack_mob(&self) -> Option<&dyn CrossbowAttackMob> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPillagerAmbient)
    }

    // pillager: crossbow and spawn enchantments (Pillager.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let difficulty = RegionalDifficulty::at(world, self.get_entity().pos.load());
        self.populate_default_equipment_slots(world, &difficulty);
        self.populate_default_equipment_enchantments(&difficulty);
        self.finalize_spawn_super(group_data)
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        self.mob_entity.set_item_slot(
            &EquipmentSlot::MAIN_HAND,
            ItemStack::new(1, &Item::CROSSBOW),
        );
    }

    fn enchant_spawned_weapon(&self, difficulty: &RegionalDifficulty) {
        self.enchant_spawned_equipment(
            &EquipmentSlot::MAIN_HAND,
            MobEntity::MAX_ENCHANTED_WEAPON_CHANCE,
            difficulty,
        );
        if rand::random_range(0..300) == 0 {
            let living = &self.mob_entity.living_entity;
            let mut equipment = living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut weapon = equipment.get(&EquipmentSlot::MAIN_HAND);
            if weapon.item.id == Item::CROSSBOW.id {
                apply_pillager_crossbow_enchantments(&mut weapon, None);
                equipment.put(&EquipmentSlot::MAIN_HAND, weapon);
            }
        }
    }

    fn preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        Some(&tag::Item::MINECRAFT_PILLAGER_PREFERRED_WEAPONS)
    }

    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        (self.has_active_raid() && stack.item.id == Item::WHITE_BANNER.id)
            || self.can_hold_item(stack)
    }

    // pillager: raid banners go into the five-slot inventory (Pillager.pickUpItem)
    fn mob_pick_up_item(&self, item: &crate::entity::item::ItemEntity, stack: &ItemStack) -> bool {
        if !self.has_active_raid() || stack.item.id != Item::WHITE_BANNER.id {
            return false;
        }
        let Some(remainder) = self.add_to_inventory(stack.clone()) else {
            let _ = self
                .mob_entity
                .living_entity
                .pickup(item.get_entity(), u32::from(stack.item_count));
            item.get_entity().remove();
            return true;
        };
        if remainder.item_count == stack.item_count {
            return false;
        }
        let picked_up = stack.item_count - remainder.item_count;
        if !self
            .mob_entity
            .living_entity
            .pickup(item.get_entity(), u32::from(picked_up))
        {
            return false;
        }
        *item
            .get_item_stack()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = remainder;
        item.init_data_tracker();
        true
    }

    fn is_max_group_size_reached(&self, group_size: i32) -> bool {
        group_size >= 1
    }

    fn mob_init_data_tracker(&self) {
        self.set_charging_crossbow(self.is_charging_crossbow.load(Ordering::Relaxed));
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_raider_nbt(nbt);
        nbt.put_bool("CanPickUpLoot", true);

        let inv = self
            .inventory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !inv.is_empty() {
            let mut items_tag = Vec::new();
            for item in inv.iter() {
                if !item.is_empty() {
                    let mut item_nbt = NbtCompound::new();
                    item.write_item_stack(&mut item_nbt);
                    items_tag.push(NbtTag::Compound(item_nbt));
                }
            }
            if !items_tag.is_empty() {
                nbt.put_list("Inventory", items_tag);
            }
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_raider_nbt(nbt);
        self.mob_entity.set_can_pick_up_loot(true);

        if let Some(inv_list) = nbt.get_list("Inventory") {
            let mut inv = self
                .inventory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inv.clear();
            for tag in inv_list {
                if let Some(compound) = tag.extract_compound()
                    && let Some(stack) = ItemStack::read_item_stack(compound)
                {
                    inv.push(stack);
                }
            }
        }
    }

    fn drop_inventory(&self) {
        PillagerEntity::drop_inventory(self);
    }
}

impl CustomSound for PillagerEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPillagerDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPillagerHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl PatrollingMonster for PillagerEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }
}

impl Raider for PillagerEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityPillagerCelebrate
    }

    // pillager: raid wave crossbow providers (Pillager.applyRaidBuffs)
    fn apply_raid_buffs(&self, wave: i32, _is_captain: bool) {
        let Some(raid_id) = self.raider_data.raid_id.load() else {
            return;
        };
        let world = self.get_entity().world.load();
        let raids = world
            .raids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(raid) = raids.get(raid_id) else {
            return;
        };
        let enchant_odds = match raid.raid_omen_level {
            2 => 0.1,
            3 => 0.25,
            4 => 0.5,
            5 => 0.75,
            _ => 0.0,
        };
        if rand::random::<f32>() > enchant_odds {
            return;
        }
        let provider_wave = if wave
            > crate::world::raid::Raid::get_num_groups(pumpkin_util::Difficulty::Normal)
        {
            Some(5)
        } else if wave > crate::world::raid::Raid::get_num_groups(pumpkin_util::Difficulty::Easy) {
            Some(3)
        } else {
            None
        };
        let Some(provider_wave) = provider_wave else {
            return;
        };
        drop(raids);
        let mut crossbow = ItemStack::new(1, &Item::CROSSBOW);
        apply_pillager_crossbow_enchantments(&mut crossbow, Some(provider_wave));
        self.mob_entity
            .set_item_slot(&EquipmentSlot::MAIN_HAND, crossbow);
    }
}

impl CrossbowAttackMob for PillagerEntity {
    fn set_charging_crossbow(&self, is_charging: bool) {
        self.set_charging_crossbow(is_charging);
    }

    fn is_charging_crossbow(&self) -> bool {
        self.is_charging_crossbow()
    }
}
