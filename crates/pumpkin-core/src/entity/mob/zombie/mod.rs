use super::{Mob, MobEntity, MobMetadata};
use crate::entity::ai::goal::break_door::BreakDoorGoal;
use crate::entity::ai::goal::destroy_egg::DestroyEggGoal;
use crate::entity::ai::goal::look_around::RandomLookAroundGoal;
use crate::entity::ai::goal::revenge::RevengeGoal;
use crate::entity::ai::goal::wander_around::WanderAroundGoal;
use crate::entity::ai::goal::zombie_attack::ZombieAttackGoal;
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::mob::conversion;
use crate::entity::mob::equipment::RegionalDifficulty;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::r#type::from_type;
use crate::entity::{
    Entity, EntityBase,
    ai::behavior::hunting::{self, Hunt},
    ai::goal::look_at_entity::LookAtEntityGoal,
    ai::target_match::TargetMatch,
};
use crate::world::World;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::Difficulty;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

pub mod drowned;
pub mod husk;
#[allow(clippy::module_inception)]
pub mod zombie;
pub mod zombie_villager;

const SPEED_MODIFIER_BABY_ID: &str = "minecraft:baby";
const REINFORCEMENT_CALLER_CHARGE_ID: &str = "minecraft:reinforcement_caller_charge";
const REINFORCEMENT_CALLEE_CHARGE_ID: &str = "minecraft:reinforcement_callee_charge";
const LEADER_ZOMBIE_BONUS_ID: &str = "minecraft:leader_zombie_bonus";
const ZOMBIE_RANDOM_SPAWN_BONUS_ID: &str = "minecraft:zombie_random_spawn_bonus";
const RANDOM_SPAWN_BONUS_ID: &str = "minecraft:random_spawn_bonus";
const BABY_SPAWN_CHANCE: f32 = 0.05;
const BABY_XP_MULTIPLIER: f64 = 2.5;
const REINFORCEMENT_ATTEMPTS: usize = 50;
const IN_WATER_CONVERSION_TICKS: i32 = 600;
const DROWNED_CONVERSION_TICKS: i32 = 300;
const NOT_CONVERTING: i32 = -1;

const ZOMBIE_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
    hunting::baby_turtles(5, true),
];

pub struct ZombieEntityBase {
    pub mob_entity: MobEntity,
    pub can_break_doors: AtomicBool,
    pub is_baby: AtomicBool,
    in_water_time: AtomicI32,
    drowned_conversion_time: AtomicI32,
}

impl ZombieEntityBase {
    pub fn new(entity: Entity) -> Arc<Self> {
        Self::with_can_break_doors(entity, false)
    }

    pub fn with_can_break_doors(entity: Entity, can_break_doors: bool) -> Arc<Self> {
        Self::with_targets(entity, can_break_doors, Self::revenge_goal(), ZOMBIE_HUNTS)
    }

    /// Vanilla `HurtByTargetGoal(this).setAlertOthers(ZombifiedPiglin.class)`.
    #[must_use]
    pub fn revenge_goal() -> RevengeGoal {
        RevengeGoal::new(true)
            .alerting_others_except(|entity_type| entity_type == &EntityType::ZOMBIFIED_PIGLIN)
    }

    /// For subclasses that replace the target goals, like the drowned.
    pub fn with_targets(
        entity: Entity,
        can_break_doors: bool,
        revenge: RevengeGoal,
        hunts: &[Hunt],
    ) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let zombie = Self {
            mob_entity,
            can_break_doors: AtomicBool::new(can_break_doors),
            is_baby: AtomicBool::new(false),
            in_water_time: AtomicI32::new(0),
            drowned_conversion_time: AtomicI32::new(NOT_CONVERTING),
        };
        let mob_arc = Arc::new(zombie);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        // zombie: goals (Zombie.registerGoals + addBehaviourGoals), no float goal: zombies sink
        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            goal_selector.add_goal(4, DestroyEggGoal::new(1.0, 3));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
            if can_break_doors {
                goal_selector.add_goal(1, Box::new(BreakDoorGoal::default()));
            }
            goal_selector.add_goal(3, ZombieAttackGoal::new(1.0, false));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::water_avoiding(1.0)));

            target_selector.add_goal(1, Box::new(revenge));
            hunting::apply(target_selector, &mob_arc.mob_entity, hunts);
        };

        mob_arc
    }

    #[must_use]
    pub fn can_break_doors(&self) -> bool {
        self.can_break_doors.load(Ordering::Relaxed)
    }

    pub fn set_can_break_doors(&self, can_break_doors: bool, mob: &dyn Mob) {
        if self
            .can_break_doors
            .swap(can_break_doors, Ordering::Relaxed)
            != can_break_doors
        {
            if can_break_doors {
                self.mob_entity
                    .goals_selector
                    .add_goal(1, Box::new(BreakDoorGoal::default()));
            } else {
                self.mob_entity
                    .goals_selector
                    .remove_goal::<BreakDoorGoal>(mob);
            }
        }
    }
}

impl ZombieEntityBase {
    // zombie: baby flag + 50% speed bonus (Zombie.setBaby)
    pub fn set_baby(&self, baby: bool) {
        self.mob_entity.set_baby_flag(
            &self.is_baby,
            pumpkin_data::tracked_data::zombie::BABY,
            baby,
        );
        apply_baby_speed(&self.mob_entity, baby);
    }

    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.is_baby.load(Ordering::Relaxed)
    }

    // zombie: baby xp x2.5 (Zombie.getBaseExperienceReward)
    #[must_use]
    pub fn experience_reward(&self, base: u32) -> u32 {
        if self.is_baby() {
            (f64::from(base) * BABY_XP_MULTIPLIER) as u32
        } else {
            base
        }
    }

    fn set_drowned_converting(&self, ticks: i32) {
        self.drowned_conversion_time.store(ticks, Ordering::Relaxed);
        self.mob_entity.living_entity.entity.set_synced_data(
            pumpkin_data::tracked_data::zombie::DATA_DROWNED_CONVERSION_ID,
            ticks != NOT_CONVERTING,
        );
    }

    #[must_use]
    pub fn is_drowned_converting(&self) -> bool {
        self.drowned_conversion_time.load(Ordering::Relaxed) != NOT_CONVERTING
    }

    // zombie: drown -> drowned, husk -> zombie (Zombie.tick, ConversionTracker)
    pub fn tick_drowning(&self, mob: &dyn Mob) {
        let entity = &self.mob_entity.living_entity.entity;
        let Some((target_type, sound)) = drowning_conversion(entity.entity_type) else {
            return;
        };
        if !entity.is_alive() || self.mob_entity.is_no_ai() {
            return;
        }
        if !entity.is_submerged_in_water() {
            self.in_water_time.store(-1, Ordering::Relaxed);
            if self.is_drowned_converting() {
                self.set_drowned_converting(NOT_CONVERTING);
            }
            return;
        }
        if self.is_drowned_converting() {
            let left = self.drowned_conversion_time.fetch_sub(1, Ordering::Relaxed) - 1;
            if left < 0 {
                self.convert_drowned(mob, target_type, sound);
            }
        } else if self.in_water_time.fetch_add(1, Ordering::Relaxed) + 1
            >= IN_WATER_CONVERSION_TICKS
        {
            self.set_drowned_converting(DROWNED_CONVERSION_TICKS);
        }
    }

    fn convert_drowned(&self, mob: &dyn Mob, target_type: &'static EntityType, sound: WorldEvent) {
        let can_break_doors = self.can_break_doors();
        let world = self.mob_entity.living_entity.entity.world.load_full();
        conversion::convert_single(mob, target_type, true, true, |converted| {
            if let Some(base) = zombie_base_of(converted.as_ref()) {
                if can_break_doors {
                    base.set_can_break_doors(true, base);
                }
                let difficulty = RegionalDifficulty::at(&world, converted.get_entity().pos.load());
                base.handle_attributes(difficulty.special_multiplier, true);
            }
            let converted_entity = converted.get_entity();
            if !converted_entity.is_silent() {
                world.sync_world_event(sound, converted_entity.block_pos.load(), 0);
            }
        });
    }

    // zombie: spawn rolls (Zombie.finalizeSpawn)
    pub fn finalize_zombie_spawn(
        &self,
        mob: &dyn Mob,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = mob.finalize_spawn_super(group_data);
        let difficulty = RegionalDifficulty::at(world, mob.get_entity().pos.load());
        let multiplier = difficulty.special_multiplier;
        let mut random = rand::rng();
        self.mob_entity
            .set_can_pick_up_loot(random.random::<f32>() < 0.55 * multiplier);
        let group_data = group_data.unwrap_or(SpawnGroupData::Zombie {
            baby: random.random::<f32>() < BABY_SPAWN_CHANCE,
        });
        if let SpawnGroupData::Zombie { baby } = group_data {
            if baby {
                mob.spawn_as_baby();
            }
            self.set_can_break_doors(random.random::<f32>() < multiplier * 0.1, mob);
            mob.populate_default_equipment_slots(world, &difficulty);
            mob.populate_default_equipment_enchantments(&difficulty);
        }
        maybe_wear_halloween_head(&self.mob_entity);
        self.handle_attributes(multiplier, false);
        Some(group_data)
    }

    // zombie: random knockback/follow bonuses, leader zombies (Zombie.handleAttributes)
    pub fn handle_attributes(&self, multiplier: f32, conversion: bool) {
        if handle_zombie_attributes(&self.mob_entity, multiplier, conversion, true) {
            self.set_can_break_doors(true, self);
        }
    }

    // zombie: ignite target, infect villager (Zombie.doHurtTarget, killedEntity)
    pub fn on_zombie_attack(&self, _mob: &dyn Mob, target: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load_full();
        let difficulty = RegionalDifficulty::at(&world, entity.pos.load()).effective_difficulty;
        if self.main_hand_empty() && entity.is_on_fire() && rand::random::<f32>() < difficulty * 0.3
        {
            target.set_on_fire_for((2 * difficulty as i32) as f32);
        }
        let world_difficulty = world.level_info.load().difficulty;
        if target.get_entity().entity_type == &EntityType::VILLAGER
            && target
                .get_living_entity()
                .is_some_and(|living| living.health.load() <= 0.0)
            && matches!(world_difficulty, Difficulty::Normal | Difficulty::Hard)
            && (world_difficulty == Difficulty::Hard || rand::random::<bool>())
            && let Some(villager) = target.get_mob()
        {
            let mut villager_nbt = NbtCompound::new();
            villager.mob_write_nbt(&mut villager_nbt);
            let silent = entity.is_silent();
            let block_pos = entity.block_pos.load();
            conversion::convert_single(
                villager,
                &EntityType::ZOMBIE_VILLAGER,
                true,
                true,
                |converted| {
                    if let Some(zombie_villager) = converted.get_mob() {
                        zombie_villager.mob_read_nbt(&villager_nbt);
                    }
                    if !silent {
                        world.sync_world_event(WorldEvent::SoundZombieInfected, block_pos, 0);
                    }
                },
            );
        }
    }

    #[must_use]
    pub fn main_hand_empty(&self) -> bool {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::MAIN_HAND)
            .is_empty()
    }

    // zombie: call reinforcements on hard (Zombie.hurtServer)
    pub fn on_zombie_hurt(&self, attacker: Option<&dyn EntityBase>) {
        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load_full();
        let level_info = world.level_info.load();
        if level_info.difficulty != Difficulty::Hard || !level_info.game_rules.spawn_monsters {
            return;
        }
        let target = self.mob_entity.get_target().or_else(|| {
            attacker.and_then(|attacker| world.get_entity_by_id(attacker.get_entity().entity_id))
        });
        let Some(target) = target.filter(|target| target.get_living_entity().is_some()) else {
            return;
        };
        let living = &self.mob_entity.living_entity;
        if rand::random::<f64>() >= living.get_attribute_value(&Attributes::SPAWN_REINFORCEMENTS) {
            return;
        }
        let entity_type = entity.entity_type;
        let origin = entity.block_pos.load();
        let thundering = world.is_thundering();
        let mut random = rand::rng();
        let mut offset = || random.random_range(7..=40) * random.random_range(-1..=1);
        for _ in 0..REINFORCEMENT_ATTEMPTS {
            let pos = origin.add(offset(), offset(), offset());
            if !crate::world::natural_spawner::is_spawn_position_ok(&world, &pos, entity_type)
                || !crate::entity::r#type::check_spawn_rules(entity_type, &world, &pos, thundering)
            {
                continue;
            }
            let spawn_pos =
                Vector3::new(f64::from(pos.0.x), f64::from(pos.0.y), f64::from(pos.0.z));
            let spawn_box =
                entity_type.get_spawn_bounding_box(spawn_pos.x, spawn_pos.y, spawn_pos.z);
            if !world.get_nearby_players(spawn_pos, 7.0).is_empty()
                || !world.is_space_empty(spawn_box)
                || world.contains_any_liquid(spawn_box)
            {
                continue;
            }
            let reinforcement = from_type(entity_type, spawn_pos, &world, uuid::Uuid::new_v4());
            if let Some(reinforcement_mob) = reinforcement.get_mob() {
                reinforcement_mob
                    .get_mob_entity()
                    .set_target(Some(target.clone()));
            }
            crate::entity::mob::spawn::finalize_spawn(&reinforcement, &world, None);
            if let Some(reinforcement_mob) = reinforcement.get_mob() {
                charge_reinforcements(
                    reinforcement_mob.get_mob_entity(),
                    REINFORCEMENT_CALLEE_CHARGE_ID,
                    -0.05,
                );
            }
            world.spawn_entity(reinforcement);
            let existing = living
                .attributes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&Attributes::SPAWN_REINFORCEMENTS.id)
                .and_then(|attribute| {
                    attribute
                        .modifiers
                        .iter()
                        .find(|modifier| modifier.id == REINFORCEMENT_CALLER_CHARGE_ID)
                        .map(|modifier| modifier.amount)
                })
                .unwrap_or(0.0);
            charge_reinforcements(
                &self.mob_entity,
                REINFORCEMENT_CALLER_CHARGE_ID,
                existing - 0.05,
            );
            break;
        }
    }
}

// zombie family: baby +50% speed (Zombie.setBaby)
pub fn apply_baby_speed(mob_entity: &MobEntity, baby: bool) {
    let mut attributes = mob_entity
        .living_entity
        .attributes
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
        speed.remove_modifier(SPEED_MODIFIER_BABY_ID);
        if baby {
            speed.add_or_replace_modifier(Modifier {
                id: SPEED_MODIFIER_BABY_ID.to_string(),
                amount: 0.5,
                operation: ModifierOperation::MultiplyBase,
            });
        }
    }
}

// zombie family: spawn attribute rolls, returns true for a leader (Zombie.handleAttributes)
pub fn handle_zombie_attributes(
    mob_entity: &MobEntity,
    multiplier: f32,
    conversion: bool,
    randomize_reinforcements: bool,
) -> bool {
    let mut random = rand::rng();
    let living = &mob_entity.living_entity;
    let leader = {
        let mut attributes = living
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(reinforcements) = attributes.get_mut(&Attributes::SPAWN_REINFORCEMENTS.id) {
            reinforcements.base_value = if randomize_reinforcements {
                random.random::<f64>() * 0.1
            } else {
                0.0
            };
            reinforcements.mark_dirty();
        }
        if let Some(knockback) = attributes.get_mut(&Attributes::KNOCKBACK_RESISTANCE.id) {
            knockback.add_or_replace_permanent_modifier(Modifier {
                id: RANDOM_SPAWN_BONUS_ID.to_string(),
                amount: random.random::<f64>() * 0.05,
                operation: ModifierOperation::Add,
            });
        }
        let follow_bonus = random.random::<f64>() * 1.5 * f64::from(multiplier);
        if follow_bonus > 1.0
            && let Some(follow) = attributes.get_mut(&Attributes::FOLLOW_RANGE.id)
        {
            follow.add_or_replace_permanent_modifier(Modifier {
                id: ZOMBIE_RANDOM_SPAWN_BONUS_ID.to_string(),
                amount: follow_bonus,
                operation: ModifierOperation::MultiplyTotal,
            });
        }
        let leader = random.random::<f32>() < multiplier * 0.05;
        if leader {
            if let Some(reinforcements) = attributes.get_mut(&Attributes::SPAWN_REINFORCEMENTS.id) {
                reinforcements.add_or_replace_permanent_modifier(Modifier {
                    id: LEADER_ZOMBIE_BONUS_ID.to_string(),
                    amount: random.random::<f64>() * 0.25 + 0.5,
                    operation: ModifierOperation::Add,
                });
            }
            if let Some(health) = attributes.get_mut(&Attributes::MAX_HEALTH.id) {
                health.add_or_replace_permanent_modifier(Modifier {
                    id: LEADER_ZOMBIE_BONUS_ID.to_string(),
                    amount: random.random::<f64>() * 3.0 + 1.0,
                    operation: ModifierOperation::MultiplyTotal,
                });
            }
        }
        leader
    };
    if leader && !conversion {
        living.set_health(living.get_attribute_value(&Attributes::MAX_HEALTH) as f32);
    }
    leader
}

// zombie family: halloween pumpkin head (Zombie.finalizeSpawn)
pub fn maybe_wear_halloween_head(mob_entity: &MobEntity) {
    let mut random = rand::rng();
    if !crate::server::seasonal_events::is_halloween()
        || !mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::HEAD)
            .is_empty()
        || random.random::<f32>() >= 0.25
    {
        return;
    }
    let head = if random.random::<f32>() < 0.1 {
        &Item::JACK_O_LANTERN
    } else {
        &Item::CARVED_PUMPKIN
    };
    mob_entity.set_item_slot(&EquipmentSlot::HEAD, ItemStack::new(1, head));
    mob_entity.set_drop_chance(&EquipmentSlot::HEAD, 0.0);
}

fn charge_reinforcements(mob: &MobEntity, id: &str, amount: f64) {
    if let Some(attribute) = mob
        .living_entity
        .attributes
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&Attributes::SPAWN_REINFORCEMENTS.id)
    {
        attribute.add_or_replace_permanent_modifier(Modifier {
            id: id.to_string(),
            amount,
            operation: ModifierOperation::Add,
        });
    }
}

// zombie family: what drowning turns this type into (Zombie.convertsToWhenDrowning)
fn drowning_conversion(entity_type: &EntityType) -> Option<(&'static EntityType, WorldEvent)> {
    if entity_type == &EntityType::ZOMBIE {
        Some((&EntityType::DROWNED, WorldEvent::SoundZombieToDrowned))
    } else if entity_type == &EntityType::HUSK {
        Some((&EntityType::ZOMBIE, WorldEvent::SoundHuskToZombie))
    } else {
        None
    }
}

// zombie family: shared base of any zombie wrapper
#[must_use]
pub fn zombie_base_of(entity: &dyn EntityBase) -> Option<&ZombieEntityBase> {
    let any = entity.cast_any();
    any.downcast_ref::<zombie::ZombieEntity>()
        .map(zombie::ZombieEntity::base)
        .or_else(|| {
            any.downcast_ref::<husk::HuskEntity>()
                .map(husk::HuskEntity::base)
        })
        .or_else(|| {
            any.downcast_ref::<drowned::DrownedEntity>()
                .map(drowned::DrownedEntity::base)
        })
        .or_else(|| {
            any.downcast_ref::<zombie_villager::ZombieVillagerEntity>()
                .map(|zombie_villager| zombie_villager.mob_entity.as_ref())
        })
}

impl Mob for ZombieEntityBase {
    fn mob_metadata(&self) -> Option<MobMetadata> {
        self.is_baby().then(|| MobMetadata::new().baby(true))
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn spawn_as_baby(&self) -> bool {
        self.set_baby(true);
        true
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        difficulty: &RegionalDifficulty,
    ) {
        // Default armor slots (super.populateDefaultEquipmentSlots)
        if rand::random::<f32>()
            < MobEntity::MAX_WEARING_ARMOR_CHANCE * difficulty.special_multiplier
        {
            let mut armor_type = rand::random_range(0..3);
            for _ in 1..=3 {
                if rand::random::<f32>() < MobEntity::WEARING_ARMOR_UPGRADE_MATERIAL_CHANCE {
                    armor_type += 1;
                }
            }

            let partial_chance = if difficulty.base_difficulty == Difficulty::Hard {
                0.1f32
            } else {
                0.25f32
            };

            let living = &self.mob_entity.living_entity;
            let mut equipment = living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut first = true;

            for slot in &MobEntity::EQUIPMENT_POPULATION_ORDER {
                let current = equipment.get(slot);
                if !first && rand::random::<f32>() < partial_chance {
                    break;
                }
                first = false;
                if current.is_empty()
                    && let Some(item) = MobEntity::get_equipment_for_slot(slot, armor_type)
                {
                    equipment.put(slot, ItemStack::new(1, item));
                }
            }
        }

        let weapon_chance = if difficulty.base_difficulty == Difficulty::Hard {
            0.05f32
        } else {
            0.01f32
        };
        if rand::random::<f32>() < weapon_chance {
            let r = rand::random_range(0..6);
            let weapon_item = match r {
                0 => &Item::IRON_SWORD,
                1 => &Item::IRON_SPEAR,
                _ => &Item::IRON_SHOVEL,
            };
            let living = &self.mob_entity.living_entity;
            let mut equipment = living
                .entity_equipment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            equipment.put(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, weapon_item));
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        // Vanilla Zombie.addAdditionalSaveData; shared by every zombie-family mob.
        nbt.put_bool("IsBaby", self.is_baby());
        if self.can_break_doors() {
            nbt.put_bool("CanBreakDoors", true);
        }
        if drowning_conversion(self.mob_entity.living_entity.entity.entity_type).is_some() {
            nbt.put_int(
                "DrownedConversionTime",
                self.drowned_conversion_time.load(Ordering::Relaxed),
            );
            nbt.put_int("InWaterTime", self.in_water_time.load(Ordering::Relaxed));
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_baby(nbt.get_bool("IsBaby").unwrap_or(false));
        if let Some(can_break_doors) = nbt.get_bool("CanBreakDoors") {
            self.set_can_break_doors(can_break_doors, self);
        }
        self.in_water_time
            .store(nbt.get_int("InWaterTime").unwrap_or(0), Ordering::Relaxed);
        self.set_drowned_converting(
            nbt.get_int("DrownedConversionTime")
                .unwrap_or(NOT_CONVERTING),
        );
    }
}
