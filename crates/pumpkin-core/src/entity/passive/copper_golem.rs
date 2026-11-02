use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, AtomicI64, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::block_properties::{CopperGolemStatueLikeProperties, EnumVariants};
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::environment_attribute::Activity;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::world::WorldEvent;
use pumpkin_data::{Block, BlockState};
use pumpkin_inventory::Inventory;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use rand::RngExt;
use uuid::Uuid;

use crate::block::blocks::weathering_copper::WeatherState as BlockWeatherState;
use crate::block::entities::chest::ChestBlockEntity;
use crate::block::entities::copper_golem_statue::CopperGolemStatueBlockEntity;
use crate::block::entities::trapped_chest::TrappedChestBlockEntity;
use crate::entity::ai::brain::activity::ActivityData;
use crate::entity::ai::brain::behavior::transport_items_between_containers::{
    ContainerInteractionState, TransportItemTarget, TransportItemsBetweenContainers,
};
use crate::entity::ai::brain::behavior::{
    AnimalPanic, CountDownCooldownTicks, DoNothing, LookAtTargetSink, MoveToTargetSink, Timed,
    interact_with_door, random_stroll, run_one_with_conditions, set_entity_look_target_sometimes,
};
use crate::entity::ai::brain::memory::{MemoryModuleId, MemoryStatus, PackedMemories, types};
use crate::entity::ai::brain::sensing::SensorType;
use crate::entity::ai::brain::{Brain, BrainProvider};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::item::ItemEntity;
use crate::entity::{
    Entity, EntityBase,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity},
    player::Player,
};
use crate::world::World;

const WEATHERING_TICK_FROM: i32 = 504_000;
const WEATHERING_TICK_TO: i32 = 552_000;
const IGNORE_WEATHERING_TICK: i64 = -2;
const UNSET_WEATHERING_TICK: i64 = -1;
const TURN_TO_STATUE_CHANCE: f32 = 0.0058;
const SPAWN_COOLDOWN_MIN: i32 = 60;
const SPAWN_COOLDOWN_MAX: i32 = 100;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 1.5;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const TRANSPORT_ITEM_HORIZONTAL_SEARCH_RADIUS: i32 = 32;
const TRANSPORT_ITEM_VERTICAL_SEARCH_RADIUS: i32 = 8;
const TICK_TO_START_ON_REACHED_INTERACTION: i32 = 1;
const TICK_TO_PLAY_ON_REACHED_SOUND: i32 = 9;
const REQUIRED_PATH_LENGTH: f32 = 48.0;
const ANTENNA_DROP_HEIGHT: f64 = 1.5;
const CONTAINER_CLOSE_TICK: i32 = 60;
const THROW_VELOCITY: f64 = 0.3;
const HAND_Y_DISTANCE_FROM_EYE: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum WeatherState {
    #[default]
    Unaffected = 0,
    Exposed = 1,
    Weathered = 2,
    Oxidized = 3,
}

impl WeatherState {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::Exposed,
            2 => Self::Weathered,
            3 => Self::Oxidized,
            _ => Self::Unaffected,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    // WeatherState.getSerializedName
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unaffected => "unaffected",
            Self::Exposed => "exposed",
            Self::Weathered => "weathered",
            Self::Oxidized => "oxidized",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "unaffected" => Some(Self::Unaffected),
            "exposed" => Some(Self::Exposed),
            "weathered" => Some(Self::Weathered),
            "oxidized" => Some(Self::Oxidized),
            _ => None,
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Unaffected => Self::Exposed,
            Self::Exposed => Self::Weathered,
            Self::Weathered | Self::Oxidized => Self::Oxidized,
        }
    }

    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Oxidized => Self::Weathered,
            Self::Weathered => Self::Exposed,
            Self::Exposed | Self::Unaffected => Self::Unaffected,
        }
    }

    // copper golem: WeatherState from the weathering copper block system
    #[must_use]
    pub const fn from_block_state(state: BlockWeatherState) -> Self {
        match state {
            BlockWeatherState::Unaffected => Self::Unaffected,
            BlockWeatherState::Exposed => Self::Exposed,
            BlockWeatherState::Weathered => Self::Weathered,
            BlockWeatherState::Oxidized => Self::Oxidized,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum CopperGolemState {
    #[default]
    Idle = 0,
    GettingItem = 1,
    GettingNoItem = 2,
    DroppingItem = 3,
    DroppingNoItem = 4,
}

impl CopperGolemState {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::GettingItem,
            2 => Self::GettingNoItem,
            3 => Self::DroppingItem,
            4 => Self::DroppingNoItem,
            _ => Self::Idle,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }
}

const MEMORIES: [MemoryModuleId; 0] = [];

/// Represents a Copper Golem, a passive creation mob made of copper blocks and lightning rod.
///
/// Wiki: <https://minecraft.wiki/w/Copper_Golem>
pub struct CopperGolemEntity {
    pub mob_entity: MobEntity,
    pub weather_state: AtomicI32,
    pub state: AtomicI32,
    pub next_weathering_tick: AtomicI64,
    opened_chest_pos: AtomicCell<Option<BlockPos>>,
    opened_container: Mutex<Option<Arc<dyn Inventory>>>,
    last_lightning_bolt: AtomicCell<Option<Uuid>>,
}

impl CopperGolemEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mut navigator = mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator.set_required_path_length(REQUIRED_PATH_LENGTH);
        navigator.set_can_open_doors(true);
        // copper golem: setPathfindingMalus FIRE_IN_NEIGHBOR, DAMAGING_IN_NEIGHBOR, FIRE
        navigator.set_pathfinding_malus(PathType::DangerFire, 16.0);
        navigator.set_pathfinding_malus(PathType::DangerOther, 16.0);
        navigator.set_pathfinding_malus(PathType::DamageFire, -1.0);
        drop(navigator);
        mob_entity
            .persistence_required
            .store(true, Ordering::Relaxed);
        let golem = Arc::new(Self {
            mob_entity,
            weather_state: AtomicI32::new(WeatherState::Unaffected.id()),
            state: AtomicI32::new(CopperGolemState::Idle.id()),
            next_weathering_tick: AtomicI64::new(UNSET_WEATHERING_TICK),
            opened_chest_pos: AtomicCell::new(None),
            opened_container: Mutex::new(None),
            last_lightning_bolt: AtomicCell::new(None),
        });
        let cooldown = golem
            .get_random()
            .random_range(SPAWN_COOLDOWN_MIN..SPAWN_COOLDOWN_MAX);
        golem.mob_entity.with_brain(golem.as_ref(), |brain| {
            brain.set(types::TRANSPORT_ITEMS_COOLDOWN_TICKS, cooldown);
        });
        golem
    }

    #[must_use]
    pub fn get_weather_state(&self) -> WeatherState {
        WeatherState::from_id(self.weather_state.load(Ordering::Relaxed))
    }

    pub fn set_weather_state(&self, state: WeatherState) {
        self.weather_state.store(state.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::copper_golem::WEATHER_STATE,
            VarInt(state.id()),
        );
    }

    #[must_use]
    pub fn get_state(&self) -> CopperGolemState {
        CopperGolemState::from_id(self.state.load(Ordering::Relaxed))
    }

    pub fn set_state(&self, state: CopperGolemState) {
        self.state.store(state.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::copper_golem::COPPER_GOLEM_STATE,
            VarInt(state.id()),
        );
    }

    // copper golem: spawn(weatherState)
    pub fn spawn(&self, weather_state: WeatherState) {
        self.set_weather_state(weather_state);
        self.play_spawn_sound();
    }

    pub fn play_spawn_sound(&self) {
        self.play_entity_sound(Sound::EntityCopperGolemSpawn, 1.0, 1.0);
    }

    #[must_use]
    pub fn step_sound(&self) -> Sound {
        match self.get_weather_state() {
            WeatherState::Unaffected | WeatherState::Exposed => Sound::EntityCopperGolemStep,
            WeatherState::Weathered => Sound::EntityCopperGolemWeatheredStep,
            WeatherState::Oxidized => Sound::EntityCopperGolemOxidizedStep,
        }
    }

    // copper golem: ContainerUser, opens the chest for the lid animation and the queue
    fn start_open(&self, container: &Arc<dyn Inventory>, pos: BlockPos) {
        let mut opened = self
            .opened_container
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if opened.is_none() {
            container.on_open();
            *opened = Some(container.clone());
        }
        self.opened_chest_pos.store(Some(pos));
    }

    fn stop_open(&self) {
        let opened = self
            .opened_container
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(container) = opened {
            container.on_close();
        }
        self.opened_chest_pos.store(None);
    }

    // copper golem: readyForShearing
    fn ready_for_shearing(&self) -> bool {
        self.antenna()
            .item
            .has_tag(&tag::Item::MINECRAFT_SHEARABLE_FROM_COPPER_GOLEM)
    }

    fn antenna(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::SADDLE)
    }

    // copper golem: shear
    fn shear(&self) {
        let entity = self.get_entity();
        entity.world.load().play_sound(
            Sound::EntityCopperGolemShear,
            SoundCategory::Players,
            &entity.pos.load(),
        );
        let antenna = self
            .mob_entity
            .set_item_slot(&EquipmentSlot::SADDLE, ItemStack::EMPTY.clone());
        self.spawn_at_height(antenna, ANTENNA_DROP_HEIGHT);
    }

    // entity: spawnAtLocation(level, stack, yOffset)
    fn spawn_at_height(&self, stack: ItemStack, y_offset: f64) {
        if stack.is_empty() {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let item = ItemEntity::new(
            Entity::new(
                world.clone(),
                Vector3::new(pos.x, pos.y + y_offset, pos.z),
                &EntityType::ITEM,
            ),
            stack,
        );
        world.spawn_entity(Arc::new(item));
    }

    // behavior utils: BehaviorUtils.throwItem
    fn throw_item(&self, stack: ItemStack, target: Vector3<f64>) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let item = ItemEntity::new(
            Entity::new(
                world.clone(),
                Vector3::new(pos.x, entity.get_eye_y() - HAND_Y_DISTANCE_FROM_EYE, pos.z),
                &EntityType::ITEM,
            ),
            stack,
        );
        item.set_thrower(entity.entity_uuid);
        item.get_entity()
            .velocity
            .store((target - pos).normalize().multiply(
                THROW_VELOCITY,
                THROW_VELOCITY,
                THROW_VELOCITY,
            ));
        world.spawn_entity(Arc::new(item));
    }

    // copper golem: updateWeathering
    fn update_weathering(&self, game_time: i64) {
        let next = self.next_weathering_tick.load(Ordering::Relaxed);
        if next == IGNORE_WEATHERING_TICK {
            return;
        }
        let mut rng = self.get_random();
        let interval = |rng: &mut rand::rngs::ThreadRng| {
            i64::from(rng.random_range(WEATHERING_TICK_FROM..=WEATHERING_TICK_TO))
        };
        if next == UNSET_WEATHERING_TICK {
            self.next_weathering_tick
                .store(game_time + interval(&mut rng), Ordering::Relaxed);
            return;
        }
        let weather = self.get_weather_state();
        let fully_oxidized = weather == WeatherState::Oxidized;
        if game_time >= next && !fully_oxidized {
            let new_state = weather.next();
            self.set_weather_state(new_state);
            self.next_weathering_tick.store(
                if new_state == WeatherState::Oxidized {
                    0
                } else {
                    next + interval(&mut rng)
                },
                Ordering::Relaxed,
            );
        }
        if fully_oxidized && self.can_turn_to_statue() {
            self.turn_to_statue();
        }
    }

    // copper golem: canTurnToStatue
    fn can_turn_to_statue(&self) -> bool {
        let entity = self.get_entity();
        entity
            .world
            .load()
            .get_block_state(&entity.block_pos.load())
            .is_air()
            && self.get_random().random::<f32>() <= TURN_TO_STATUE_CHANCE
    }

    // copper golem: turnToStatue
    fn turn_to_statue(&self) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.block_pos.load();
        let mut props =
            CopperGolemStatueLikeProperties::default(&Block::OXIDIZED_COPPER_GOLEM_STATUE);
        props.copper_golem_pose = EnumVariants::from_index(self.get_random().random_range(0..4));
        props.facing = entity.get_horizontal_facing();
        world.set_block_state(
            &pos,
            props.to_state_id(&Block::OXIDIZED_COPPER_GOLEM_STATUE),
            pumpkin_world::world::BlockFlags::NOTIFY_ALL,
        );
        let Some(block_entity) = world.get_block_entity(&pos) else {
            return;
        };
        if let Some(statue) = block_entity
            .as_any()
            .downcast_ref::<CopperGolemStatueBlockEntity>()
        {
            // statue: createStatue keeps the custom name
            let name = (*entity.custom_name.load_full())
                .clone()
                .map(TextComponent::get_text);
            *statue
                .custom_name
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = name;
        }
        self.drop_preserved_equipment();
        let leashed = entity.is_leashed();
        self.play_entity_sound(Sound::EntityCopperGolemBecomeStatue, 1.0, 1.0);
        if leashed {
            // dropLeash with the entityDrops rule, else removeLeash
            entity.unleash();
        }
        entity.remove();
    }

    // mob: dropPreservedEquipment, slots whose drop chance is preserved
    fn drop_preserved_equipment(&self) {
        self.mob_entity.drop_preserved_equipment();
    }
}

impl CustomSound for CopperGolemEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        Some(match self.get_weather_state() {
            WeatherState::Unaffected | WeatherState::Exposed => Sound::EntityCopperGolemHurt,
            WeatherState::Weathered => Sound::EntityCopperGolemWeatheredHurt,
            WeatherState::Oxidized => Sound::EntityCopperGolemOxidizedHurt,
        })
    }

    fn death_sound(&self) -> Option<Sound> {
        Some(match self.get_weather_state() {
            WeatherState::Unaffected | WeatherState::Exposed => Sound::EntityCopperGolemDeath,
            WeatherState::Weathered => Sound::EntityCopperGolemWeatheredDeath,
            WeatherState::Oxidized => Sound::EntityCopperGolemOxidizedDeath,
        })
    }
}

// copper golem: CopperGolemAi.getTargetReachedInteractions
fn on_reached_target_interaction(
    mob: &dyn Mob,
    target: &TransportItemTarget,
    ticks_since_reaching_target: i32,
    state: CopperGolemState,
    sound: Sound,
) {
    let Some(golem) = mob.cast_any().downcast_ref::<CopperGolemEntity>() else {
        return;
    };
    if ticks_since_reaching_target == TICK_TO_START_ON_REACHED_INTERACTION {
        golem.start_open(&target.container, target.pos);
        golem.set_state(state);
    }
    if ticks_since_reaching_target == TICK_TO_PLAY_ON_REACHED_SOUND {
        golem.play_entity_sound(sound, 1.0, 1.0);
    }
    if ticks_since_reaching_target == CONTAINER_CLOSE_TICK {
        golem.stop_open();
    }
}

fn on_pickup_item(mob: &dyn Mob, target: &TransportItemTarget, ticks: i32) {
    on_reached_target_interaction(
        mob,
        target,
        ticks,
        CopperGolemState::GettingItem,
        Sound::EntityCopperGolemNoItemGet,
    );
}

fn on_pickup_no_item(mob: &dyn Mob, target: &TransportItemTarget, ticks: i32) {
    on_reached_target_interaction(
        mob,
        target,
        ticks,
        CopperGolemState::GettingNoItem,
        Sound::EntityCopperGolemNoItemNoGet,
    );
}

fn on_place_item(mob: &dyn Mob, target: &TransportItemTarget, ticks: i32) {
    on_reached_target_interaction(
        mob,
        target,
        ticks,
        CopperGolemState::DroppingItem,
        Sound::EntityCopperGolemItemDrop,
    );
}

fn on_place_no_item(mob: &dyn Mob, target: &TransportItemTarget, ticks: i32) {
    on_reached_target_interaction(
        mob,
        target,
        ticks,
        CopperGolemState::DroppingNoItem,
        Sound::EntityCopperGolemItemNoDrop,
    );
}

// copper golem: CopperGolemAi.onTravelling
fn on_travelling(mob: &dyn Mob) {
    if let Some(golem) = mob.cast_any().downcast_ref::<CopperGolemEntity>() {
        golem.stop_open();
        golem.set_state(CopperGolemState::Idle);
    }
}

// copper golem: CopperGolemAi.shouldQueueForTarget
fn should_queue_for_target(target: &TransportItemTarget) -> bool {
    let any = target.block_entity.as_any();
    any.downcast_ref::<ChestBlockEntity>()
        .map(ChestBlockEntity::get_viewer_count)
        .or_else(|| {
            any.downcast_ref::<TrappedChestBlockEntity>()
                .map(TrappedChestBlockEntity::get_viewer_count)
        })
        .is_some_and(|viewers| viewers > 0)
}

fn is_transport_source(block: &Block) -> bool {
    block.has_tag(&tag::Block::MINECRAFT_COPPER_CHESTS)
}

fn is_transport_destination(block: &Block) -> bool {
    block == &Block::CHEST || block == &Block::TRAPPED_CHEST
}

fn make_brain(golem: &CopperGolemEntity, packed: &PackedMemories) -> Brain {
    BrainProvider {
        memory_types: &MEMORIES,
        sensor_types: &[SensorType::NearestLivingEntities, SensorType::HurtBy],
        activities,
    }
    .make_brain(golem, packed)
}

// copper golem: CopperGolemAi.getActivities
fn activities(_mob: &dyn Mob) -> Vec<ActivityData> {
    vec![
        ActivityData::with_priority_start(
            Activity::Core,
            0,
            vec![
                Box::new(Timed::new(AnimalPanic::new(
                    SPEED_MULTIPLIER_WHEN_PANICKING,
                ))),
                Box::new(Timed::new(LookAtTargetSink::new(45, 90))),
                Box::new(Timed::new(MoveToTargetSink::default())),
                Box::new(interact_with_door::create()),
                Box::new(CountDownCooldownTicks::new(types::GAZE_COOLDOWN_TICKS)),
                Box::new(CountDownCooldownTicks::new(
                    types::TRANSPORT_ITEMS_COOLDOWN_TICKS,
                )),
            ],
        ),
        ActivityData::with_pairs(
            Activity::Idle,
            vec![
                (
                    0,
                    Box::new(Timed::new(TransportItemsBetweenContainers::new(
                        SPEED_MULTIPLIER_WHEN_IDLING,
                        is_transport_source,
                        is_transport_destination,
                        TRANSPORT_ITEM_HORIZONTAL_SEARCH_RADIUS,
                        TRANSPORT_ITEM_VERTICAL_SEARCH_RADIUS,
                        [
                            (ContainerInteractionState::PickupItem, on_pickup_item),
                            (ContainerInteractionState::PickupNoItem, on_pickup_no_item),
                            (ContainerInteractionState::PlaceItem, on_place_item),
                            (ContainerInteractionState::PlaceNoItem, on_place_no_item),
                        ],
                        on_travelling,
                        should_queue_for_target,
                    ))),
                ),
                (
                    1,
                    Box::new(set_entity_look_target_sometimes::create_for_type(
                        &EntityType::PLAYER,
                        6.0,
                        40,
                        80,
                    )),
                ),
                (
                    2,
                    Box::new(run_one_with_conditions(
                        vec![
                            (types::WALK_TARGET.id(), MemoryStatus::ValueAbsent),
                            (
                                types::TRANSPORT_ITEMS_COOLDOWN_TICKS.id(),
                                MemoryStatus::ValuePresent,
                            ),
                        ],
                        vec![
                            (
                                Box::new(random_stroll::stroll_in_range(
                                    SPEED_MULTIPLIER_WHEN_IDLING,
                                    2,
                                    2,
                                )),
                                1,
                            ),
                            (Box::new(DoNothing::new(30, 60)), 1),
                        ],
                    )),
                ),
            ],
        ),
    ]
}

impl Mob for CopperGolemEntity {
    fn mob_play_step_sound(
        &self,
        _pos: &pumpkin_util::math::position::BlockPos,
        _state: &BlockState,
    ) {
        self.play_entity_sound(self.step_sound(), 1.0, 1.0);
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn make_brain(&self, packed: &PackedMemories) -> Brain {
        make_brain(self, packed)
    }

    // copper golem: customServerAiStep (copperGolemBrain, CopperGolemAi.updateActivity)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, |brain| {
            brain.set_active_activity_to_first_valid(&[Activity::Idle]);
        });
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let world = self.get_entity().world.load();
        self.update_weathering(world.get_world_age());
    }

    fn on_removed(&self) {
        self.stop_open();
    }

    // copper golem: actuallyHurt resets the animation state
    fn on_damage(
        &self,
        _damage_type: pumpkin_data::damage::DamageType,
        _source: Option<&dyn EntityBase>,
    ) {
        self.set_state(CopperGolemState::Idle);
    }

    // copper golem: finalizeSpawn plays the spawn sound
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<crate::entity::mob::spawn::SpawnGroupData>,
    ) -> Option<crate::entity::mob::spawn::SpawnGroupData> {
        self.play_spawn_sound();
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_long(
            "next_weather_age",
            self.next_weathering_tick.load(Ordering::Relaxed),
        );
        nbt.put_string("weather_state", self.get_weather_state().name().to_string());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.next_weathering_tick.store(
            nbt.get_long("next_weather_age")
                .unwrap_or(UNSET_WEATHERING_TICK),
            Ordering::Relaxed,
        );
        self.set_weather_state(
            nbt.get_string("weather_state")
                .and_then(WeatherState::from_name)
                .unwrap_or_default(),
        );
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // copper golem: thunderHit, a new bolt scrapes one oxidation level off
    fn mob_on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        self.mob_entity
            .living_entity
            .on_lightning_strike(caller, lightning);
        let bolt = lightning.get_entity().entity_uuid;
        if self.last_lightning_bolt.load() != Some(bolt) {
            self.last_lightning_bolt.store(Some(bolt));
            let weather = self.get_weather_state();
            if weather != WeatherState::Unaffected {
                self.next_weathering_tick
                    .store(UNSET_WEATHERING_TICK, Ordering::Relaxed);
                self.set_weather_state(weather.previous());
            }
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::copper_golem::WEATHER_STATE,
            VarInt(self.get_weather_state().id()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::copper_golem::COPPER_GOLEM_STATE,
            VarInt(self.get_state().id()),
        );
    }

    // copper golem: mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let entity = self.get_entity();
        let world = entity.world.load();
        if item_stack.is_empty() {
            let held = self.mob_entity.with_main_hand(Clone::clone);
            if !held.is_empty() {
                self.throw_item(held, player.get_entity().pos.load());
                self.mob_entity
                    .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::EMPTY.clone());
                return true;
            }
        }
        let item = item_stack.item;
        if item == &Item::SHEARS && self.ready_for_shearing() {
            self.shear();
            world.emit_game_event(GameEvent::Shear.name(), entity.pos.load());
            let _ = player.damage_item_stack(item_stack, 1);
            return true;
        }
        let next = self.next_weathering_tick.load(Ordering::Relaxed);
        let pos = entity.block_pos.load();
        if item == &Item::HONEYCOMB && next != IGNORE_WEATHERING_TICK {
            world.sync_world_event(WorldEvent::ParticlesWaxOn, pos, 0);
            world.play_sound(
                Sound::ItemHoneycombWaxOn,
                SoundCategory::Blocks,
                &pos.to_f64(),
            );
            self.next_weathering_tick
                .store(IGNORE_WEATHERING_TICK, Ordering::Relaxed);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            return true;
        }
        if item.has_tag(&tag::Item::MINECRAFT_AXES) {
            if next == IGNORE_WEATHERING_TICK {
                world.play_sound(
                    Sound::ItemAxeScrape,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
                world.sync_world_event(WorldEvent::ParticlesWaxOff, pos, 0);
                self.next_weathering_tick
                    .store(UNSET_WEATHERING_TICK, Ordering::Relaxed);
                let _ = player.damage_item_stack(item_stack, 1);
                return true;
            }
            let weather = self.get_weather_state();
            if weather != WeatherState::Unaffected {
                world.play_sound(
                    Sound::ItemAxeScrape,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
                world.sync_world_event(WorldEvent::ParticlesScrape, pos, 0);
                self.next_weathering_tick
                    .store(UNSET_WEATHERING_TICK, Ordering::Relaxed);
                self.set_weather_state(weather.previous());
                let _ = player.damage_item_stack(item_stack, 1);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_state_walks_the_oxidation_chain_and_saves_by_name() {
        assert_eq!(WeatherState::Unaffected.next(), WeatherState::Exposed);
        assert_eq!(WeatherState::Weathered.next(), WeatherState::Oxidized);
        assert_eq!(WeatherState::Oxidized.next(), WeatherState::Oxidized);
        assert_eq!(WeatherState::Exposed.previous(), WeatherState::Unaffected);
        assert_eq!(
            WeatherState::from_name(WeatherState::Weathered.name()),
            Some(WeatherState::Weathered)
        );
    }
}
