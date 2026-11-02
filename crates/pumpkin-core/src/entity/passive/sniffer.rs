use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_protocol::bedrock::server::ActorEventID;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::{
        brain::{BrainState, memory::GlobalPos, memory::PackedMemories, memory::types},
        goal::breed::BreedGoal,
        pathfinder::node::PathType,
    },
    custom_sound::CustomSound,
    item::ItemEntity,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};

mod ai;

pub const SNIFFER_BABY_START_AGE: i32 = -48000;
const DIGGING_DROP_SEED_OFFSET_TICKS: i32 = 120;
const DIGGING_BB_HEIGHT_OFFSET: f32 = 0.4;
const DIGGING_EYE_HEIGHT: f32 = 0.81;
const MAX_EXPLORED_POSITIONS: usize = 20;
const MAX_HEAD_Y_ROT: f32 = 50.0;
const HEAD_FORWARD_OFFSET: f64 = 2.25;
const BREEDING_COOLDOWN: i32 = 6000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum SnifferState {
    #[default]
    Idling = 0,
    FeelingHappy = 1,
    Scenting = 2,
    Sniffing = 3,
    Searching = 4,
    Digging = 5,
    Rising = 6,
}

impl SnifferState {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::FeelingHappy,
            2 => Self::Scenting,
            3 => Self::Sniffing,
            4 => Self::Searching,
            5 => Self::Digging,
            6 => Self::Rising,
            _ => Self::Idling,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }
}

/// Represents a Sniffer, a large passive mob that digs up ancient seeds.
///
/// Wiki: <https://minecraft.wiki/w/Sniffer>
pub struct SnifferEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub state: AtomicI32,
    pub drop_seed_at_tick: AtomicI32,
}

impl SnifferEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        {
            let navigator = mob_entity
                .navigator
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            navigator.set_can_float(true);
            navigator.set_pathfinding_malus(PathType::Water, -1.0);
            navigator.set_pathfinding_malus(PathType::PowderSnow, -1.0);
            navigator.set_pathfinding_malus(PathType::DamageCautious, -1.0);
        }
        Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            state: AtomicI32::new(SnifferState::Idling.id()),
            drop_seed_at_tick: AtomicI32::new(0),
        })
    }

    #[must_use]
    pub fn get_state(&self) -> SnifferState {
        SnifferState::from_id(self.state.load(Ordering::Relaxed))
    }

    // sniffer: setState + onSyncedDataUpdated refreshDimensions
    fn set_state(&self, state: SnifferState) {
        self.state.store(state.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::sniffer::STATE,
            VarInt(state.id()),
        );
        self.update_dimensions();
    }

    // sniffer: getDefaultDimensions, shorter while digging
    fn update_dimensions(&self) {
        let entity = self.get_entity();
        let base = Entity::type_dimensions(entity.entity_type);
        let scale = if self.is_baby() { 0.5 } else { 1.0 };
        let dimensions = if self.get_state() == SnifferState::Digging {
            EntityDimensions {
                width: base.width * scale,
                height: (base.height - DIGGING_BB_HEIGHT_OFFSET) * scale,
                eye_height: DIGGING_EYE_HEIGHT * scale,
            }
        } else {
            EntityDimensions {
                width: base.width * scale,
                height: base.height * scale,
                eye_height: base.eye_height * scale,
            }
        };
        entity.set_default_dimensions(dimensions);
    }

    // sniffer: transitionTo
    pub fn transition_to(&self, state: SnifferState) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        match state {
            SnifferState::Idling | SnifferState::Searching => self.set_state(state),
            SnifferState::FeelingHappy => {
                world.play_sound(Sound::EntitySnifferHappy, SoundCategory::Neutral, &pos);
                self.set_state(state);
            }
            SnifferState::Scenting => {
                self.set_state(state);
                let pitch = if self.is_baby() { 1.3 } else { 1.0 };
                world.play_sound_fine(
                    Sound::EntitySnifferScenting,
                    SoundCategory::Neutral,
                    &pos,
                    1.0,
                    pitch,
                );
            }
            SnifferState::Sniffing => {
                world.play_sound(Sound::EntitySnifferSniffing, SoundCategory::Neutral, &pos);
                self.set_state(state);
            }
            SnifferState::Digging => {
                self.set_state(state);
                // sniffer: onDiggingStart
                let drop_tick =
                    entity.tick_count.load(Ordering::Relaxed) + DIGGING_DROP_SEED_OFFSET_TICKS;
                self.drop_seed_at_tick.store(drop_tick, Ordering::Relaxed);
                entity.set_synced_data(
                    pumpkin_data::tracked_data::sniffer::DROP_SEED_AT_TICK,
                    VarInt(drop_tick),
                );
                world.send_entity_status(
                    entity,
                    EntityStatus::SnifferDiggingSound,
                    ActorEventID::None,
                );
            }
            SnifferState::Rising => {
                world.play_sound(
                    Sound::EntitySnifferDiggingStop,
                    SoundCategory::Neutral,
                    &pos,
                );
                self.set_state(state);
            }
        }
    }

    // sniffer: isTempted
    #[must_use]
    pub fn is_tempted(brain: &BrainState) -> bool {
        brain.has_memory_value(types::TEMPTING_PLAYER.id())
    }

    fn is_panicking_in(brain: &BrainState) -> bool {
        brain.has_memory_value(types::IS_PANICKING.id())
    }

    fn is_passenger(&self) -> bool {
        self.get_entity()
            .vehicle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    // sniffer: canSniff
    #[must_use]
    pub fn can_sniff(&self, brain: &BrainState) -> bool {
        let entity = self.get_entity();
        !Self::is_tempted(brain)
            && !Self::is_panicking_in(brain)
            && !entity.is_in_water()
            && !self.mob_entity.is_in_love()
            && entity.on_ground.load(Ordering::Relaxed)
            && !self.is_passenger()
            && !entity.is_leashed()
    }

    // sniffer: getHeadPosition
    fn head_position(&self) -> Vector3<f64> {
        let entity = self.get_entity();
        let forward =
            Vector3::rotation_vector(f64::from(entity.pitch.load()), f64::from(entity.yaw.load()));
        entity.pos.load() + forward * HEAD_FORWARD_OFFSET
    }

    // sniffer: getHeadBlock
    fn head_block(&self) -> BlockPos {
        let head = self.head_position();
        BlockPos::floored(
            head.x,
            self.get_entity().pos.load().y + f64::from(0.2f32),
            head.z,
        )
    }

    // sniffer: canDig()
    #[must_use]
    pub fn can_dig(&self, brain: &BrainState) -> bool {
        let entity = self.get_entity();
        !Self::is_panicking_in(brain)
            && !Self::is_tempted(brain)
            && !self.is_baby()
            && !entity.is_in_water()
            && entity.on_ground.load(Ordering::Relaxed)
            && !self.is_passenger()
            && self.can_dig_at(brain, self.head_block().down())
    }

    // sniffer: canDig(BlockPos)
    fn can_dig_at(&self, brain: &BrainState, pos: BlockPos) -> bool {
        let world = self.get_entity().world.load();
        if !world
            .get_block(&pos)
            .has_tag(&tag::Block::MINECRAFT_SNIFFER_DIGGABLE_BLOCK)
        {
            return false;
        }
        let dimension = world.dimension.minecraft_name;
        let explored = brain
            .get(types::SNIFFER_EXPLORED_POSITIONS)
            .is_some_and(|list| {
                list.iter().any(|explored| {
                    explored.pos == pos && explored.dimension.minecraft_name == dimension
                })
            });
        if explored {
            return false;
        }
        let steered = self.mob_entity.steered();
        steered
            .navigator()
            .create_path(
                &self.mob_entity.living_entity,
                Vector3::new(f64::from(pos.0.x), f64::from(pos.0.y), f64::from(pos.0.z)),
                1,
            )
            .is_some_and(|path| path.can_reach())
    }

    // sniffer: calculateDigPosition
    #[must_use]
    pub fn calculate_dig_position(&self, brain: &BrainState) -> Option<BlockPos> {
        let world = self.get_entity().world.load();
        (0..5).find_map(|idx| {
            let pos = crate::entity::ai::util::land_random_pos::get_pos(self, 10 + 2 * idx, 3)?;
            let pos = BlockPos::floored_v(pos);
            let in_border = world
                .worldborder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_block(pos.0.x, pos.0.z);
            let below = pos.down();
            (in_border && self.can_dig_at(brain, below)).then_some(below)
        })
    }

    // sniffer: onDiggingComplete + storeExploredPosition
    pub fn on_digging_complete(&self, brain: &mut BrainState, success: bool) {
        if !success {
            return;
        }
        let entity = self.get_entity();
        let pos = entity.pos.load();
        let on_pos = BlockPos::floored(pos.x, pos.y - f64::from(0.2f32), pos.z);
        let Some(dimension) = pumpkin_data::dimension::Dimension::from_name(
            entity.world.load().dimension.minecraft_name,
        ) else {
            return;
        };
        let mut updated: Vec<GlobalPos> = Vec::with_capacity(MAX_EXPLORED_POSITIONS + 1);
        updated.push(GlobalPos::new(dimension, on_pos));
        if let Some(list) = brain.get(types::SNIFFER_EXPLORED_POSITIONS) {
            updated.extend(list.iter().take(MAX_EXPLORED_POSITIONS).copied());
        }
        brain.set(types::SNIFFER_EXPLORED_POSITIONS, updated);
    }

    // sniffer: resetSniffing
    pub fn reset_sniffing(&self, brain: &mut BrainState) {
        brain.erase(types::SNIFFER_DIGGING.id());
        brain.erase(types::SNIFFER_SNIFFING_TARGET.id());
        self.transition_to(SnifferState::Idling);
    }

    // sniffer: dropSeed
    fn drop_seed(&self) {
        let entity = self.get_entity();
        if self.drop_seed_at_tick.load(Ordering::Relaxed)
            != entity.tick_count.load(Ordering::Relaxed)
        {
            return;
        }
        let world = entity.world.load_full();
        let head = self.head_block();
        let seed = rand::random::<i64>();
        for stack in crate::world::loot::generate_loot(
            &pumpkin_data::loot_table::GAMEPLAY_SNIFFER_DIGGING,
            seed,
        ) {
            let item = ItemEntity::new(
                Entity::new(
                    world.clone(),
                    Vector3::new(
                        f64::from(head.0.x),
                        f64::from(head.0.y),
                        f64::from(head.0.z),
                    ),
                    &EntityType::ITEM,
                ),
                stack,
            );
            world.spawn_entity(Arc::new(item));
        }
        world.play_sound(
            Sound::EntitySnifferDropSeed,
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

impl AgeableMob for SnifferEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn get_baby_start_age(&self) -> i32 {
        SNIFFER_BABY_START_AGE
    }
}

impl Animal for SnifferEntity {
    // sniffer: eat sound (Sniffer.playEatingSound)
    fn eating_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySnifferEat)
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_SNIFFER_FOOD)
    }
}

impl CustomSound for SnifferEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySnifferDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySnifferHurt)
    }
}

impl Mob for SnifferEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntitySnifferStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        match self.get_state() {
            SnifferState::Digging | SnifferState::Searching => None,
            _ => Some(Sound::EntitySnifferIdle),
        }
    }

    fn get_max_head_rotation(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    // sniffer: Sniffer.canMate, both idle-like
    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        let idle_like = |state: SnifferState| {
            matches!(
                state,
                SnifferState::Idling | SnifferState::Scenting | SnifferState::FeelingHappy
            )
        };
        partner
            .get_mob()
            .and_then(|mob| mob.cast_any().downcast_ref::<Self>())
            .is_some_and(|partner| idle_like(self.get_state()) && idle_like(partner.get_state()))
    }

    // sniffer: spawnChildFromBreeding lays an egg
    fn custom_breed(&self, mate: &dyn EntityBase) -> bool {
        let Some(mate_mob) = mate.get_mob() else {
            return false;
        };
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let egg = ItemEntity::new(
            Entity::new(world.clone(), pos, &EntityType::ITEM),
            ItemStack::new(1, &Item::SNIFFER_EGG),
        );
        BreedGoal::award_breeding(self, mate, None);
        self.mob_entity.reset_love_ticks();
        self.mob_entity
            .breeding_cooldown
            .store(BREEDING_COOLDOWN, Ordering::Relaxed);
        mate_mob.get_mob_entity().reset_love_ticks();
        mate.set_breeding_cooldown(BREEDING_COOLDOWN);
        let pitch =
            (self.get_random().random::<f32>() - self.get_random().random::<f32>()) * 0.2 + 0.5;
        world.play_sound_fine(
            Sound::BlockSnifferEggPlop,
            SoundCategory::Neutral,
            &pos,
            1.0,
            pitch,
        );
        world.spawn_entity(Arc::new(egg));
        if world.level_info.load().game_rules.mob_drops {
            crate::entity::experience_orb::ExperienceOrbEntity::spawn(
                &world,
                pos,
                self.get_random().random_range(1..8),
            );
        }
        true
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // sniffer: jumpFromGround nudge
    fn on_jump_from_ground(&self) {
        let entity = self.get_entity();
        let wants_move = self
            .mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .speed_modifier()
            > 0.0;
        if !wants_move {
            return;
        }
        let velocity = entity.velocity.load();
        if velocity.x * velocity.x + velocity.z * velocity.z < 0.01 {
            let yaw = f64::from(entity.yaw.load()).to_radians();
            entity.velocity.store(Vector3::new(
                velocity.x - yaw.sin() * 0.1,
                velocity.y,
                velocity.z + yaw.cos() * 0.1,
            ));
        }
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        match self.get_state() {
            SnifferState::Digging => {
                if self.mob_entity.living_entity.is_dead_or_dying() {
                    self.transition_to(SnifferState::Idling);
                    return;
                }
                let entity = self.get_entity();
                if entity.tick_count.load(Ordering::Relaxed) % 10 == 0 {
                    let head = self.head_block();
                    entity.world.load().emit_game_event(
                        GameEvent::EntityAction.name(),
                        Vector3::new(
                            f64::from(head.0.x),
                            f64::from(head.0.y),
                            f64::from(head.0.z),
                        ),
                    );
                }
                self.drop_seed();
            }
            SnifferState::Idling => {}
            _ => {
                if self.mob_entity.living_entity.is_dead_or_dying() {
                    self.transition_to(SnifferState::Idling);
                }
            }
        }
    }

    // sniffer: customServerAiStep
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        // sniffer: onPathfindingStart/Done water malus
        let entity = self.get_entity();
        let water_malus = if entity.fire_ticks.load(Ordering::Relaxed) > 0 || entity.is_in_water() {
            0.0
        } else {
            -1.0
        };
        self.mob_entity
            .steered()
            .navigator()
            .set_pathfinding_malus(PathType::Water, water_malus);
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::sniffer::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::sniffer::STATE,
            VarInt(self.get_state().id()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::sniffer::DROP_SEED_AT_TICK,
            VarInt(self.drop_seed_at_tick.load(Ordering::Relaxed)),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntitySnifferIdle)
    }
}
