use std::sync::{
    Arc, Mutex, PoisonError, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::mob::spawn::SpawnReason;
use crate::entity::passive::tamable::is_untamed;
use crate::entity::passive::wandering_trader::WanderingTraderEntity;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        llama_follow_caravan::LlamaFollowCaravanGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, ranged_attack::RangedAttackGoal, revenge::RevengeGoal,
        run_around_like_crazy::RunAroundLikeCrazyGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, RangedAttackMob, spawn::SpawnGroupData},
    passive::{
        abstract_horse::{
            HORSE_SOUND_VOLUME, HorseCore, HorseLike, MAX_HEALTH, MAX_JUMP_STRENGTH,
            MAX_MOVEMENT_SPEED, MIN_HEALTH, MIN_JUMP_STRENGTH, MIN_MOVEMENT_SPEED,
            controlling_passenger_horse, create_offspring_attribute, generate_max_health,
            tick_ridden_horse,
        },
        animal::{Animal, get_carpet_color_from_item},
    },
    player::Player,
    projectile::llama_spit::LlamaSpitEntity,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::HAY_BLOCK];
const MAX_STRENGTH: i32 = 5;
const LLAMA_MAX_TEMPER: i32 = 30;
const REQUIRED_PATH_LENGTH: f32 = 40.0;
const DEFAULT_DESPAWN_DELAY: i32 = 47999;
const LLAMA_SOUND_VOLUME_PITCH_RANGE: f32 = 0.2;
const SPIT_UNCERTAINTY: f32 = 10.0;
const SPIT_VELOCITY: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum LlamaVariant {
    #[default]
    Creamy = 0,
    White = 1,
    Brown = 2,
    Gray = 3,
}

impl LlamaVariant {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::White,
            2 => Self::Brown,
            3 => Self::Gray,
            _ => Self::Creamy,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub fn random_variant() -> Self {
        let mut rng = rand::rng();
        match rng.random_range(0..4) {
            1 => Self::White,
            2 => Self::Brown,
            3 => Self::Gray,
            _ => Self::Creamy,
        }
    }
}

/// Vanilla `Wolf.avoidLlama`: the stronger the llama, the likelier a wolf flees.
pub fn scares_wolves(threat: &dyn EntityBase) -> bool {
    threat
        .cast_any()
        .downcast_ref::<LlamaEntity>()
        .map(LlamaEntity::get_strength)
        .is_some_and(|strength| strength >= rand::rng().random_range(0..5))
}

/// Vanilla `LlamaAttackWolfGoal`: untamed wolves within a quarter of the follow range.
pub const LLAMA_HUNTS: &[Hunt] = &[Hunt::new(2, TargetMatch::Type(&EntityType::WOLF), false)
    .every(16)
    .only_mobs(is_untamed)
    .range_scale(0.25)];

const TRADER_LLAMA_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::ZOMBIE_LIKE, true).matching(|_hunter, target, _world| {
        target.entity.entity_type != &EntityType::ZOMBIFIED_PIGLIN
    }),
    Hunt::new(2, TargetMatch::ILLAGER, true),
];

// llama: LlamaHurtByTargetGoal.canContinueToUse, a llama that just spat lets go of the attacker
fn keeps_hurt_by_target(mob: &dyn Mob) -> bool {
    mob.cast_any()
        .downcast_ref::<LlamaEntity>()
        .is_none_or(|llama| !llama.did_spit.swap(false, Ordering::Relaxed))
}

// trader_llama: TraderLlama.TraderLlamaDefendWanderingTraderGoal
#[derive(Default)]
struct TraderLlamaDefendWanderingTraderGoal {
    timestamp: i32,
}

impl TraderLlamaDefendWanderingTraderGoal {
    fn trader(mob: &dyn Mob) -> Option<Arc<dyn EntityBase>> {
        mob.get_entity()
            .leashed_to
            .lock()
            .ok()
            .and_then(|holder| holder.clone())
            .filter(|holder| holder.get_entity().entity_type == &EntityType::WANDERING_TRADER)
    }
}

impl Goal for TraderLlamaDefendWanderingTraderGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(living) = Self::trader(mob).and_then(|trader| {
            trader.get_living_entity().map(|living| {
                (
                    living.get_last_hurt_by_mob_timestamp(),
                    living.get_last_hurt_by_mob(),
                )
            })
        }) else {
            return false;
        };
        living.0 != self.timestamp
            && living
                .1
                .is_some_and(|attacker| mob.can_attack(attacker.as_ref()))
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        mob.get_mob_entity().get_target().is_some()
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(trader) = Self::trader(mob)
            && let Some(living) = trader.get_living_entity()
        {
            self.timestamp = living.get_last_hurt_by_mob_timestamp();
            mob.get_mob_entity()
                .set_target(living.get_last_hurt_by_mob());
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().set_target(None);
    }

    fn controls(&self) -> Controls {
        Controls::TARGET
    }
}

/// Llama and trader llama, vanilla `Llama` and `TraderLlama` over `AbstractChestedHorse`.
pub struct LlamaEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub strength: AtomicI32,
    pub carpet_color: AtomicCell<Option<u8>>,
    pub flags: AtomicU8,
    pub has_chest: AtomicBool,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub chest_inventory: Arc<SimpleInventory>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
    pub despawn_delay: AtomicI32,
    did_spit: AtomicBool,
    // llama: caravanHead is the llama in front, caravanTail the one behind
    caravan_head: Mutex<Option<Weak<dyn EntityBase>>>,
    caravan_tail: Mutex<Option<Weak<dyn EntityBase>>>,
    core: HorseCore,
}

impl LlamaEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .set_required_path_length(REQUIRED_PATH_LENGTH);
        // llama: getLeashOffset, (0, 0.75 * eyeHeight, width * 0.5)
        mob_entity
            .living_entity
            .entity
            .leash_offset_factors
            .store(Some((0.75, 0.5)));
        let is_trader = mob_entity.living_entity.entity.entity_type == &EntityType::TRADER_LLAMA;

        let llama = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(LlamaVariant::Creamy.id()),
            strength: AtomicI32::new(0),
            carpet_color: AtomicCell::new(None),
            flags: AtomicU8::new(0),
            has_chest: AtomicBool::new(false),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            chest_inventory: Arc::new(SimpleInventory::new(15)),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
            armor_inventory: Arc::new(SimpleInventory::new(1)),
            despawn_delay: AtomicI32::new(DEFAULT_DESPAWN_DELAY),
            did_spit: AtomicBool::new(false),
            caravan_head: Mutex::new(None),
            caravan_tail: Mutex::new(None),
            core: HorseCore::default(),
        };
        let mob_arc = Arc::new(llama);
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

            // llama: Llama.registerGoals
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(RunAroundLikeCrazyGoal::new(1.2)));
            goal_selector.add_goal(2, Box::new(LlamaFollowCaravanGoal::new(2.1)));
            goal_selector.add_goal(
                3,
                Box::new(RangedAttackGoal::new(ranged_weak, 1.25, 40, 20.0)),
            );
            goal_selector.add_goal(3, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(4, BreedGoal::new(1.0));
            goal_selector.add_goal(5, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(6, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::water_avoiding(0.7)));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(9, Box::new(RandomLookAroundGoal::default()));
            if is_trader {
                // trader_llama: TraderLlama.registerGoals
                goal_selector.add_goal(1, EscapeDangerGoal::new(2.0));
                target_selector
                    .add_goal(1, Box::new(TraderLlamaDefendWanderingTraderGoal::default()));
                hunting::apply(target_selector, &mob_arc.mob_entity, TRADER_LLAMA_HUNTS);
            } else {
                target_selector.add_goal(
                    1,
                    Box::new(RevengeGoal::new(true).continuing_while(keeps_hurt_by_target)),
                );
                hunting::apply(target_selector, &mob_arc.mob_entity, LLAMA_HUNTS);
            }
        };

        mob_arc
    }

    #[must_use]
    pub fn is_trader_llama(&self) -> bool {
        self.get_entity().entity_type == &EntityType::TRADER_LLAMA
    }

    #[must_use]
    pub fn get_variant(&self) -> LlamaVariant {
        LlamaVariant::from_id(self.variant.load(Ordering::Relaxed))
    }

    pub fn set_variant(&self, variant: LlamaVariant) {
        self.variant.store(variant.id(), Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_VARIANT_ID,
            VarInt(variant.id()),
        );
    }

    #[must_use]
    pub fn get_strength(&self) -> i32 {
        self.strength.load(Ordering::Relaxed)
    }

    // llama: setStrength
    pub fn set_strength(&self, strength: i32) {
        let clamped = strength.clamp(1, MAX_STRENGTH);
        self.strength.store(clamped, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_STRENGTH_ID,
            VarInt(clamped),
        );
    }

    // llama: setRandomStrength
    fn set_random_strength(&self) {
        let mut rng = self.get_random();
        let max_strength = if rng.random::<f32>() < 0.04 { 5 } else { 3 };
        self.set_strength(1 + rng.random_range(0..max_strength));
    }

    #[must_use]
    pub fn has_chest(&self) -> bool {
        self.has_chest.load(Ordering::Relaxed)
    }

    pub fn set_has_chest(&self, has_chest: bool) {
        self.has_chest.store(has_chest, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(pumpkin_data::tracked_data::llama::DATA_ID_CHEST, has_chest);
    }

    #[must_use]
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flag(flag)
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        self.put_flag(flag, val);
    }

    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.is_horse_tame()
    }

    // llama: getInventoryColumns
    fn inventory_columns(&self) -> usize {
        if self.has_chest() {
            self.get_strength().clamp(1, MAX_STRENGTH) as usize
        } else {
            0
        }
    }

    // llama: AbstractChestedHorse.randomizeAttributes
    fn randomize_attributes(&self) {
        let max_health = generate_max_health(&mut self.get_random());
        self.mob_entity.living_entity.set_max_health(max_health);
        self.mob_entity.living_entity.health.store(max_health);
    }

    // llama: spit, the spit flies with an uncertainty of 10
    pub fn spit(&self, target: &Arc<dyn EntityBase>) {
        let entity = self.get_entity();
        let world = entity.world.load_full();

        let spit_entity = Entity::new(world.clone(), entity.pos.load(), &EntityType::LLAMA_SPIT);
        let spit = LlamaSpitEntity::new_shot(spit_entity, entity);

        let mob_pos = entity.pos.load();
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();
        let target_height = f64::from(target_entity.entity_dimension.load().height);

        let dx = target_pos.x - mob_pos.x;
        let dy = (target_pos.y + target_height / 3.0) - spit.get_entity().pos.load().y;
        let dz = target_pos.z - mob_pos.z;
        let yo = dx.hypot(dz) * f64::from(0.2f32);

        spit.thrown.set_velocity(
            dx,
            dy + yo,
            dz,
            f64::from(SPIT_VELOCITY),
            f64::from(SPIT_UNCERTAINTY),
        );

        if !entity.silent.load(Ordering::Relaxed) {
            let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
                .mul_add(LLAMA_SOUND_VOLUME_PITCH_RANGE, 1.0);
            world.play_sound_fine(
                Sound::EntityLlamaSpit,
                SoundCategory::Neutral,
                &mob_pos,
                1.0,
                pitch,
            );
        }

        let spit_arc: Arc<dyn EntityBase> = Arc::new(spit);
        world.spawn_entity(spit_arc);
        self.did_spit.store(true, Ordering::Relaxed);
    }

    // llama: caravan, head is the llama in front
    fn caravan_link(slot: &Mutex<Option<Weak<dyn EntityBase>>>) -> Option<Arc<dyn EntityBase>> {
        slot.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }

    #[must_use]
    pub fn caravan_head(&self) -> Option<Arc<dyn EntityBase>> {
        Self::caravan_link(&self.caravan_head)
    }

    #[must_use]
    pub fn in_caravan(&self) -> bool {
        self.caravan_head().is_some()
    }

    #[must_use]
    pub fn has_caravan_tail(&self) -> bool {
        Self::caravan_link(&self.caravan_tail).is_some()
    }

    #[must_use]
    pub fn is_leashed(&self) -> bool {
        self.get_entity()
            .leashed_to
            .lock()
            .is_ok_and(|holder| holder.is_some())
    }

    // llama: joinCaravan(tail), this llama follows the given one
    pub fn join_caravan(&self, head: &Arc<dyn EntityBase>) {
        *self
            .caravan_head
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(head));
        if let Some(head_llama) = head.cast_any().downcast_ref::<Self>()
            && let Some(own) = self
                .get_entity()
                .world
                .load()
                .get_entity_by_id(self.get_entity().entity_id)
        {
            *head_llama
                .caravan_tail
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(&own));
        }
    }

    // llama: leaveCaravan
    pub fn leave_caravan(&self) {
        if let Some(head) = self.caravan_head()
            && let Some(head_llama) = head.cast_any().downcast_ref::<Self>()
        {
            *head_llama
                .caravan_tail
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
        *self
            .caravan_head
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    // trader_llama: TraderLlama.maybeDespawn
    fn maybe_despawn(&self) {
        let entity = self.get_entity();
        let leashed_to = entity
            .leashed_to
            .lock()
            .ok()
            .and_then(|holder| holder.clone());
        let to_trader = leashed_to
            .as_ref()
            .filter(|holder| holder.get_entity().entity_type == &EntityType::WANDERING_TRADER);
        let leashed_to_other = leashed_to.is_some() && to_trader.is_none();
        let exactly_one_player = entity
            .passengers
            .lock()
            .is_ok_and(|passengers| passengers.len() == 1 && passengers[0].get_player().is_some());
        if self.is_tame()
            || leashed_to_other
            || exactly_one_player
            || self.is_age_locked()
            || self.mob_entity.persistence_required.load(Ordering::Relaxed)
        {
            return;
        }
        let delay = to_trader
            .and_then(|trader| {
                trader
                    .cast_any()
                    .downcast_ref::<WanderingTraderEntity>()
                    .map(WanderingTraderEntity::get_despawn_delay)
            })
            .unwrap_or_else(|| self.despawn_delay.load(Ordering::Relaxed))
            - 1;
        self.despawn_delay.store(delay, Ordering::Relaxed);
        if delay <= 0 {
            entity.remove_leash();
            entity.remove();
        }
    }
}

impl HorseLike for LlamaEntity {
    fn core(&self) -> &HorseCore {
        &self.core
    }

    fn horse_flags(&self) -> &AtomicU8 {
        &self.flags
    }

    fn horse_temper(&self) -> &AtomicI32 {
        &self.temper
    }

    fn horse_owner(&self) -> &AtomicCell<Option<Uuid>> {
        &self.owner
    }

    fn store_flags(&self, flags: u8) {
        self.flags.store(flags, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_ID_FLAGS,
            flags as i8,
        );
    }

    fn horse_saddled(&self) -> bool {
        false
    }

    fn set_horse_saddle(&self, _saddled: bool) {}

    fn horse_eat_sound(&self) -> Sound {
        Sound::EntityLlamaEat
    }

    fn angry_sound(&self) -> Sound {
        Sound::EntityLlamaAngry
    }

    fn jump_sound(&self) -> Sound {
        Sound::EntityHorseJump
    }

    // llama: canPerformRearing, canEatGrass
    fn can_perform_rearing(&self) -> bool {
        false
    }

    fn can_eat_grass(&self) -> bool {
        false
    }

    fn can_gallop(&self) -> bool {
        false
    }

    // llama: getMaxTemper
    fn horse_max_temper(&self) -> i32 {
        LLAMA_MAX_TEMPER
    }

    // llama: isImmobile
    fn horse_immobile(&self) -> bool {
        self.mob_entity.living_entity.is_dead_or_dying() || self.is_horse_eating()
    }

    // llama: causeFallDamage plays no landing sound
    fn horse_landed(&self, _fall_distance: f32) {}

    // llama: followMommy only outside a caravan
    fn follow_mommy(&self) {
        if !self.in_caravan() && self.is_baby() {
            self.follow_mommy_default();
        }
    }

    // llama: Llama.handleEating, wheat and hay blocks
    fn horse_handle_eating(&self, player: &Player, stack: &ItemStack) -> bool {
        let item = stack.get_item();
        let (mut age_up, mut temper, mut heal, mut item_used) = (0, 0, 0.0f32, false);
        if item == &Item::WHEAT {
            (age_up, temper, heal) = (10, 3, 2.0);
        } else if item == &Item::HAY_BLOCK {
            (age_up, temper, heal) = (90, 6, 10.0);
            if self.is_horse_tame() && self.get_age() == 0 && self.can_fall_in_love() {
                item_used = true;
                self.mob_entity
                    .set_love_ticks(600, Some(player.gameprofile.id));
            }
        }
        let living = &self.mob_entity.living_entity;
        if living.health.load() < living.get_max_health() && heal > 0.0 {
            living.heal(heal);
            item_used = true;
        }
        if self.is_baby() && age_up > 0 && !self.is_age_locked() {
            self.age_up(age_up, false);
            item_used = true;
        }
        if temper > 0
            && (item_used || !self.is_horse_tame())
            && self.temper.load(Ordering::Relaxed) < LLAMA_MAX_TEMPER
        {
            self.horse_modify_temper(temper);
            item_used = true;
        }
        if item_used && !self.get_entity().is_silent() {
            let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
                .mul_add(0.2, 1.0);
            self.play_entity_sound(Sound::EntityLlamaEat, 1.0, pitch);
        }
        item_used
    }

    // trader_llama: doPlayerRide, not while leashed to a wandering trader
    fn do_player_ride(&self, player: &Arc<Player>) -> bool {
        let to_trader = self
            .get_entity()
            .leashed_to
            .lock()
            .ok()
            .and_then(|holder| holder.clone())
            .is_some_and(|holder| holder.get_entity().entity_type == &EntityType::WANDERING_TRADER);
        if to_trader {
            return true;
        }
        self.do_player_ride_default(player)
    }
}

impl AgeableMob for LlamaEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for LlamaEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_LLAMA_FOOD)
    }

    // llama: Llama.getBreedOffspring, offspring attributes, strength and coat from the parents
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let mut rng = self.get_random();
        let baby_base = from_type(
            self.get_entity().entity_type,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        if let Some(baby) = baby_base.cast_any().downcast_ref::<Self>() {
            if baby.is_trader_llama() {
                baby.mob_entity
                    .persistence_required
                    .store(true, Ordering::Relaxed);
            }
            let own = &self.mob_entity.living_entity;
            let other = partner
                .get_mob()
                .map(|mob| &mob.get_mob_entity().living_entity);
            let parent_values = |attribute: &'static pumpkin_data::attributes::Attributes| {
                (
                    own.get_attribute_base(attribute),
                    other.map_or_else(
                        || own.get_attribute_base(attribute),
                        |living| living.get_attribute_base(attribute),
                    ),
                )
            };
            let living = &baby.mob_entity.living_entity;
            let (a, b) = parent_values(&pumpkin_data::attributes::Attributes::MAX_HEALTH);
            let health = create_offspring_attribute(a, b, MIN_HEALTH, MAX_HEALTH, &mut rng);
            living.set_max_health(health as f32);
            living.health.store(health as f32);
            let (a, b) = parent_values(&pumpkin_data::attributes::Attributes::JUMP_STRENGTH);
            living.set_attribute_base(
                &pumpkin_data::attributes::Attributes::JUMP_STRENGTH,
                create_offspring_attribute(a, b, MIN_JUMP_STRENGTH, MAX_JUMP_STRENGTH, &mut rng),
            );
            let (a, b) = parent_values(&pumpkin_data::attributes::Attributes::MOVEMENT_SPEED);
            living.set_attribute_base(
                &pumpkin_data::attributes::Attributes::MOVEMENT_SPEED,
                create_offspring_attribute(a, b, MIN_MOVEMENT_SPEED, MAX_MOVEMENT_SPEED, &mut rng),
            );

            let other_llama = partner.cast_any().downcast_ref::<Self>();
            let other_strength = other_llama.map_or(self.get_strength(), Self::get_strength);
            let mut baby_strength =
                rng.random_range(0..self.get_strength().max(other_strength).max(1)) + 1;
            if rng.random::<f32>() < 0.03 {
                baby_strength += 1;
            }
            baby.set_strength(baby_strength);
            baby.set_variant(match other_llama {
                Some(other) if !rng.random::<bool>() => other.get_variant(),
                _ => self.get_variant(),
            });
        }
        baby_base
    }
}

impl CustomSound for LlamaEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaHurt)
    }

    fn sound_volume(&self) -> f32 {
        HORSE_SOUND_VOLUME
    }
}

impl Mob for LlamaEntity {
    crate::horse_mob_hooks!();

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaAmbient)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityLlamaStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.horse_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
    }

    // llama: AbstractChestedHorse.openCustomInventoryScreen, strength columns
    fn open_custom_inventory(&self, player: &Arc<Player>) {
        let entity = self.get_entity();
        if (!entity.has_passengers() || entity.has_passenger(player.entity_id())) && self.is_tamed()
        {
            let columns = self.inventory_columns();
            let handler = Arc::new(Mutex::new(MountScreenHandler::new(
                columns as u8,
                &player.inventory,
                self.chest_inventory.clone(),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                columns,
            )));
            player.open_mount_screen(handler, columns, entity.entity_id);
        }
    }

    // llama: AbstractChestedHorse.dropEquipment, the chest and what it holds
    fn drop_inventory(&self) {
        let entity = self.get_entity();
        for slot in 0..self.chest_inventory.size() {
            entity.spawn_at_location(self.chest_inventory.remove_stack(slot));
        }
        if self.has_chest() {
            entity.spawn_at_location(ItemStack::new(1, &Item::CHEST));
            self.set_has_chest(false);
        }
    }

    // llama: Llama.finalizeSpawn, TraderLlama.finalizeSpawn
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_random_strength();
        self.set_variant(LlamaVariant::random_variant());
        self.randomize_attributes();
        if self.is_trader_llama() && self.mob_entity.spawn_reason() == SpawnReason::Event {
            self.set_age(0);
        }
        self.finalize_spawn_super(group_data)
    }

    // llama: Llama.canMate
    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        partner.get_entity().entity_id != self.get_entity().entity_id
            && partner
                .cast_any()
                .downcast_ref::<Self>()
                .is_some_and(|other| self.can_parent() && other.can_parent())
    }

    // trader_llama: aiStep despawn
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.horse_tick();
        if self.is_trader_llama() {
            self.maybe_despawn();
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        self.write_horse_nbt(nbt);
        nbt.put_int("Variant", self.get_variant().id());
        nbt.put_int("Strength", self.get_strength());
        nbt.put_bool("ChestedHorse", self.has_chest());
        if self.has_chest() {
            self.chest_inventory.write_inventory_nbt(nbt, false);
        }
        if self.is_trader_llama() {
            nbt.put_int("DespawnDelay", self.despawn_delay.load(Ordering::Relaxed));
        }
        if let Some(color) = self.carpet_color.load() {
            nbt.put_byte("DecorColor", color as i8);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.set_strength(nbt.get_int("Strength").unwrap_or(0));
        self.read_horse_nbt(nbt);
        self.set_variant(LlamaVariant::from_id(nbt.get_int("Variant").unwrap_or(0)));
        let chested = nbt.get_bool("ChestedHorse").unwrap_or(false);
        self.set_has_chest(chested);
        let mut stacks = vec![ItemStack::EMPTY.clone(); self.chest_inventory.size()];
        if chested {
            self.chest_inventory.read_data(nbt, &mut stacks);
        }
        for (slot, stack) in stacks.into_iter().enumerate() {
            self.chest_inventory.set_stack(slot, stack);
        }
        self.despawn_delay.store(
            nbt.get_int("DespawnDelay").unwrap_or(DEFAULT_DESPAWN_DELAY),
            Ordering::Relaxed,
        );
        if let Some(color) = nbt.get_byte("DecorColor") {
            self.carpet_color.store(Some(color as u8));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::llama::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_VARIANT_ID,
            VarInt(self.get_variant().id()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_STRENGTH_ID,
            VarInt(self.get_strength()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_ID_CHEST,
            self.has_chest(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    // llama: AbstractChestedHorse.mobInteract with the carpet of AbstractHorse.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let sneaking = player.living_entity.entity.is_sneaking();
        let should_open_inventory = !self.is_baby() && self.is_horse_tame() && sneaking;
        let holds_golden_dandelion = player.inventory().held_item().get_item()
            == &Item::GOLDEN_DANDELION
            || player.inventory().off_hand_item().get_item() == &Item::GOLDEN_DANDELION;
        if !self.get_entity().has_passengers()
            && !should_open_inventory
            && (!self.is_baby() || !holds_golden_dandelion)
            && !item_stack.is_empty()
        {
            if self.is_food(item_stack) {
                return self.horse_fed_food(player, item_stack);
            }
            if !self.is_horse_tame() {
                self.horse_make_mad();
                return true;
            }
            if !self.has_chest() && item_stack.get_item() == &Item::CHEST {
                self.set_has_chest(true);
                let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
                    .mul_add(0.2, 1.0);
                self.play_entity_sound(Sound::EntityLlamaChest, 1.0, pitch);
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                return true;
            }
        }
        self.abstract_horse_interact(player, item_stack, |llama, player, stack| {
            // AbstractHorse.equipBodyArmor, the carpet
            let Some(color) = get_carpet_color_from_item(stack.get_item()) else {
                return false;
            };
            if llama.carpet_color.load().is_some() {
                return false;
            }
            llama.carpet_color.store(Some(color));
            let carpet = ItemStack::new(1, stack.get_item());
            llama.armor_inventory.set_stack(0, carpet.clone());
            llama
                .mob_entity
                .set_item_slot_and_drop_when_killed(&EquipmentSlot::BODY, carpet);
            stack.decrement_unless_creative(player.gamemode.load(), 1);
            llama.play_entity_sound(Sound::EntityLlamaSwag, 1.0, 1.0);
            true
        })
    }
}

impl RangedAttackMob for LlamaEntity {
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, _power: f32) {
        self.spit(target);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;
    use pumpkin_util::math::vector3::Vector3;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn llama_in(world: &Arc<World>, entity_type: &'static EntityType) -> Arc<LlamaEntity> {
        let llama = LlamaEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            entity_type,
        ));
        world.entities.rcu(|list| {
            let mut list = (**list).clone();
            list.push(llama.clone() as Arc<dyn EntityBase>);
            Arc::new(list)
        });
        llama
    }

    #[tokio::test]
    async fn a_llama_tops_out_at_temper_thirty_and_a_caravan_links_both_ways() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let head = llama_in(&world, &EntityType::LLAMA);
        let tail = llama_in(&world, &EntityType::TRADER_LLAMA);
        head.horse_modify_temper(100);
        assert_eq!(head.get_temper(), 30);
        assert!(tail.is_trader_llama());

        let head_dyn: Arc<dyn EntityBase> = head.clone();
        tail.join_caravan(&head_dyn);
        assert!(tail.in_caravan());
        assert!(head.has_caravan_tail());
        tail.leave_caravan();
        assert!(!tail.in_caravan());
        assert!(!head.has_caravan_tail());
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_llama_never_rears_and_its_chest_columns_follow_its_strength() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let llama = llama_in(&world, &EntityType::LLAMA);
        llama.set_strength(4);
        assert_eq!(llama.inventory_columns(), 0);
        llama.set_has_chest(true);
        assert_eq!(llama.inventory_columns(), 4);
        llama.make_mad();
        assert!(!llama.is_horse_standing());
        world.level.shutdown().await;
        Ok(())
    }
}
