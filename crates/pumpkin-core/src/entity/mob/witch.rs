use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use rand::RngExt;

use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Potion;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        Controls, Goal, active_target::ActiveTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, ranged_attack::RangedAttackGoal, revenge::RevengeGoal,
        swim::SwimGoal, to_goal_ticks, wander_around::WanderAroundGoal,
    },
    mob::{
        Mob, MobEntity, RangedAttackMob,
        patrol::{LongDistancePatrolGoal, PatrolData, PatrollingMonster},
        raider::{
            ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider, RaiderCelebrationGoal,
            RaiderData, RaiderMoveThroughVillageGoal, is_raider,
        },
    },
    projectile::splash_potion::SplashPotionEntity,
};

fn create_potion_stack(item: &'static Item, potion: &'static Potion) -> ItemStack {
    use pumpkin_data::data_component::DataComponent;
    use pumpkin_data::data_component_impl::{DataComponentImpl, PotionContentsImpl};
    let mut stack = ItemStack::new(1, item);
    stack.patch.push((
        DataComponent::PotionContents,
        Some(
            PotionContentsImpl {
                potion_id: Some(i32::from(potion.id)),
                custom_color: None,
                custom_effects: Vec::new(),
                custom_name: None,
            }
            .to_dyn(),
        ),
    ));
    stack
}

/// Represents a Witch, a hostile ranged mob that throws splash potions and drinks restorative potions.
///
/// Wiki: <https://minecraft.wiki/w/Witch>
const WITCH_HUNTS: &[Hunt] =
    &[Hunt::new(3, TargetMatch::Type(&EntityType::PLAYER), true)
        .starts_only_when(witch_can_attack)];

const SPEED_MODIFIER_DRINKING_ID: &str = "minecraft:drinking";
const HEAL_RAIDERS_COOLDOWN: i32 = 200;

// witch: players only while not healing raiders (NearestAttackableWitchTargetGoal.canAttack)
fn witch_can_attack(mob: &dyn Mob) -> bool {
    mob.get_entity()
        .entity_type
        .eq(&EntityType::WITCH)
        .then(|| mob_as_witch(mob))
        .flatten()
        .is_none_or(|witch| witch.heal_cooldown.load(Ordering::Relaxed) <= 0)
}

fn mob_as_witch(mob: &dyn Mob) -> Option<&WitchEntity> {
    let base: &dyn EntityBase = mob;
    base.cast_any().downcast_ref::<WitchEntity>()
}

// witch: heal raiders in a raid, 50% per check, 200 tick cooldown (NearestHealableRaiderTargetGoal)
struct WitchHealRaidersGoal {
    inner: Box<ActiveTargetGoal>,
    witch: Weak<WitchEntity>,
}

impl Goal for WitchHealRaidersGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(witch) = self.witch.upgrade() else {
            return false;
        };
        if witch.heal_cooldown.load(Ordering::Relaxed) > 0
            || !mob.get_random().random::<bool>()
            || !witch.has_active_raid()
        {
            return false;
        }
        self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(witch) = self.witch.upgrade() {
            witch
                .heal_cooldown
                .store(to_goal_ticks(HEAL_RAIDERS_COOLDOWN), Ordering::Relaxed);
        }
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

pub struct WitchEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    drinking_potion: AtomicBool,
    using_time: AtomicI32,
    heal_cooldown: AtomicI32,
}

impl WitchEntity {
    #[must_use]
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let witch = Self {
            mob_entity,
            raider_data: RaiderData::default(),
            drinking_potion: AtomicBool::new(false),
            using_time: AtomicI32::new(0),
            heal_cooldown: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(witch);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let ranged_weak: Weak<dyn RangedAttackMob> = {
            let ranged_arc: Arc<dyn RangedAttackMob> = mob_arc.clone();
            Arc::downgrade(&ranged_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            // witch: raider + patrol goals, then its own (Witch.registerGoals)
            goal_selector.add_goal(4, Box::new(LongDistancePatrolGoal::new(0.7, 0.595)));
            goal_selector.add_goal(1, Box::new(ObtainRaidLeaderBannerGoal));
            goal_selector.add_goal(3, Box::new(PathfindToRaidGoal::default()));
            goal_selector.add_goal(4, Box::new(RaiderMoveThroughVillageGoal::new(1.05)));
            goal_selector.add_goal(5, Box::new(RaiderCelebrationGoal));
            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                2,
                Box::new(RangedAttackGoal::new(ranged_weak, 1.0, 60, 10.0)),
            );
            goal_selector.add_goal(2, Box::new(WanderAroundGoal::water_avoiding(1.0)));
            goal_selector.add_goal(
                3,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(3, Box::new(RandomLookAroundGoal::default()));

            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).ignoring(is_raider)));
            target_selector.add_goal(
                2,
                Box::new(WitchHealRaidersGoal {
                    inner: ActiveTargetGoal::with_default(
                        &mob_arc.mob_entity,
                        TargetMatch::Tag(&tag::EntityType::MINECRAFT_RAIDERS),
                        true,
                    )
                    .every(0)
                    .selecting(|mob, target, _world| {
                        mob.as_raider().is_some_and(Raider::has_active_raid)
                            && target.entity.entity_type != &EntityType::WITCH
                    }),
                    witch: Arc::downgrade(&mob_arc),
                }),
            );
            hunting::apply(target_selector, &mob_arc.mob_entity, WITCH_HUNTS);
        };

        mob_arc
    }

    pub fn set_drinking_potion(&self, drinking: bool) {
        self.drinking_potion.store(drinking, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(tracked_data::witch::DATA_USING_ITEM, drinking);
    }

    #[must_use]
    pub fn is_drinking_potion(&self) -> bool {
        self.drinking_potion.load(Ordering::Relaxed)
    }

    pub fn throw_potion(&self, target: &Arc<dyn EntityBase>) {
        if self.is_drinking_potion() {
            return;
        }

        let entity = &self.mob_entity.living_entity.entity;
        let world = entity.world.load_full();

        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();
        let target_vel = target_entity.velocity.load();
        let witch_pos = entity.pos.load();

        let xd = target_pos.x + target_vel.x - witch_pos.x;
        let yd = target_pos.y + target_entity.get_eye_height() - 1.1 - witch_pos.y;
        let zd = target_pos.z + target_vel.z - witch_pos.z;
        let dist = xd.hypot(zd);

        let mut potion = &Potion::HARMING;

        let target_is_raider = target
            .get_mob()
            .is_some_and(|mob| mob.as_raider().is_some());
        if target_is_raider {
            // witch: heal or regenerate raiders, then drop them as target (Witch.performRangedAttack)
            potion = if target
                .get_living_entity()
                .is_some_and(|living| living.health.load() <= 4.0)
            {
                &Potion::HEALING
            } else {
                &Potion::REGENERATION
            };
            self.mob_entity.set_target(None);
        } else if let Some(target_living) = target.get_living_entity() {
            let r: f32 = rand::random();
            if dist >= 8.0 && !target_living.has_effect(&StatusEffect::SLOWNESS) {
                potion = &Potion::SLOWNESS;
            } else if target_living.health.load() >= 8.0
                && !target_living.has_effect(&StatusEffect::POISON)
            {
                potion = &Potion::POISON;
            } else if dist <= 3.0 && !target_living.has_effect(&StatusEffect::WEAKNESS) && r < 0.25
            {
                potion = &Potion::WEAKNESS;
            }
        }

        let potion_stack = create_potion_stack(&Item::SPLASH_POTION, potion);

        let splash_entity = Entity::new(world.clone(), witch_pos, &EntityType::SPLASH_POTION);
        let splash = SplashPotionEntity::new_shot(splash_entity, entity);
        splash.set_item_stack(potion_stack);

        let speed = if dist <= 2.0 { 0.45 } else { 0.75 };
        let yo = dist * 0.2;

        splash.thrown.set_velocity(xd, yd + yo, zd, speed, 8.0);

        if !entity.silent.load(Ordering::Relaxed) {
            world.play_sound_fine(
                Sound::EntityWitchThrow,
                SoundCategory::Hostile,
                &witch_pos,
                1.0,
                rand::random::<f32>().mul_add(0.4, 0.8),
            );
        }

        let splash_arc: Arc<dyn EntityBase> = Arc::new(splash);
        world.spawn_entity(splash_arc);
    }
}

impl Mob for WitchEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_patrolling_monster(&self) -> Option<&dyn PatrollingMonster> {
        Some(self)
    }

    fn as_raider(&self) -> Option<&dyn Raider> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        self.write_raider_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &pumpkin_nbt::compound::NbtCompound) {
        self.read_raider_nbt(nbt);
    }

    fn pre_damage(&self, _damage_type: DamageType, source: Option<&dyn EntityBase>) -> bool {
        if let Some(src) = source
            && src.get_entity().entity_id == self.mob_entity.living_entity.entity.entity_id
        {
            return false;
        }
        true
    }

    fn modify_incoming_damage(&self, mut amount: f32, damage_type: DamageType) -> f32 {
        // Vanilla `Witch.getDamageAfterMagicAbsorb`.
        if damage_type.has_tag(&tag::DamageType::MINECRAFT_WITCH_RESISTANT_TO) {
            amount *= 0.15;
        }
        amount
    }

    // witch: heal cooldown, drink potions, particles (Witch.aiStep)
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        let living = &self.mob_entity.living_entity;
        if !entity.is_alive() {
            return;
        }
        self.heal_cooldown.fetch_sub(1, Ordering::Relaxed);
        let mut random = rand::rng();

        if self.is_drinking_potion() {
            if self.using_time.fetch_sub(1, Ordering::Relaxed) <= 0 {
                self.set_drinking_potion(false);
                let stack = self
                    .mob_entity
                    .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::EMPTY.clone());
                if stack.item.id == Item::POTION.id {
                    let scale = stack
                        .get_data_component::<pumpkin_data::data_component_impl::PotionDurationScaleImpl>()
                        .map_or(1.0, |component| component.scale);
                    crate::item::potion::PotionContents::apply_effects_to(
                        living,
                        crate::item::potion::PotionContents::read_potion_effects(&stack),
                        scale,
                        crate::item::potion::PotionApplicationSource::Normal,
                        None,
                    );
                }
                entity.world.load().emit_game_event_with_context(
                    pumpkin_data::game_event::GameEvent::Drink.name(),
                    entity.pos.load(),
                    &crate::world::game_event::GameEventContext {
                        source_entity: Some(self),
                    },
                );
                self.set_drinking_speed(false);
            }
        } else {
            let last_fire = living
                .last_damage_type
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some_and(|damage| damage.has_tag(&tag::DamageType::MINECRAFT_IS_FIRE));
            let potion = if random.random::<f32>() < 0.15
                && entity.is_submerged_in_water()
                && !living.has_effect(&StatusEffect::WATER_BREATHING)
            {
                Some(&Potion::WATER_BREATHING)
            } else if random.random::<f32>() < 0.15
                && (entity.is_on_fire() || last_fire)
                && !living.has_effect(&StatusEffect::FIRE_RESISTANCE)
            {
                Some(&Potion::FIRE_RESISTANCE)
            } else if random.random::<f32>() < 0.05
                && living.health.load() < living.get_max_health()
            {
                Some(&Potion::HEALING)
            } else if random.random::<f32>() < 0.5
                && let Some(target) = self.mob_entity.get_target()
                && !living.has_effect(&StatusEffect::SPEED)
                && target
                    .get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&entity.pos.load())
                    > 121.0
            {
                Some(&Potion::SWIFTNESS)
            } else {
                None
            };

            if let Some(potion) = potion {
                let stack = create_potion_stack(&Item::POTION, potion);
                self.mob_entity
                    .set_item_slot(&EquipmentSlot::MAIN_HAND, stack);
                // vanilla potion use duration
                self.using_time.store(32, Ordering::Relaxed);
                self.set_drinking_potion(true);
                if !entity.is_silent() {
                    entity.world.load().play_sound_fine(
                        Sound::EntityWitchDrink,
                        SoundCategory::Hostile,
                        &entity.pos.load(),
                        1.0,
                        random.random::<f32>().mul_add(0.4, 0.8),
                    );
                }
                self.set_drinking_speed(true);
            }
        }

        if random.random::<f32>() < 7.5E-4 {
            entity.world.load().send_entity_status(
                entity,
                pumpkin_data::entity::EntityStatus::WitchHatMagic,
                pumpkin_protocol::bedrock::server::actor_event::ActorEventID::None,
            );
        }
    }
}

impl WitchEntity {
    // witch: -0.25 speed while drinking (Witch.SPEED_MODIFIER_DRINKING)
    fn set_drinking_speed(&self, drinking: bool) {
        let mut attributes = self
            .mob_entity
            .living_entity
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(speed) =
            attributes.get_mut(&pumpkin_data::attributes::Attributes::MOVEMENT_SPEED.id)
        {
            speed.remove_modifier(SPEED_MODIFIER_DRINKING_ID);
            if drinking {
                speed.add_or_replace_modifier(crate::entity::attributes::Modifier {
                    id: SPEED_MODIFIER_DRINKING_ID.to_string(),
                    amount: -0.25,
                    operation: crate::entity::attributes::ModifierOperation::Add,
                });
            }
        }
    }
}

impl RangedAttackMob for WitchEntity {
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, _power: f32) {
        self.throw_potion(target);
    }
}

impl PatrollingMonster for WitchEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }

    fn can_be_leader(&self) -> bool {
        false
    }
}

impl Raider for WitchEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityWitchCelebrate
    }
}
