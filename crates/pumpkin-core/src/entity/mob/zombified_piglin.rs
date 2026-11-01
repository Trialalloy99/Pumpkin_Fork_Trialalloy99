use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;

use crate::entity::ai::util::goal_utils;
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::{
    Entity, EntityBase,
    ai::behavior::neutral::apply_targets,
    ai::goal::{
        break_door::BreakDoorGoal, destroy_egg::DestroyEggGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, revenge::RevengeGoal, spear_use::SpearUseGoal,
        wander_around::WanderAroundGoal,
        zombie_attack::ZombieAttackGoal,
    },
    mob::{
        Mob, MobEntity,
        equipment::RegionalDifficulty,
        neutral::{NeutralData, NeutralMob},
        spawn::SpawnGroupData,
        spawn::SpawnReason,
        zombie::{
            apply_baby_speed, handle_zombie_attributes, maybe_wear_halloween_head,
            roll_chicken_jockey, zombie_baby_dimensions,
        },
    },
};
use crate::plugin::api::events::entity::entity_target::TargetReason;
use crate::plugin::api::events::entity::pig_zombie_anger::PigZombieAngerEvent;
use crate::world::World;

/// Vanilla `ALERT_INTERVAL`: 4 to 6 seconds.
const ALERT_INTERVAL: std::ops::RangeInclusive<i32> = 80..=120;
const FIRST_ANGER_SOUND_DELAY: std::ops::RangeInclusive<i32> = 0..=20;
const SPEED_MODIFIER_ATTACKING_ID: &str = "minecraft:attacking";

pub struct ZombifiedPiglinEntity {
    pub mob_entity: MobEntity,
    neutral_data: NeutralData,
    ticks_until_next_alert: AtomicI32,
    first_anger_sound_in: AtomicI32,
    /// Detects the target transition that restarts the alert interval.
    had_target: AtomicBool,
    can_break_doors: AtomicBool,
}

impl ZombifiedPiglinEntity {
    pub const XP_REWARD: u32 = 5;

    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let piglin = Self {
            mob_entity,
            neutral_data: NeutralData::default(),
            ticks_until_next_alert: AtomicI32::new(0),
            first_anger_sound_in: AtomicI32::new(0),
            had_target: AtomicBool::new(false),
            can_break_doors: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(piglin);
        // zombified piglin: lava path malus 8 (ZombifiedPiglin constructor)
        mob_arc
            .mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_pathfinding_malus(PathType::Lava, 8.0);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(4, DestroyEggGoal::new(1.0, 3));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(1, SpearUseGoal::new(1.0, 1.0, 10.0, 2.0));
            goal_selector.add_goal(2, ZombieAttackGoal::new(1.0, false));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::water_avoiding(1.0)));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).alerting_others()));
            apply_targets(target_selector, &mob_arc.mob_entity, 2, 3, true);
        };

        mob_arc
    }

    /// Hands the current target to nearby piglins that have none of their own.
    fn maybe_alert_others(&self, target: &Arc<dyn EntityBase>) {
        let remaining = self.ticks_until_next_alert.load(Ordering::Relaxed);
        if remaining > 0 {
            self.ticks_until_next_alert
                .store(remaining - 1, Ordering::Relaxed);
            return;
        }

        if self.has_line_of_sight(target.get_entity()) {
            for other in goal_utils::nearby_same_type(self) {
                let Some(other_mob) = other.get_mob() else {
                    continue;
                };
                if other_mob.get_mob_entity().get_target().is_some()
                    || other.is_allied_to(target.as_ref())
                {
                    continue;
                }
                other_mob.set_mob_target(Some(target.clone()), TargetReason::PigZombieTarget);
            }
        }

        self.ticks_until_next_alert
            .store(rand::random_range(ALERT_INTERVAL), Ordering::Relaxed);
    }
}

impl ZombifiedPiglinEntity {
    // zombified piglin: inherited door breaking (Zombie.setCanBreakDoors)
    fn set_can_break_doors(&self, can_break_doors: bool) {
        if self
            .can_break_doors
            .swap(can_break_doors, Ordering::Relaxed)
            == can_break_doors
        {
            return;
        }
        self.mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_open_doors(can_break_doors);
        if can_break_doors {
            self.mob_entity
                .goals_selector
                .add_goal(1, Box::new(BreakDoorGoal::default()));
        } else {
            self.mob_entity.goals_selector.remove_goal::<BreakDoorGoal>(self);
        }
    }
}

impl NeutralMob for ZombifiedPiglinEntity {
    fn get_neutral_data(&self) -> &NeutralData {
        &self.neutral_data
    }

    fn approve_grudge(&self, target: &dyn EntityBase, ticks: i32) -> Option<i32> {
        let entity = &self.mob_entity.living_entity.entity;
        let Some(server) = entity.world.load().server.upgrade() else {
            return Some(ticks);
        };
        let mut event =
            PigZombieAngerEvent::new(entity.entity_id, Some(target.get_entity().entity_id), ticks);
        server.plugin_manager.fire_blocking(&server, &mut event);
        (!event.cancelled).then_some(event.new_anger)
    }

    fn updates_anger_in_ai_step(&self) -> bool {
        true
    }
}

impl Mob for ZombifiedPiglinEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityZombieStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn spawn_as_baby(&self) -> bool {
        self.mob_entity.set_baby_by_age();
        apply_baby_speed(&self.mob_entity, true);
        self.get_entity()
            .set_default_dimensions(zombie_baby_dimensions(&EntityType::ZOMBIFIED_PIGLIN));
        true
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_angry() {
            Sound::EntityZombifiedPiglinAngry
        } else {
            Sound::EntityZombifiedPiglinAmbient
        })
    }

    fn get_base_experience_reward(&self) -> u32 {
        let base = self.get_entity().entity_type.experience_reward;
        if self.mob_entity.living_entity.is_baby() {
            (f64::from(base) * 2.5) as u32
        } else {
            base
        }
    }

    // zombified piglin: zombie spawn rolls, no reinforcements (ZombifiedPiglin via Zombie.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.finalize_spawn_super(group_data);
        let difficulty = RegionalDifficulty::at(world, self.get_entity().pos.load());
        let multiplier = difficulty.special_multiplier;
        let mut random = rand::rng();
        let conversion = self.mob_entity.spawn_reason() == SpawnReason::Conversion;
        if !conversion {
            self.mob_entity
                .set_can_pick_up_loot(random.random::<f32>() < 0.55 * multiplier);
        }
        let group_data = group_data.unwrap_or(SpawnGroupData::Zombie {
            baby: random.random::<f32>() < 0.05,
            can_spawn_jockey: true,
            tried_camel_husk: false,
        });
        if let SpawnGroupData::Zombie {
            baby,
            can_spawn_jockey,
            ..
        } = group_data
        {
            if baby {
                self.spawn_as_baby();
                if can_spawn_jockey {
                    roll_chicken_jockey(&self.mob_entity, world, &mut random);
                }
            }
            self.set_can_break_doors(random.random::<f32>() < multiplier * 0.1);
            if !conversion {
                self.populate_default_equipment_slots(world, &difficulty);
                self.populate_default_equipment_enchantments(&difficulty);
            }
        }
        maybe_wear_halloween_head(&self.mob_entity);
        if handle_zombie_attributes(&self.mob_entity, multiplier, conversion, false) {
            self.set_can_break_doors(true);
        }
        Some(group_data)
    }

    // zombified piglin: zombie save data (Zombie.addAdditionalSaveData)
    fn mob_write_nbt(&self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        nbt.put_bool("IsBaby", self.mob_entity.living_entity.is_baby());
        if self.can_break_doors.load(Ordering::Relaxed) {
            nbt.put_bool("CanBreakDoors", true);
        }
    }

    fn mob_read_nbt(&self, nbt: &pumpkin_nbt::compound::NbtCompound) {
        if nbt.get_bool("IsBaby").unwrap_or(false) && !self.mob_entity.living_entity.is_baby() {
            self.spawn_as_baby();
        }
        if let Some(can_break_doors) = nbt.get_bool("CanBreakDoors") {
            self.set_can_break_doors(can_break_doors);
        }
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        let living = &self.mob_entity.living_entity;
        let mut equipment = living
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let weapon = if rand::random_range(0..20) == 0 {
            &Item::GOLDEN_SPEAR
        } else {
            &Item::GOLDEN_SWORD
        };
        equipment.put(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, weapon));
    }

    // zombified piglin: angry speed + first anger sound (ZombifiedPiglin.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let angry = self.is_angry();
        {
            let mut attributes = self
                .mob_entity
                .living_entity
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
                let has = speed
                    .modifiers
                    .iter()
                    .any(|modifier| modifier.id == SPEED_MODIFIER_ATTACKING_ID);
                if angry && !has && !self.mob_entity.living_entity.is_baby() {
                    speed.add_or_replace_modifier(Modifier {
                        id: SPEED_MODIFIER_ATTACKING_ID.to_string(),
                        amount: 0.05,
                        operation: ModifierOperation::Add,
                    });
                } else if !angry && has {
                    speed.remove_modifier(SPEED_MODIFIER_ATTACKING_ID);
                }
            }
        }
        if angry
            && self.first_anger_sound_in.load(Ordering::Relaxed) > 0
            && self.first_anger_sound_in.fetch_sub(1, Ordering::Relaxed) == 1
        {
            let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2
                + if self.mob_entity.living_entity.is_baby() {
                    1.5
                } else {
                    1.0
                };
            self.play_entity_sound(Sound::EntityZombifiedPiglinAngry, 2.0, pitch * 1.8);
        }
        self.update_persistent_anger();
        let entity = &self.mob_entity.living_entity.entity;
        if !entity.is_alive() {
            return;
        }

        let Some(target) = self.mob_entity.get_target() else {
            self.had_target.store(false, Ordering::Relaxed);
            return;
        };

        // Fresh target: wait out a full interval before spreading the word.
        if !self.had_target.swap(true, Ordering::Relaxed) {
            self.ticks_until_next_alert
                .store(rand::random_range(ALERT_INTERVAL), Ordering::Relaxed);
            self.first_anger_sound_in.store(
                rand::random_range(FIRST_ANGER_SOUND_DELAY),
                Ordering::Relaxed,
            );
        }

        self.maybe_alert_others(&target);
    }
}
