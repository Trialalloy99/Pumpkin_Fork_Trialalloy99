use std::sync::{
    Arc,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::block_state::SoundType;
use pumpkin_data::entity_status::EntityStatus;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::{Block, BlockState};
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::{
    EntityBase,
    mob::Mob,
    player::{Player, advancement::trigger::AdvancementTrigger},
};

pub const FLAG_TAME: u8 = 2;
pub const FLAG_SADDLE: u8 = 4;
pub const FLAG_BRED: u8 = 8;
pub const FLAG_EATING: u8 = 16;
pub const FLAG_STANDING: u8 = 32;
pub const FLAG_OPEN_MOUTH: u8 = 64;

// abstract_horse: generate random max health (AbstractHorse.generateMaxHealth)
pub fn generate_max_health(rng: &mut impl rand::Rng) -> f32 {
    15.0 + rng.random_range(0..8) as f32 + rng.random_range(0..9) as f32
}

// abstract_horse: generate random jump strength (AbstractHorse.generateJumpStrength)
pub fn generate_jump_strength(rng: &mut impl rand::Rng) -> f64 {
    0.4 + rng.random::<f64>() * 0.2 + rng.random::<f64>() * 0.2 + rng.random::<f64>() * 0.2
}

// abstract_horse: generate random speed (AbstractHorse.generateSpeed)
pub fn generate_speed(rng: &mut impl rand::Rng) -> f64 {
    (0.45 + rng.random::<f64>() * 0.3 + rng.random::<f64>() * 0.3 + rng.random::<f64>() * 0.3)
        * 0.25
}

// abstract_horse: MIN and MAX of the offspring ranges (AbstractHorse.MIN_HEALTH .. MAX_MOVEMENT_SPEED)
pub const MIN_HEALTH: f64 = 15.0;
pub const MAX_HEALTH: f64 = 30.0;
pub const MIN_JUMP_STRENGTH: f64 = 0.4f32 as f64;
pub const MAX_JUMP_STRENGTH: f64 = 0.4f32 as f64 + 0.6;
pub const MIN_MOVEMENT_SPEED: f64 = 0.45f32 as f64 * 0.25;
pub const MAX_MOVEMENT_SPEED: f64 = (0.45f32 as f64 + 0.9) * 0.25;

// abstract_horse: AbstractHorse.createOffspringAttribute, the foal lands around the parents' average
#[must_use]
pub fn create_offspring_attribute(
    parent_a: f64,
    parent_b: f64,
    range_min: f64,
    range_max: f64,
    rng: &mut impl rand::Rng,
) -> f64 {
    let a = parent_a.clamp(range_min, range_max);
    let b = parent_b.clamp(range_min, range_max);
    let margin = 0.15 * (range_max - range_min);
    let range = (a - b).abs() + margin * 2.0;
    let average = f64::midpoint(a, b);
    let baby_quality =
        (rng.random::<f64>() + rng.random::<f64>() + rng.random::<f64>()) / 3.0 - 0.5;
    let new_value = average + range * baby_quality;
    if new_value > range_max {
        range_max - (new_value - range_max)
    } else if new_value < range_min {
        range_min + (range_min - new_value)
    } else {
        new_value
    }
}

// abstract_horse: calculate offspring health, jump and speed (AbstractHorse.setOffspringAttributes)
pub fn calculate_offspring_attributes(
    p1_health: f32,
    p2_health: f32,
    p1_jump: f64,
    p2_jump: f64,
    p1_speed: f64,
    p2_speed: f64,
    rng: &mut impl rand::Rng,
) -> (f32, f64, f64) {
    let health = (p1_health + p2_health + generate_max_health(rng)) / 3.0;
    let jump = (p1_jump + p2_jump + generate_jump_strength(rng)) / 3.0;
    let speed = (p1_speed + p2_speed + generate_speed(rng)) / 3.0;
    (health, jump, speed)
}

// abstract_horse: feed horse items, handle health, temper, growth and breeding (AbstractHorse.handleEating)
pub fn feed_horse(
    horse: &dyn Mob,
    player: &Player,
    item_stack: &mut ItemStack,
    flags: &AtomicU8,
    temper: &AtomicI32,
    eating_sound: Sound,
    can_breed: bool,
) -> bool {
    let item = item_stack.get_item();
    let mut item_used = false;
    let mut heal = 0.0f32;
    let mut age_up = 0i32;
    let mut add_temper = 0i32;

    if item == &Item::WHEAT {
        heal = 2.0;
        age_up = 20;
        add_temper = 3;
    } else if item == &Item::SUGAR {
        heal = 1.0;
        age_up = 30;
        add_temper = 3;
    } else if item == &Item::HAY_BLOCK {
        heal = 20.0;
        age_up = 180;
    } else if item == &Item::APPLE || item == &Item::CARROT {
        heal = 3.0;
        age_up = 60;
        add_temper = 3;
    } else if item == &Item::RED_MUSHROOM {
        heal = 3.0;
        add_temper = 3;
    } else if item == &Item::GOLDEN_CARROT {
        heal = 4.0;
        age_up = 60;
        add_temper = 5;
        if can_breed
            && horse.is_tamed()
            && !horse.is_baby_mob()
            && let Some(animal) = horse.as_animal()
            && !animal.get_mob_entity().is_in_love()
        {
            item_used = true;
            animal
                .get_mob_entity()
                .set_love_ticks(600, Some(player.gameprofile.id));
        }
    } else if item == &Item::GOLDEN_APPLE || item == &Item::ENCHANTED_GOLDEN_APPLE {
        heal = 10.0;
        age_up = 240;
        add_temper = 10;
        if can_breed
            && horse.is_tamed()
            && !horse.is_baby_mob()
            && let Some(animal) = horse.as_animal()
            && !animal.get_mob_entity().is_in_love()
        {
            item_used = true;
            animal
                .get_mob_entity()
                .set_love_ticks(600, Some(player.gameprofile.id));
        }
    }

    let living = &horse.get_mob_entity().living_entity;
    let health = living.health.load();
    let max_health = living.get_max_health();
    if health < max_health && heal > 0.0 {
        living.heal(heal);
        item_used = true;
    }

    if horse.is_baby_mob()
        && age_up > 0
        && let Some(ageable) = horse.as_ageable()
    {
        ageable.age_up(age_up, true);
        item_used = true;
    }

    let cur_temper = temper.load(Ordering::Relaxed);
    if add_temper > 0 && (item_used || !horse.is_tamed()) && cur_temper < 100 {
        temper.store((cur_temper + add_temper).min(100), Ordering::Relaxed);
        item_used = true;
    }

    if item_used {
        // Open mouth flag
        let current = flags.load(Ordering::Relaxed);
        flags.store(current | FLAG_OPEN_MOUTH, Ordering::Relaxed);
        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        let entity = horse.get_entity();
        let world = entity.world.load();
        world.play_sound(eating_sound, SoundCategory::Neutral, &entity.pos.load());
        world.emit_game_event(GameEvent::Eat.name(), entity.pos.load());
    }

    item_used
}

// abstract_horse: tame with player (AbstractHorse.tameWithName), the flag reaches the clients
pub fn tame_horse(
    horse: &dyn Mob,
    player: &Player,
    flags: &AtomicU8,
    owner: &AtomicCell<Option<Uuid>>,
) -> bool {
    let value = flags.load(Ordering::Relaxed) | FLAG_TAME;
    flags.store(value, Ordering::Relaxed);
    owner.store(Some(player.gameprofile.id));

    let entity = horse.get_entity();
    entity.set_synced_data(
        pumpkin_data::tracked_data::abstract_horse::DATA_ID_FLAGS,
        value as i8,
    );
    let world = entity.world.load();
    world.broadcast_entity_event(
        entity,
        EntityStatus::TamingSucceeded,
        ActorEventID::TamingSucceeded,
    );

    player.trigger_advancement(AdvancementTrigger::TameAnimal {
        entity_type: format!("minecraft:{}", entity.entity_type.resource_name),
    });

    true
}

// abstract_horse: AbstractHorse.makeMad of a mob that cannot rear (Llama.canPerformRearing), only the sound
pub fn throw_rider_off(horse: &dyn Mob, _flags: &AtomicU8, angry_sound: Sound) {
    horse.make_sound(angry_sound);
}

// abstract_horse: face rider and sync rotation (AbstractHorse.tickRidden)
pub fn tick_ridden_horse(horse: &dyn Mob, controller: &Arc<dyn EntityBase>) {
    let rider = controller.get_entity();
    let yaw = rider.yaw.load();
    let entity = horse.get_entity();
    entity.set_rotation(yaw, rider.pitch.load() * 0.5);
    entity.head_yaw.store(yaw);
    entity.body_yaw.store(yaw);
}

// abstract_horse: saddled + player passenger steers (AbstractHorse.getControllingPassenger)
pub fn controlling_passenger_horse(
    horse: &dyn Mob,
    is_saddled: bool,
) -> Option<Arc<dyn EntityBase>> {
    if horse.get_mob_entity().is_no_ai() || !is_saddled {
        return None;
    }
    let passengers = horse
        .get_entity()
        .passengers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let first = passengers.first()?.clone();
    first.get_player().is_some().then_some(first)
}

// abstract_horse: play horse step sound (AbstractHorse.playStepSound)
pub fn play_horse_step_sound(
    horse: &dyn Mob,
    pos: &BlockPos,
    state: &BlockState,
    can_gallop: bool,
) {
    if state.is_liquid() {
        return;
    }
    let entity = horse.get_entity();
    let above = entity.world.load().get_block_state(&pos.up());
    let sound_type = if above.id.to_block() == &Block::SNOW {
        above.get_sound_type()
    } else {
        state.get_sound_type()
    };
    let volume = sound_type.volume * 0.15;
    let ridden = entity.has_passengers() && can_gallop;
    let sound = if ridden || is_wood_sound_type(sound_type) {
        Sound::EntityHorseStepWood
    } else if entity.age.load(Ordering::Relaxed) < 0 {
        Sound::EntityBabyHorseStep
    } else {
        Sound::EntityHorseStep
    };
    horse.play_entity_sound(sound, volume, sound_type.pitch);
}

// abstract_horse: wood sound type check (AbstractHorse.isWoodSoundType)
#[must_use]
pub fn is_wood_sound_type(sound_type: &SoundType) -> bool {
    matches!(
        sound_type.name,
        "WOOD" | "NETHER_WOOD" | "STEM" | "CHERRY_WOOD" | "BAMBOO_WOOD"
    )
}

// ---------------------------------------------------------------------------------------------
// AbstractHorse behaviour shared by every horse-like mob (AbstractHorse.java)
// ---------------------------------------------------------------------------------------------

use crossbeam::atomic::AtomicCell as Cell;
use std::sync::atomic::AtomicBool;

use crate::entity::ageable::AgeableMob;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::passive::animal::Animal;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::Vector3;

/// The counters vanilla keeps next to the flags.
#[derive(Default)]
pub struct HorseCore {
    pub eating_counter: AtomicI32,
    pub mouth_counter: AtomicI32,
    pub stand_counter: AtomicI32,
    pub allow_stand_sliding: AtomicBool,
    pub jump_scale: Cell<f32>,
    pub gallop_sound_counter: AtomicI32,
}

pub const MAX_TEMPER: i32 = 100;
pub const STAND_TICKS: i32 = 20;
pub const AMBIENT_SOUND_INTERVAL: i32 = 400;
pub const HORSE_SOUND_VOLUME: f32 = 0.8;
const EATING_GRASS_TICKS: i32 = 50;
const MOUTH_OPEN_TICKS: i32 = 30;
const MOMMY_RANGE: f64 = 16.0;
const JUMP_SOUND_VOLUME: f32 = 0.4;
const LAND_SOUND_VOLUME: f32 = 0.4;

pub const ABSTRACT_HORSES: &[&pumpkin_data::entity::EntityType] = &[
    &pumpkin_data::entity::EntityType::HORSE,
    &pumpkin_data::entity::EntityType::DONKEY,
    &pumpkin_data::entity::EntityType::MULE,
    &pumpkin_data::entity::EntityType::SKELETON_HORSE,
    &pumpkin_data::entity::EntityType::ZOMBIE_HORSE,
    &pumpkin_data::entity::EntityType::CAMEL,
    &pumpkin_data::entity::EntityType::CAMEL_HUSK,
    &pumpkin_data::entity::EntityType::LLAMA,
    &pumpkin_data::entity::EntityType::TRADER_LLAMA,
];

/// `AbstractHorse`: what a species supplies, the rest is shared.
pub trait HorseLike: Mob + Animal + AgeableMob + Sized {
    fn core(&self) -> &HorseCore;
    fn horse_flags(&self) -> &AtomicU8;
    fn horse_temper(&self) -> &AtomicI32;
    fn horse_owner(&self) -> &Cell<Option<Uuid>>;
    /// Writes the flag byte and syncs it to the clients.
    fn store_flags(&self, flags: u8);
    fn horse_saddled(&self) -> bool;
    fn set_horse_saddle(&self, saddled: bool);
    fn horse_eat_sound(&self) -> Sound;
    fn angry_sound(&self) -> Sound;
    fn jump_sound(&self) -> Sound;
    fn ambient_stand_sound(&self) -> Option<Sound> {
        self.get_ambient_sound()
    }
    fn can_perform_rearing(&self) -> bool {
        true
    }
    fn can_gallop(&self) -> bool {
        true
    }
    fn can_eat_grass(&self) -> bool {
        true
    }
    fn horse_max_temper(&self) -> i32 {
        MAX_TEMPER
    }

    fn flag(&self, flag: u8) -> bool {
        self.horse_flags().load(Ordering::Relaxed) & flag != 0
    }

    fn put_flag(&self, flag: u8, value: bool) {
        let current = self.horse_flags().load(Ordering::Relaxed);
        self.store_flags(if value {
            current | flag
        } else {
            current & !flag
        });
    }

    fn is_horse_tame(&self) -> bool {
        self.flag(FLAG_TAME)
    }

    fn is_horse_eating(&self) -> bool {
        self.flag(FLAG_EATING)
    }

    fn is_horse_standing(&self) -> bool {
        self.flag(FLAG_STANDING)
    }

    fn is_horse_bred(&self) -> bool {
        self.flag(FLAG_BRED)
    }

    fn set_horse_eating(&self, value: bool) {
        self.put_flag(FLAG_EATING, value);
    }

    // AbstractHorse.canParent
    fn can_parent(&self) -> bool {
        let entity = self.get_entity();
        let living = &self.get_mob_entity().living_entity;
        !entity.has_passengers()
            && !entity.has_vehicle()
            && self.is_horse_tame()
            && !self.is_baby()
            && living.health.load() >= living.get_max_health()
            && self.get_mob_entity().is_in_love()
    }

    // AbstractHorse.canUseSlot(SADDLE)
    fn can_use_saddle_slot(&self) -> bool {
        self.get_mob_entity().living_entity.is_alive() && !self.is_baby() && self.is_horse_tame()
    }

    fn open_mouth(&self) {
        self.core().mouth_counter.store(1, Ordering::Relaxed);
        self.put_flag(FLAG_OPEN_MOUTH, true);
    }

    // AbstractHorse.eating
    fn horse_eating(&self) {
        self.open_mouth();
        if !self.get_entity().is_silent() {
            let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
                .mul_add(0.2, 1.0);
            self.play_entity_sound(self.horse_eat_sound(), 1.0, pitch);
        }
    }

    // AbstractHorse.setStanding
    fn set_standing(&self, ticks: i32) {
        self.set_horse_eating(false);
        self.put_flag(FLAG_STANDING, true);
        self.core().stand_counter.store(ticks, Ordering::Relaxed);
    }

    // AbstractHorse.clearStanding
    fn clear_standing(&self) {
        self.put_flag(FLAG_STANDING, false);
        self.core().stand_counter.store(0, Ordering::Relaxed);
    }

    // AbstractHorse.standIfPossible
    fn stand_if_possible(&self) {
        if self.can_perform_rearing() && !self.get_mob_entity().is_no_ai() {
            self.set_standing(STAND_TICKS);
        }
    }

    // AbstractHorse.makeMad
    fn horse_make_mad(&self) {
        if !self.is_horse_standing() {
            self.stand_if_possible();
            self.make_sound(self.angry_sound());
        }
    }

    // AbstractHorse.tameWithName
    fn horse_tame(&self, player: &Player) -> bool {
        self.horse_owner().store(Some(player.gameprofile.id));
        self.put_flag(FLAG_TAME, true);
        let entity = self.get_entity();
        player.trigger_advancement(AdvancementTrigger::TameAnimal {
            entity_type: format!("minecraft:{}", entity.entity_type.resource_name),
        });
        entity.world.load().broadcast_entity_event(
            entity,
            EntityStatus::TamingSucceeded,
            ActorEventID::TamingSucceeded,
        );
        true
    }

    // AbstractHorse.modifyTemper
    fn horse_modify_temper(&self, delta: i32) {
        let cur = self.horse_temper().load(Ordering::Relaxed);
        self.horse_temper().store(
            (cur + delta).clamp(0, self.horse_max_temper()),
            Ordering::Relaxed,
        );
    }

    // AbstractHorse.handleEating (the generic food table)
    fn horse_handle_eating(&self, player: &Player, stack: &ItemStack) -> bool {
        let item = stack.get_item();
        let mut item_used = false;
        let (mut heal, mut age_up, mut temper, mut in_love_food) = (0.0f32, 0, 0, false);
        if item == &Item::WHEAT {
            (heal, age_up, temper) = (2.0, 20, 3);
        } else if item == &Item::SUGAR {
            (heal, age_up, temper) = (1.0, 30, 3);
        } else if item == &Item::HAY_BLOCK {
            (heal, age_up) = (20.0, 180);
        } else if item == &Item::APPLE || item == &Item::CARROT {
            (heal, age_up, temper) = (3.0, 60, 3);
        } else if item == &Item::RED_MUSHROOM {
            (heal, temper) = (3.0, 3);
        } else if item == &Item::GOLDEN_CARROT {
            (heal, age_up, temper, in_love_food) = (4.0, 60, 5, true);
        } else if item == &Item::GOLDEN_APPLE || item == &Item::ENCHANTED_GOLDEN_APPLE {
            (heal, age_up, temper, in_love_food) = (10.0, 240, 10, true);
        }
        if in_love_food
            && self.is_horse_tame()
            && self.get_age() == 0
            && !self.get_mob_entity().is_in_love()
        {
            item_used = true;
            self.get_mob_entity()
                .set_love_ticks(600, Some(player.gameprofile.id));
        }
        let living = &self.get_mob_entity().living_entity;
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
            && self.horse_temper().load(Ordering::Relaxed) < self.horse_max_temper()
        {
            self.horse_modify_temper(temper);
            item_used = true;
        }
        if item_used {
            self.horse_eating();
            let entity = self.get_entity();
            entity
                .world
                .load()
                .emit_game_event(GameEvent::Eat.name(), entity.pos.load());
        }
        item_used
    }

    // AbstractHorse.fedFood
    fn horse_fed_food(&self, player: &Player, stack: &mut ItemStack) -> bool {
        let ate = self.horse_handle_eating(player, stack);
        if ate {
            stack.decrement_unless_creative(player.gamemode.load(), 1);
        }
        ate
    }

    fn do_player_ride(&self, player: &Arc<Player>) -> bool {
        self.do_player_ride_default(player)
    }

    // AbstractHorse.doPlayerRide
    fn do_player_ride_default(&self, player: &Arc<Player>) -> bool {
        self.set_horse_eating(false);
        self.clear_standing();
        let entity = self.get_entity();
        player
            .get_entity()
            .set_rotation(entity.yaw.load(), entity.pitch.load());
        let world = player.world();
        if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
            && let Some(passenger) = world.get_player_by_id(player.entity_id())
        {
            entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
        }
        true
    }

    /// `AbstractHorse.mobInteract`; `equip` is the own item handling of the species, e.g. body armor.
    fn abstract_horse_interact(
        &self,
        player: &Arc<Player>,
        stack: &mut ItemStack,
        equip: impl FnOnce(&Self, &Arc<Player>, &mut ItemStack) -> bool,
    ) -> bool {
        if self.get_entity().has_passengers() || self.is_baby() {
            return self.animal_interact(player, stack, Sound::EntityHorseAmbient);
        }
        if self.is_horse_tame() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }
        if !stack.is_empty() {
            if stack.get_item() == &Item::SADDLE
                && self.can_use_saddle_slot()
                && !self.horse_saddled()
            {
                self.set_horse_saddle(true);
                stack.decrement_unless_creative(player.gamemode.load(), 1);
                self.play_entity_sound(Sound::EntityHorseSaddle, 1.0, 1.0);
                return true;
            }
            if equip(self, player, stack) {
                return true;
            }
        }
        self.do_player_ride(player)
    }

    // AbstractHorse.onPlayerJump, handleStartJump
    fn horse_start_jump(&self, jump_scale: i32) {
        if !self.horse_saddled() || jump_scale <= 0 {
            return;
        }
        let scale = if jump_scale >= 90 {
            1.0
        } else {
            0.4 + 0.4 * jump_scale as f32 / 90.0
        };
        self.core().jump_scale.store(scale);
        self.core()
            .allow_stand_sliding
            .store(true, Ordering::Relaxed);
        self.stand_if_possible();
        self.play_entity_sound(self.jump_sound(), JUMP_SOUND_VOLUME, 1.0);
    }

    // AbstractHorse.isImmobile
    fn horse_immobile(&self) -> bool {
        (self.get_mob_entity().living_entity.is_dead_or_dying()
            && self.get_entity().has_passengers()
            && self.horse_saddled())
            || self.is_horse_eating()
            || self.is_horse_standing()
    }

    // AbstractHorse.hurtServer, a third of the hits rear the horse up
    fn horse_hurt(&self) {
        if self.get_random().random_range(0..3) == 0 {
            self.stand_if_possible();
        }
    }

    // AbstractHorse.causeFallDamage landing sound
    fn horse_landed(&self, fall_distance: f32) {
        if fall_distance > 1.0 {
            let sound = if self.is_baby() {
                Sound::EntityBabyHorseLand
            } else {
                Sound::EntityHorseLand
            };
            self.play_entity_sound(sound, LAND_SOUND_VOLUME, 1.0);
        }
    }

    // AbstractHorse.playStepSound with the gallop counter
    fn horse_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        if state.is_liquid() {
            return;
        }
        let entity = self.get_entity();
        let above = entity.world.load().get_block_state(&pos.up());
        let sound_type = if above.id.to_block() == &Block::SNOW {
            above.get_sound_type()
        } else {
            state.get_sound_type()
        };
        let volume = sound_type.volume * 0.15;
        if entity.has_passengers() && self.can_gallop() {
            let counter = self
                .core()
                .gallop_sound_counter
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if counter > 5 && counter % 3 == 0 {
                self.play_entity_sound(Sound::EntityHorseGallop, volume, sound_type.pitch);
            } else if counter <= 5 {
                self.play_entity_sound(Sound::EntityHorseStepWood, volume, sound_type.pitch);
            }
        } else if is_wood_sound_type(sound_type) {
            self.play_entity_sound(Sound::EntityHorseStepWood, volume, sound_type.pitch);
        } else {
            let sound = if self.is_baby() {
                Sound::EntityBabyHorseStep
            } else {
                Sound::EntityHorseStep
            };
            self.play_entity_sound(sound, volume, sound_type.pitch);
        }
    }

    // AbstractHorse.aiStep: slow healing, grazing and the foal following its mother
    fn horse_ai_step(&self) {
        let living = &self.get_mob_entity().living_entity;
        if !living.is_alive() {
            return;
        }
        let entity = self.get_entity();
        if self.get_random().random_range(0..900) == 0
            && living.death_time.load(Ordering::Relaxed) == 0
        {
            living.heal(1.0);
        }
        if self.can_eat_grass() {
            if !self.is_horse_eating()
                && !entity.has_passengers()
                && self.get_random().random_range(0..300) == 0
                && entity
                    .world
                    .load()
                    .get_block(&entity.block_pos.load().down())
                    == &Block::GRASS_BLOCK
            {
                self.set_horse_eating(true);
            }
            if self.is_horse_eating() {
                let counter = self.core().eating_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if counter > EATING_GRASS_TICKS {
                    self.core().eating_counter.store(0, Ordering::Relaxed);
                    self.set_horse_eating(false);
                }
            }
        }
        self.follow_mommy();
    }

    fn follow_mommy(&self) {
        self.follow_mommy_default();
    }

    // AbstractHorse.followMommy
    fn follow_mommy_default(&self) {
        if !(self.is_horse_bred() && self.is_baby() && !self.is_horse_eating()) {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        let bounds = entity.bounding_box.load();
        let area = BoundingBox::new(
            Vector3::new(
                bounds.min.x - MOMMY_RANGE,
                bounds.min.y - MOMMY_RANGE,
                bounds.min.z - MOMMY_RANGE,
            ),
            Vector3::new(
                bounds.max.x + MOMMY_RANGE,
                bounds.max.y + MOMMY_RANGE,
                bounds.max.z + MOMMY_RANGE,
            ),
        );
        let conditions = TargetPredicate::create_non_attackable()
            .ignore_visibility()
            .set_base_max_distance(MOMMY_RANGE);
        let mommy = world
            .get_all_at_box(&area)
            .into_iter()
            .filter(|other| {
                other.get_entity().entity_id != entity.entity_id
                    && ABSTRACT_HORSES.contains(&other.get_entity().entity_type)
                    && other.get_mob().is_some_and(Mob::horse_is_bred)
                    && conditions.test(&world, Some(self), other.as_ref())
            })
            .min_by(|a, b| {
                a.get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&pos)
                    .total_cmp(&b.get_entity().pos.load().squared_distance_to_vec(&pos))
            });
        if let Some(mommy) = mommy {
            let mommy_pos = mommy.get_entity().pos.load();
            if mommy_pos.squared_distance_to_vec(&pos) > 4.0 {
                let _ = self
                    .get_mob_entity()
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .create_path(&self.get_mob_entity().living_entity, mommy_pos, 0);
            }
        }
    }

    // AbstractHorse.tick counters
    fn horse_tick(&self) {
        let core = self.core();
        let mouth = core.mouth_counter.load(Ordering::Relaxed);
        if mouth > 0 {
            if mouth + 1 > MOUTH_OPEN_TICKS {
                core.mouth_counter.store(0, Ordering::Relaxed);
                self.put_flag(FLAG_OPEN_MOUTH, false);
            } else {
                core.mouth_counter.store(mouth + 1, Ordering::Relaxed);
            }
        }
        let stand = core.stand_counter.load(Ordering::Relaxed);
        if stand > 0 {
            core.stand_counter.store(stand - 1, Ordering::Relaxed);
            if stand - 1 <= 0 {
                self.clear_standing();
            }
        }
        if !self.is_horse_standing() {
            core.allow_stand_sliding.store(false, Ordering::Relaxed);
        }
    }

    // AbstractHorse.addAdditionalSaveData
    fn write_horse_nbt(&self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        nbt.put_bool("EatingHaystack", self.is_horse_eating());
        nbt.put_bool("Bred", self.is_horse_bred());
        nbt.put_int("Temper", self.horse_temper().load(Ordering::Relaxed));
        nbt.put_bool("Tame", self.is_horse_tame());
        if let Some(owner) = self.horse_owner().load() {
            nbt.put_uuid("Owner", owner);
        }
    }

    // AbstractHorse.readAdditionalSaveData
    fn read_horse_nbt(&self, nbt: &pumpkin_nbt::compound::NbtCompound) {
        self.put_flag(FLAG_EATING, nbt.get_bool("EatingHaystack").unwrap_or(false));
        self.put_flag(FLAG_BRED, nbt.get_bool("Bred").unwrap_or(false));
        self.horse_temper()
            .store(nbt.get_int("Temper").unwrap_or(0), Ordering::Relaxed);
        self.put_flag(FLAG_TAME, nbt.get_bool("Tame").unwrap_or(false));
        if let Some(owner) = nbt.get_uuid("Owner") {
            self.horse_owner().store(Some(owner));
        }
    }
}

/// `RandomStandGoal` for any horse-like mob.
pub struct RandomStandGoal {
    next_stand: i32,
}

impl Default for RandomStandGoal {
    fn default() -> Self {
        Self {
            next_stand: -AMBIENT_SOUND_INTERVAL,
        }
    }
}

impl crate::entity::ai::goal::Goal for RandomStandGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.next_stand += 1;
        if self.next_stand > 0 && mob.get_random().random_range(0..1000) < self.next_stand {
            self.next_stand = -AMBIENT_SOUND_INTERVAL;
            return !mob.is_immobile() && mob.get_random().random_range(0..10) == 0;
        }
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        mob.horse_stand_and_whinny();
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> crate::entity::ai::goal::Controls {
        crate::entity::ai::goal::Controls::empty()
    }
}

/// The `Mob` hooks every horse-like species forwards to `HorseLike`.
#[macro_export]
macro_rules! horse_mob_hooks {
    () => {
        fn is_tamed(&self) -> bool {
            $crate::entity::passive::abstract_horse::HorseLike::is_horse_tame(self)
        }

        fn horse_is_bred(&self) -> bool {
            $crate::entity::passive::abstract_horse::HorseLike::is_horse_bred(self)
        }

        fn horse_stand_and_whinny(&self) {
            $crate::entity::passive::abstract_horse::HorseLike::stand_if_possible(self);
            if let Some(sound) =
                $crate::entity::passive::abstract_horse::HorseLike::ambient_stand_sound(self)
            {
                self.make_sound(sound);
            }
        }

        fn mob_is_pushable(&self) -> bool {
            !self.get_entity().has_passengers()
        }

        fn get_ambient_sound_interval(&self) -> i32 {
            $crate::entity::passive::abstract_horse::AMBIENT_SOUND_INTERVAL
        }

        fn is_mob_immobile(&self) -> bool {
            $crate::entity::passive::abstract_horse::HorseLike::horse_immobile(self)
        }

        fn on_player_start_jump(&self, jump_scale: i32) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_start_jump(self, jump_scale);
        }

        fn on_player_stop_jump(&self) {}

        fn get_temper(&self) -> i32 {
            $crate::entity::passive::abstract_horse::HorseLike::horse_temper(self)
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        fn max_temper(&self) -> i32 {
            $crate::entity::passive::abstract_horse::HorseLike::horse_max_temper(self)
        }

        fn modify_temper(&self, delta: i32) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_modify_temper(self, delta);
        }

        fn tame_with_name(&self, player: &$crate::entity::player::Player) -> bool {
            $crate::entity::passive::abstract_horse::HorseLike::horse_tame(self, player)
        }

        fn make_mad(&self) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_make_mad(self);
        }

        fn pre_damage(
            &self,
            _damage_type: pumpkin_data::damage::DamageType,
            _source: Option<&dyn $crate::entity::EntityBase>,
        ) -> bool {
            $crate::entity::passive::abstract_horse::HorseLike::horse_hurt(self);
            true
        }

        fn on_cause_fall_damage(&self, fall_distance: f32) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_landed(self, fall_distance);
        }

        fn mob_ai_step(&self) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_ai_step(self);
        }
    };
}

/// `Mob::mob_tick` of a horse-like species without its own tick work.
#[macro_export]
macro_rules! horse_mob_tick {
    () => {
        fn mob_tick(&self, _caller: &dyn $crate::entity::EntityBase) {
            $crate::entity::passive::abstract_horse::HorseLike::horse_tick(self);
        }
    };
}
