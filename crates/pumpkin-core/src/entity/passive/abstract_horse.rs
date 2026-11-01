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

// abstract_horse: tame with player (AbstractHorse.tameWithName)
pub fn tame_horse(
    horse: &dyn Mob,
    player: &Player,
    flags: &AtomicU8,
    owner: &AtomicCell<Option<Uuid>>,
) -> bool {
    let current = flags.load(Ordering::Relaxed);
    flags.store(current | FLAG_TAME, Ordering::Relaxed);
    owner.store(Some(player.gameprofile.id));

    let entity = horse.get_entity();
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

// abstract_horse: throw rider off with rearing and angry sound (AbstractHorse.makeMad)
pub fn throw_rider_off(horse: &dyn Mob, flags: &AtomicU8, angry_sound: Sound) {
    let current = flags.load(Ordering::Relaxed);
    flags.store((current | FLAG_STANDING) & !FLAG_EATING, Ordering::Relaxed);

    let entity = horse.get_entity();
    let passengers = entity
        .passengers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for p in passengers {
        entity.remove_passenger(p.get_entity().entity_id);
    }

    let world = entity.world.load();
    world.play_sound(angry_sound, SoundCategory::Neutral, &entity.pos.load());
    world.broadcast_entity_event(
        entity,
        EntityStatus::TamingFailed,
        ActorEventID::TamingFailed,
    );
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
