use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::sulfur_cube_archetype::{
    ContactDamage, DEFAULT_KNOCKBACK_MODIFIERS, DEFAULT_SOUND_SETTINGS, ExplosionData,
    KnockbackModifiers, ModifierOperation as ArchetypeOperation, SULFUR_CUBE_ARCHETYPES,
    SoundSettings, SulfurCubeArchetype,
};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use super::sulfur_cube_ai::{
    CubeFloatGoal, CubeKeepOnJumpingGoal, CubeRandomDirectionGoal, CubeSearchForItemsGoal,
    CubeTemptGoal, SulfurCubeLookControl, SulfurCubeMoveControl,
};
use crate::entity::ageable::{AgeableData, AgeableMob};
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::mob::{Mob, MobEntity};
use crate::entity::player::Player;
use crate::entity::projectile::arrow::ArrowEntity;
use crate::entity::tnt::TNTEntity;
use crate::entity::{Entity, EntityBase};
use crate::world::{ExplosionInteraction, World};

const MIN_SIZE: i32 = 1;
const MAX_SIZE: i32 = 2;
const SPLIT_COUNT: i32 = 2;
const PICKUP_TIMER_DURATION: i32 = 100;
const PUSH_DISTANCE_THRESHOLD: f64 = 1.3f32 as f64;
const MAX_PLAYER_PUSH_SPEED: f64 = 0.5;
const PLAYER_PUSH_SPEED_SCALE_MULTIPLIER: f32 = 0.3;
const VEHICLE_PUSH_SPEED_SCALE_MULTIPLIER: f32 = 0.16;
const VERTICAL_PUSH_MULTIPLIER: f32 = 0.3;
const HORIZONTAL_HIT_ANGLE_SCALE: f32 = 1.6;
const VERTICAL_HIT_ANGLE_SCALE: f32 = 0.5;
const VERTICAL_POSITION_ANGLE_SCALE: f32 = 0.8;
const EXTRA_KNOCKBACK_DAMPENING: f32 = 0.25;
const HORIZONTAL_KNOCKBACK_SCALE: f32 = 0.4;
const VERTICAL_KNOCKBACK_SCALE: f64 = 1.2;
const MAX_KNOCKBACK_POWER: f32 = 128.0;
const NO_FUSE: i32 = -1;
const EXPLOSION_Y_OFFSET: f64 = 0.0625;
const DEFAULT_KNOCKBACK_POWER: f64 = 0.4;

// sulfur cube: state derived from the swallowed block (collectEquipmentChanges)
struct BodyEffects {
    item_id: Option<u16>,
    floats_in_liquids: bool,
    explosion: Option<ExplosionData>,
    contact_damages: Vec<ContactDamage>,
    knockback: KnockbackModifiers,
    sound: SoundSettings,
}

impl BodyEffects {
    const fn new() -> Self {
        Self {
            item_id: None,
            floats_in_liquids: false,
            explosion: None,
            contact_damages: Vec::new(),
            knockback: DEFAULT_KNOCKBACK_MODIFIERS,
            sound: DEFAULT_SOUND_SETTINGS,
        }
    }
}

pub struct SulfurCubeEntity {
    mob_entity: MobEntity,
    ageable_data: AgeableData,
    jump_delay: AtomicI32,
    target_yaw: AtomicCell<f32>,
    is_aggressive: AtomicBool,
    // MoveControl.operation == MOVE_TO with this speed modifier
    wanted_movement: AtomicCell<Option<f64>>,
    was_on_ground: AtomicBool,
    squish: AtomicCell<f32>,
    target_squish: AtomicCell<f32>,
    o_squish: AtomicCell<f32>,
    pickup_timer: AtomicI32,
    push_sound_cooldown: AtomicI32,
    fuse: AtomicI32,
    from_bucket: AtomicBool,
    has_split: AtomicBool,
    body: Mutex<BodyEffects>,
}

impl SulfurCubeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let cube = Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            jump_delay: AtomicI32::new(0),
            target_yaw: AtomicCell::new(0.0),
            is_aggressive: AtomicBool::new(false),
            wanted_movement: AtomicCell::new(None),
            was_on_ground: AtomicBool::new(false),
            squish: AtomicCell::new(0.0),
            target_squish: AtomicCell::new(0.0),
            o_squish: AtomicCell::new(0.0),
            pickup_timer: AtomicI32::new(0),
            push_sound_cooldown: AtomicI32::new(0),
            fuse: AtomicI32::new(NO_FUSE),
            from_bucket: AtomicBool::new(false),
            has_split: AtomicBool::new(false),
            body: Mutex::new(BodyEffects::new()),
        });
        let weak = Arc::downgrade(&cube);
        *cube
            .mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(SulfurCubeMoveControl::new(weak.clone()));
        *cube
            .mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(SulfurCubeLookControl::new());
        // cube mob: CubeMobFloatGoal setCanFloat
        cube.mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_float(true);

        let goals = &cube.mob_entity.goals_selector;
        // cube mob: registerGoals, addBehaviourGoals
        goals.add_goal(1, Box::new(CubeFloatGoal::new(weak.clone())));
        goals.add_goal(2, Box::new(CubeTemptGoal::new(weak.clone())));
        goals.add_goal(3, Box::new(CubeSearchForItemsGoal::new(weak.clone())));
        goals.add_goal(4, Box::new(CubeRandomDirectionGoal::new(weak.clone())));
        goals.add_goal(5, Box::new(CubeKeepOnJumpingGoal::new(weak)));
        // cube mob: ID_SIZE defaults to 1
        cube.set_size(MIN_SIZE, false);
        cube
    }

    // cube mob: setSize
    pub fn set_size(&self, size: i32, update_health: bool) {
        let actual_size = size.clamp(MIN_SIZE, 127);
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.data.store(actual_size, Ordering::Relaxed);
        living.set_attribute_base(&Attributes::MAX_HEALTH, f64::from(4 * actual_size));
        living.set_attribute_base(
            &Attributes::MOVEMENT_SPEED,
            f64::from(0.2f32 + 0.1f32 * actual_size as f32),
        );
        entity.set_default_dimensions(Self::dimensions_for_size(entity, actual_size));
        entity.set_synced_data(tracked_data::sulfur_cube::ID_SIZE, actual_size);
        if update_health {
            living.set_health(self.mob_entity.living_entity.get_max_health());
        }
        // sulfur cube: setSize turns a size 1 cube into a baby
        if update_health && actual_size == MIN_SIZE && !AgeableMob::is_baby(self) {
            self.set_baby(true);
        }
        if let Some(meta) = self.mob_metadata() {
            entity.send_bedrock_actor_data(&meta.to_bedrock_actor_data());
        }
    }

    fn dimensions_for_size(entity: &Entity, size: i32) -> EntityDimensions {
        let base = Entity::type_dimensions(entity.entity_type);
        EntityDimensions {
            width: base.width * size as f32,
            height: base.height * size as f32,
            eye_height: base.eye_height * size as f32,
        }
    }

    #[must_use]
    pub fn get_size(&self) -> i32 {
        self.get_entity().data.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_tiny(&self) -> bool {
        self.get_size() <= MIN_SIZE
    }

    #[must_use]
    pub fn is_baby_cube(&self) -> bool {
        AgeableMob::is_baby(self)
    }

    #[must_use]
    pub fn get_fuse(&self) -> i32 {
        self.fuse.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_primed(&self) -> bool {
        self.get_fuse() >= 0
    }

    #[must_use]
    pub fn pickup_timer(&self) -> i32 {
        self.pickup_timer.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_from_bucket(&self) -> bool {
        self.from_bucket.load(Ordering::Relaxed)
    }

    pub fn set_from_bucket(&self, from_bucket: bool) {
        self.from_bucket.store(from_bucket, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked_data::sulfur_cube::FROM_BUCKET, from_bucket);
    }

    fn set_fuse(&self, fuse: i32) {
        self.fuse.store(fuse, Ordering::Relaxed);
    }

    fn body_stack(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::BODY)
    }

    // sulfur cube: hasBodyItem
    #[must_use]
    pub fn has_body_item(&self) -> bool {
        !self.body_stack().is_empty()
    }

    fn effects(&self) -> std::sync::MutexGuard<'_, BodyEffects> {
        self.body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // sulfur cube: canExplode
    #[must_use]
    pub fn can_explode(&self) -> bool {
        self.effects().explosion.is_some() && self.get_entity().is_alive() && !self.is_primed()
    }

    // sulfur cube: isSwallowableItem
    #[must_use]
    pub fn is_swallowable_item(stack: &ItemStack) -> bool {
        !stack.is_empty()
            && stack
                .item
                .has_tag(&tag::Item::MINECRAFT_SULFUR_CUBE_SWALLOWABLE)
    }

    fn is_food(stack: &ItemStack) -> bool {
        stack.item.has_tag(&tag::Item::MINECRAFT_SULFUR_CUBE_FOOD)
    }

    // sulfur cube: matchingArchetypes
    fn matching_archetypes(
        stack: &ItemStack,
    ) -> impl Iterator<Item = &'static SulfurCubeArchetype> {
        let item = (!stack.is_empty()).then_some(stack.item);
        SULFUR_CUBE_ARCHETYPES
            .iter()
            .filter(move |archetype| item.is_some_and(|item| item.has_tag(archetype.items)))
    }

    // sulfur cube: collectEquipmentChanges, swaps the archetype attribute modifiers
    fn sync_body_effects(&self) {
        let current = self.body_stack();
        let current_id = (!current.is_empty()).then_some(current.item.id);
        let mut effects = self.effects();
        if effects.item_id == current_id {
            return;
        }
        let previous = effects.item_id.and_then(Item::from_id);
        let living = &self.mob_entity.living_entity;
        {
            let mut attributes = living
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(previous) = previous {
                for archetype in Self::matching_archetypes(&ItemStack::new(1, previous)) {
                    for modifier in archetype.attribute_modifiers {
                        if let Some(instance) = attributes.get_mut(&modifier.attribute.id) {
                            instance.remove_modifier(modifier.id);
                        }
                    }
                }
            }
        }
        *effects = BodyEffects::new();
        effects.item_id = current_id;
        let mut attributes = living
            .attributes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for archetype in Self::matching_archetypes(&current) {
            if archetype.buoyant {
                effects.floats_in_liquids = true;
            }
            if archetype.explosion.is_some() {
                effects.explosion = archetype.explosion;
            }
            if let Some(contact) = archetype.contact_damage {
                effects.contact_damages.push(contact);
            }
            effects.knockback = archetype.knockback_modifiers;
            effects.sound = archetype.sound_settings;
            for modifier in archetype.attribute_modifiers {
                if let Some(instance) = attributes.get_mut(&modifier.attribute.id) {
                    instance.add_or_replace_modifier(Modifier {
                        id: modifier.id.to_string(),
                        amount: modifier.amount,
                        operation: match modifier.operation {
                            ArchetypeOperation::Add => ModifierOperation::Add,
                            ArchetypeOperation::MultiplyBase => ModifierOperation::MultiplyBase,
                            ArchetypeOperation::MultiplyTotal => ModifierOperation::MultiplyTotal,
                        },
                    });
                }
            }
        }
        drop(attributes);
        drop(effects);
        if current_id.is_some() {
            // sulfur cube: removeAllGoals, setSpeed(0)
            self.mob_entity.set_speed(0.0);
        }
    }

    // cube mob: move control state used by the goals
    pub(super) fn set_wanted_movement(&self, speed_modifier: f64) {
        self.wanted_movement.store(Some(speed_modifier));
    }

    pub(super) fn set_wait(&self) {
        self.wanted_movement.store(None);
    }

    pub(super) fn has_wanted_movement(&self) -> bool {
        self.wanted_movement.load().is_some()
    }

    pub(super) fn take_wanted_movement(&self) -> Option<f64> {
        self.wanted_movement.take()
    }

    pub(super) fn target_yaw(&self) -> f32 {
        self.target_yaw.load()
    }

    pub(super) fn is_aggressive(&self) -> bool {
        self.is_aggressive.load(Ordering::Relaxed)
    }

    // cube mob: CubeMobMoveControl.setDirection
    pub(super) fn set_direction(&self, yaw: f32, aggressive: bool) {
        self.target_yaw.store(yaw);
        self.is_aggressive.store(aggressive, Ordering::Relaxed);
    }

    pub(super) fn set_direction_to_yaw(&self, aggressive: bool) {
        self.set_direction(self.get_entity().yaw.load(), aggressive);
    }

    // cube mob: jumpDelay-- <= 0
    pub(super) fn tick_jump_delay(&self) -> bool {
        self.jump_delay.fetch_sub(1, Ordering::Relaxed) <= 0
    }

    pub(super) fn set_jump_delay(&self, delay: i32) {
        self.jump_delay.store(delay, Ordering::Relaxed);
    }

    // cube mob: getJumpDelay
    pub(super) fn get_jump_delay(&self) -> i32 {
        self.get_random().random_range(0..20) + 10
    }

    pub(super) fn jump_now(&self) {
        self.mob_entity
            .jump_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jump();
    }

    pub(super) fn look_at_entity(&self, target: &dyn EntityBase, step: f32) {
        self.mob_entity.look_at_entity_body(target, step, step);
    }

    // cube mob: getSoundVolume
    fn sound_volume(&self) -> f32 {
        0.4 * self.get_size() as f32
    }

    // cube mob: getSoundPitch
    fn sound_pitch(&self) -> f32 {
        let pitch_adjuster = if self.is_tiny() { 1.4 } else { 0.8 };
        let mut rng = self.get_random();
        (rng.random::<f32>() - rng.random::<f32>()).mul_add(0.2, 1.0) * pitch_adjuster
    }

    // cube mob: doPlayJumpSound
    pub(super) fn play_jump_sound(&self) {
        if self.get_size() > 0 {
            self.play_sound_at(self.jump_sound(), self.sound_volume(), self.sound_pitch());
        }
    }

    fn play_sound_at(&self, sound: Sound, volume: f32, pitch: f32) {
        let entity = self.get_entity();
        if entity.is_silent() {
            return;
        }
        entity.world.load().play_sound_fine(
            sound,
            SoundCategory::Neutral,
            &entity.pos.load(),
            volume,
            pitch,
        );
    }

    fn jump_sound(&self) -> Sound {
        if self.is_tiny() {
            Sound::EntitySmallSulfurCubeJump
        } else {
            Sound::EntitySulfurCubeJump
        }
    }

    fn squish_sound(&self) -> Sound {
        if self.is_tiny() {
            Sound::EntitySmallSulfurCubeSquish
        } else if self.has_body_item() {
            Sound::EntitySulfurCubeBounce
        } else {
            Sound::EntitySulfurCubeSquish
        }
    }

    // sulfur cube: tickFuse
    fn tick_fuse(&self) {
        let fuse = self.get_fuse();
        if fuse > 0 {
            self.set_fuse(fuse - 1);
        }
        let Some(explosion) = self.effects().explosion else {
            return;
        };
        if self.get_fuse() != 0 {
            return;
        }
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.unleash();
        living.dead.store(true, Ordering::Relaxed);
        let world = entity.world.load_full();
        if world.level_info.load().game_rules.tnt_explodes {
            let interaction = if world.level_info.load().game_rules.mob_griefing {
                ExplosionInteraction::Tnt
            } else {
                ExplosionInteraction::None
            };
            let pos = entity.pos.load();
            let explosion_pos = Vector3::new(
                pos.x,
                f64::from(entity.entity_dimension.load().height).mul_add(EXPLOSION_Y_OFFSET, pos.y),
                pos.z,
            );
            let power = explosion.power as f32;
            match world.get_entity_by_id(entity.entity_id) {
                Some(source) => {
                    world.explode_from_entity(source, explosion_pos, power, interaction);
                }
                None => world.explode(explosion_pos, power, interaction),
            }
            living.trigger_on_death_mob_effects(&DamageType::GENERIC_KILL);
        }
        entity.remove();
    }

    // sulfur cube: primeWhenOnPoweredPosition
    fn prime_when_on_powered_position(&self) {
        if !self.can_explode() {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        if crate::block::blocks::redstone::block_receives_redstone_power(
            &world,
            &entity.block_pos.load(),
        ) {
            self.prime_time(false);
        }
    }

    // sulfur cube: primeTime
    pub fn prime_time(&self, imminent: bool) -> bool {
        let Some(explosion) = self.effects().explosion else {
            return false;
        };
        let entity = self.get_entity();
        let world = entity.world.load();
        if !entity.is_alive()
            || !world.level_info.load().game_rules.tnt_explodes
            || self.is_primed()
        {
            return false;
        }
        let fuse = explosion.fuse;
        let fuse_time = if imminent {
            TNTEntity::random_short_fuse(fuse as u32) as i32
        } else {
            fuse
        };
        entity.set_invulnerable(true);
        self.set_fuse(fuse_time);
        entity.set_synced_data(tracked_data::sulfur_cube::MAX_FUSE, fuse_time);
        self.play_sound_at(Sound::EntityTntPrimed, 1.0, 1.0);
        world.emit_game_event(GameEvent::PrimeFuse.name(), entity.pos.load());
        true
    }

    // sulfur cube: equipItem
    pub fn equip_item(&self, held: &ItemStack) -> bool {
        if self.is_baby_cube() {
            return false;
        }
        let swallowed = self.body_stack();
        if !swallowed.is_empty() {
            if held.item.id == swallowed.item.id {
                return false;
            }
            self.get_entity().spawn_at_location(swallowed);
        }
        let mut single = held.clone();
        single.item_count = 1;
        self.mob_entity.set_item_slot(&EquipmentSlot::BODY, single);
        self.mob_entity.set_guaranteed_drop(&EquipmentSlot::BODY);
        self.sync_body_effects();
        self.play_sound_at(Sound::EntitySulfurCubeAbsorb, 1.0, 1.0);
        true
    }

    // sulfur cube: shear
    pub fn shear(&self) {
        let body = self.body_stack();
        self.mob_entity
            .set_item_slot(&EquipmentSlot::BODY, ItemStack::EMPTY.clone());
        self.get_entity().spawn_at_location(body);
        self.sync_body_effects();
        self.play_sound_at(Sound::EntitySulfurCubeEject, 1.0, 1.0);
        self.pickup_timer
            .store(PICKUP_TIMER_DURATION, Ordering::Relaxed);
    }

    // sulfur cube: applyContactDamage
    fn apply_contact_damage(&self, target: &dyn EntityBase) {
        let damages = self.effects().contact_damages.clone();
        if damages.is_empty() {
            return;
        }
        let attribute_to_player = target.get_player().is_some();
        for damage in damages {
            let cause: Option<&dyn EntityBase> =
                (damage.attribute_to_source || attribute_to_player).then_some(self);
            target.damage_with_context(
                target,
                damage.amount,
                *damage.damage_type,
                None,
                cause,
                cause,
            );
        }
    }

    // sulfur cube: playerPush
    fn player_push(&self, player: &Arc<Player>) {
        if !self.has_body_item() {
            return;
        }
        let player_entity = player.get_entity();
        let root_vehicle = player_entity
            .get_vehicle()
            .map(|_| player_entity.root_vehicle_id());
        let pusher_riding = root_vehicle.is_some();
        let (pusher_pos, pusher_height) = root_vehicle
            .and_then(|id| self.get_entity().world.load().get_entity_by_id(id))
            .map_or_else(
                || {
                    (
                        player_entity.pos.load(),
                        f64::from(player_entity.entity_dimension.load().height),
                    )
                },
                |vehicle| {
                    (
                        vehicle.get_entity().pos.load(),
                        f64::from(vehicle.get_entity().entity_dimension.load().height),
                    )
                },
            );
        let entity = self.get_entity();
        let pos = entity.pos.load();
        let cube_to_pusher = pos.sub(&pusher_pos);
        let cube_top = pos.y + f64::from(entity.entity_dimension.load().height);
        let pusher_top = pusher_pos.y + pusher_height;
        if cube_to_pusher.horizontal_length() >= PUSH_DISTANCE_THRESHOLD
            || pusher_pos.y > cube_top
            || pusher_top <= pos.y
        {
            return;
        }
        let knockback = (1.0
            - self
                .mob_entity
                .living_entity
                .get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE))
        .max(0.0);
        let horizontal = Vector3::new(cube_to_pusher.x, 0.0, cube_to_pusher.z).normalize();
        let push_direction = horizontal * knockback;
        let scale = if pusher_riding {
            VEHICLE_PUSH_SPEED_SCALE_MULTIPLIER
        } else {
            PLAYER_PUSH_SPEED_SCALE_MULTIPLIER
        };
        let player_speed =
            (Entity::known_movement(player.as_ref()).length() * 2.0 * f64::from(scale))
                .clamp(0.0, MAX_PLAYER_PUSH_SPEED);
        let vertical = if entity.on_ground.load(Ordering::Relaxed) {
            knockback * f64::from(VERTICAL_PUSH_MULTIPLIER)
        } else {
            0.0
        };
        let push_velocity =
            Vector3::new(push_direction.x, vertical, push_direction.z) * player_speed;
        entity.velocity_dirty.store(true, Ordering::Relaxed);
        let sound = self.effects().sound;
        let threshold = f64::from(sound.push_sound_impulse_threshold);
        if push_velocity.length_squared() > threshold * threshold
            && self.push_sound_cooldown.load(Ordering::Relaxed) <= 0
        {
            self.push_sound_cooldown
                .store((sound.push_sound_cooldown * 20.0) as i32, Ordering::Relaxed);
            self.play_sound_at(sound.push_sound, 1.0, 1.0);
        }
        entity
            .velocity
            .store(entity.velocity.load() + push_velocity);
        self.apply_contact_damage(player.as_ref());
    }

    // sulfur cube: dealDefaultKnockback
    fn deal_default_knockback(
        &self,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
        damage: f32,
    ) {
        let entity = self.get_entity();
        let pos = entity.pos.load();
        let (mut xd, mut zd) = (0.0, 0.0);
        if let Some(projectile) = source.filter(|source| source.get_projectile_owner().is_some()) {
            // projectile: calculateHorizontalHurtKnockbackDirection, the flight direction pushes
            let velocity = projectile.get_entity().velocity.load();
            xd = -velocity.x;
            zd = -velocity.z;
        } else if let Some(origin) = position.or_else(|| source.map(|s| s.get_entity().pos.load()))
        {
            xd = origin.x - pos.x;
            zd = origin.z - pos.z;
        }
        let _ = damage_type;
        let attacker = cause.or(source);
        if !self.apply_cube_knockback(DEFAULT_KNOCKBACK_POWER, xd, zd, damage, attacker, false) {
            let resistance = self
                .mob_entity
                .living_entity
                .get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE);
            entity.apply_knockback(
                crate::entity::combat::knockback_after_resistance(
                    DEFAULT_KNOCKBACK_POWER,
                    resistance,
                ),
                xd,
                zd,
            );
        }
    }

    // sulfur cube: knockback, aimed hits transfer power by where the attacker looks
    fn apply_cube_knockback(
        &self,
        power: f64,
        xd: f64,
        zd: f64,
        damage: f32,
        attacker: Option<&dyn EntityBase>,
        comes_from_effect: bool,
    ) -> bool {
        let Some(attacker) = attacker else {
            return false;
        };
        if !self.has_body_item() {
            return false;
        }
        let (knockback, hit_sound) = {
            let effects = self.effects();
            (effects.knockback, effects.sound.hit_sound)
        };
        let mut horizontal_power = knockback.horizontal_power;
        let mut vertical_power = knockback.vertical_power;
        let attacker_entity = attacker.get_entity();
        let eye_position = attacker_entity.get_eye_pos();
        let look_angle = Vector3::rotation_vector(
            f64::from(attacker_entity.pitch.load()),
            f64::from(attacker_entity.yaw.load()),
        )
        .normalize();
        let entity = self.get_entity();
        let height = entity.entity_dimension.load().height;
        let bounds = entity.bounding_box.load();
        let center = Vector3::new(
            f64::midpoint(bounds.min.x, bounds.max.x),
            f64::midpoint(bounds.min.y, bounds.max.y),
            f64::midpoint(bounds.min.z, bounds.max.z),
        );

        let new_angle = apply_horizontal_hit_angle_scale(
            HORIZONTAL_HIT_ANGLE_SCALE,
            (xd as f32, zd as f32),
            eye_position,
            look_angle,
            center,
        );
        let (px, py) = apply_vertical_hit_angle_power_transfer(
            VERTICAL_HIT_ANGLE_SCALE,
            horizontal_power,
            vertical_power,
            eye_position,
            look_angle,
            center,
            height,
        );
        let (rx, ry) = apply_vertical_position_angle_power_rotation(
            VERTICAL_POSITION_ANGLE_SCALE,
            (px, py),
            (horizontal_power, vertical_power),
            attacker_entity.pos.load(),
            entity.pos.load(),
        );
        horizontal_power = rx;
        vertical_power = ry;
        let power_multiplier = damage.sqrt()
            * if comes_from_effect {
                power as f32 * EXTRA_KNOCKBACK_DAMPENING
            } else {
                1.0
            };
        horizontal_power *= power_multiplier;
        vertical_power *= power_multiplier;
        let resistance = self
            .mob_entity
            .living_entity
            .get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE);
        horizontal_power *= (1.0 - resistance) as f32;
        vertical_power *= (1.0 - resistance) as f32;
        entity.velocity_dirty.store(true, Ordering::Relaxed);
        horizontal_power *= HORIZONTAL_KNOCKBACK_SCALE;
        horizontal_power = horizontal_power.clamp(-MAX_KNOCKBACK_POWER, MAX_KNOCKBACK_POWER);
        vertical_power = vertical_power.clamp(-MAX_KNOCKBACK_POWER, MAX_KNOCKBACK_POWER);
        let horizontal_knockback =
            Vector3::new(f64::from(new_angle.0), 0.0, f64::from(new_angle.1)).normalize()
                * f64::from(horizontal_power);
        let delta = entity.velocity.load();
        entity.velocity.store(Vector3::new(
            delta.x - horizontal_knockback.x,
            f64::from(vertical_power).mul_add(VERTICAL_KNOCKBACK_SCALE, delta.y),
            delta.z - horizontal_knockback.z,
        ));
        self.play_sound_at(hit_sound, 1.0, 1.0);
        true
    }

    // cube mob: AbstractCubeMob.tick landing particles and squish
    fn tick_squish_effects(&self) {
        let entity = self.get_entity();
        let on_ground = entity.on_ground.load(Ordering::Relaxed);
        let was_on_ground = self.was_on_ground.load(Ordering::Relaxed);
        if on_ground && !was_on_ground {
            let world = entity.world.load();
            let pos = entity.pos.load();
            let size = entity.entity_dimension.load().width * 2.0;
            let radius = size / 2.0;
            let mut rng = self.get_random();
            for _ in 0..(size * 16.0) as i32 {
                let dir = rng.random::<f32>() * std::f32::consts::TAU;
                let d = rng.random::<f32>().mul_add(0.5, 0.5);
                world.spawn_particle(
                    Vector3::new(
                        pos.x + f64::from(dir.sin() * radius * d),
                        pos.y,
                        pos.z + f64::from(dir.cos() * radius * d),
                    ),
                    Vector3::new(0.0, 0.0, 0.0),
                    0.0,
                    1,
                    Particle::SulfurCubeGoo,
                );
            }
            self.play_sound_at(
                self.squish_sound(),
                self.sound_volume(),
                (rng.random::<f32>() - rng.random::<f32>()).mul_add(0.2, 1.0) / 0.8,
            );
            self.target_squish.store(-0.5);
        } else if !on_ground && was_on_ground {
            self.target_squish.store(1.0);
        }
        self.was_on_ground.store(on_ground, Ordering::Relaxed);
        // cube mob: decreaseSquish
        self.target_squish.store(self.target_squish.load() * 0.6);
    }

    // cube mob: remove(), splits into getSplitCount halves
    fn split_on_death(&self) {
        let size = self.get_size();
        if size <= MIN_SIZE
            || self
                .has_split
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let half_size = size / 2;
        let count = if self.is_primed() { 0 } else { SPLIT_COUNT };
        let offset = entity.entity_dimension.load().width / 2.0;
        for i in 0..count {
            let xd = ((i % 2) as f32 - 0.5) * offset;
            let zd = ((i / 2) as f32 - 0.5) * offset;
            let child = crate::entity::r#type::from_type(
                entity.entity_type,
                Vector3::new(pos.x + f64::from(xd), pos.y + 0.5, pos.z + f64::from(zd)),
                &world,
                uuid::Uuid::new_v4(),
            );
            if let Some(cube) = child.cast_any().downcast_ref::<Self>() {
                cube.set_size(half_size, true);
                cube.set_baby(true);
            }
            let child_entity = child.get_entity();
            child_entity
                .yaw
                .store(self.get_random().random::<f32>() * 360.0);
            child_entity
                .custom_name
                .store(entity.custom_name.load_full());
            child_entity.invulnerable.store(
                entity.invulnerable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            world.spawn_entity_non_save(child);
        }
    }

    // sulfur cube: setSpawnSize
    fn set_spawn_size(&self) {
        if AgeableMob::is_baby(self) {
            self.set_size(MIN_SIZE, true);
        } else {
            self.set_size(MAX_SIZE, true);
        }
    }
}

// sulfur cube: applyHorizontalHitAngleScale
fn apply_horizontal_hit_angle_scale(
    horizontal_angle_scale: f32,
    original_angle: (f32, f32),
    attacker_position: Vector3<f64>,
    attacker_aim_direction: Vector3<f64>,
    target_center: Vector3<f64>,
) -> (f32, f32) {
    let attacker_to_target = target_center.sub(&attacker_position).normalize();
    let angle_diff = (attacker_aim_direction.x * attacker_to_target.z
        - attacker_aim_direction.z * attacker_to_target.x)
        .atan2(
            attacker_aim_direction.x * attacker_to_target.x
                + attacker_aim_direction.z * attacker_to_target.z,
        ) as f32;
    // Vec2.rotate
    let angle = f64::from(angle_diff * horizontal_angle_scale);
    let (cosine, sine) = (angle.cos() as f32, angle.sin() as f32);
    (
        original_angle.0 * cosine - original_angle.1 * sine,
        original_angle.1 * cosine + original_angle.0 * sine,
    )
}

// sulfur cube: applyVerticalHitAnglePowerTransfer
fn apply_vertical_hit_angle_power_transfer(
    vertical_hit_angle_scale: f32,
    horizontal_power: f32,
    vertical_power: f32,
    attacker_position: Vector3<f64>,
    attacker_aim_direction: Vector3<f64>,
    target_centered_position: Vector3<f64>,
    target_height: f32,
) -> (f32, f32) {
    let half_height = f64::from(0.5 * target_height);
    let target_top = target_centered_position.add_raw(0.0, half_height, 0.0);
    let target_bottom = target_centered_position.add_raw(0.0, -half_height, 0.0);
    let to_top = target_top.sub(&attacker_position).normalize();
    let to_bottom = target_bottom.sub(&attacker_position).normalize();
    // Mth.clampedMap(aim.y, toTop.y, toBottom.y, -1, 1)
    let factor = ((attacker_aim_direction.y - to_top.y) / (to_bottom.y - to_top.y))
        .clamp(0.0, 1.0)
        .mul_add(2.0, -1.0) as f32;
    let mut ratio = (factor * vertical_hit_angle_scale).abs();
    if factor < 0.0 {
        ratio = -ratio;
    }
    (
        horizontal_power * (1.0 - ratio),
        vertical_power * (1.0 + ratio),
    )
}

// sulfur cube: applyVerticalPositionAnglePowerRotation
fn apply_vertical_position_angle_power_rotation(
    vertical_position_angle_scale: f32,
    power: (f32, f32),
    original_power: (f32, f32),
    attacker_feet: Vector3<f64>,
    target_feet: Vector3<f64>,
) -> (f32, f32) {
    let delta = target_feet.sub(&attacker_feet);
    let angle = (-delta.y).atan2(delta.horizontal_length()) as f32;
    let radians = f64::from(-angle * vertical_position_angle_scale);
    let (cosine, sine) = (radians.cos() as f32, radians.sin() as f32);
    let mut rotated = (
        power.0 * cosine - power.1 * sine,
        power.1 * cosine + power.0 * sine,
    );
    let horizontal_ratio = if original_power.0 > 0.0 {
        rotated.0.abs() / original_power.0
    } else {
        0.0
    };
    let vertical_ratio = if original_power.1 > 0.0 {
        rotated.1.abs() / original_power.1
    } else {
        0.0
    };
    let max_ratio = horizontal_ratio.max(vertical_ratio);
    if max_ratio > 1.0 {
        let scale = 1.0 / max_ratio;
        rotated = (rotated.0 * scale, rotated.1 * scale);
    }
    rotated
}

impl CustomSound for SulfurCubeEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_tiny() {
            Sound::EntitySmallSulfurCubeDeath
        } else {
            Sound::EntitySulfurCubeDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_tiny() {
            Sound::EntitySmallSulfurCubeHurt
        } else {
            Sound::EntitySulfurCubeHurt
        })
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Neutral
    }
}

impl AgeableMob for SulfurCubeEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    // sulfur cube: ageBoundaryReached, an adult grows to size 2
    fn age_boundary_reached(&self) {
        if !AgeableMob::is_baby(self) {
            self.set_size(MAX_SIZE, true);
        }
    }
}

impl Mob for SulfurCubeEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    // cube mob: a baby keeps its size scaled box (getDefaultDimensions)
    fn baby_dimensions(&self) -> Option<EntityDimensions> {
        Some(Self::dimensions_for_size(
            self.get_entity(),
            self.get_size(),
        ))
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn mob_metadata(&self) -> Option<crate::entity::mob::MobMetadata> {
        Some(crate::entity::mob::MobMetadata::new().slime_size(self.get_size()))
    }

    // sulfur cube: soundSource NEUTRAL
    fn mob_sound_source(&self) -> SoundCategory {
        SoundCategory::Neutral
    }

    fn mob_play_step_sound(
        &self,
        pos: &pumpkin_util::math::position::BlockPos,
        state: &pumpkin_data::BlockState,
    ) {
        if !self.has_body_item() {
            self.play_block_step_sound(state);
        }
        let _ = pos;
    }

    // sulfur cube: canDispenserEquipIntoSlot, only the body slot
    fn can_dispenser_equip_into_slot(&self, slot: &EquipmentSlot) -> bool {
        slot == &EquipmentSlot::BODY
    }

    fn requires_custom_persistence(&self) -> bool {
        self.get_entity().has_vehicle() || self.has_body_item() || self.is_from_bucket()
    }

    fn can_be_leashed(&self) -> bool {
        self.has_body_item()
    }

    fn omnidirectional_air_mover(&self) -> bool {
        self.has_body_item()
    }

    fn get_base_experience_reward(&self) -> u32 {
        if self.is_baby_cube() {
            0
        } else {
            1 + self.get_random().random_range(0..2)
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int("Size", self.get_size() - 1);
        nbt.put_bool("wasOnGround", self.was_on_ground.load(Ordering::Relaxed));
        nbt.put_int("pickup_timer", self.pickup_timer());
        nbt.put_bool("from_bucket", self.is_from_bucket());
        nbt.put_int("fuse", self.get_fuse());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_size(nbt.get_int("Size").unwrap_or(0) + 1, false);
        self.was_on_ground.store(
            nbt.get_bool("wasOnGround").unwrap_or(false),
            Ordering::Relaxed,
        );
        self.pickup_timer
            .store(nbt.get_int("pickup_timer").unwrap_or(0), Ordering::Relaxed);
        self.set_from_bucket(nbt.get_bool("from_bucket").unwrap_or(false));
        let fuse = nbt.get_int("fuse").unwrap_or(NO_FUSE);
        self.set_fuse(fuse);
        self.get_entity()
            .set_synced_data(tracked_data::sulfur_cube::MAX_FUSE, fuse);
    }

    // sulfur cube: finalizeSpawn, AgeableMob group babies then setSpawnSize
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.finalize_ageable_spawn(group_data);
        self.set_spawn_size();
        group_data
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.sync_body_effects();
        self.tick_fuse();
        self.prime_when_on_powered_position();
        // cube mob: squish interpolation
        self.o_squish.store(self.squish.load());
        self.squish.store(
            (self.target_squish.load() - self.squish.load()).mul_add(0.5, self.squish.load()),
        );
    }

    fn post_tick(&self) {
        self.tick_squish_effects();
        if self.mob_entity.living_entity.dead.load(Ordering::Relaxed) {
            self.split_on_death();
        }
    }

    // sulfur cube: customServerAiStep timers
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        if self.pickup_timer() > 0 {
            self.pickup_timer.fetch_sub(1, Ordering::Relaxed);
        }
        if self.push_sound_cooldown.load(Ordering::Relaxed) > 0 {
            self.push_sound_cooldown.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // sulfur cube: floats and vibrates on liquids with buoyant blocks (travelInFluid)
    fn after_travel_in_fluid(&self, water: bool) {
        if !self.has_body_item() || !self.effects().floats_in_liquids {
            return;
        }
        let entity = self.get_entity();
        let vibe_amount = 0.2f32 * (entity.tick_count.load(Ordering::Relaxed) as f32 * 0.4).sin();
        let fluid_height = if water {
            entity.water_height.load()
        } else {
            entity.lava_height.load()
        };
        let immersion =
            fluid_height - self.mob_entity.living_entity.get_swim_height() + f64::from(vibe_amount);
        if immersion > 0.0 {
            let velocity = entity.velocity.load();
            entity.velocity.store(Vector3::new(
                velocity.x,
                immersion.min(1.0).mul_add(f64::from(0.04f32), velocity.y),
                velocity.z,
            ));
        }
    }

    // sulfur cube: hurtServer, priming by fire and explosions, block immune damage
    fn damage_override(
        &self,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> Option<bool> {
        if !self.has_body_item() {
            return None;
        }
        if self.can_explode() {
            let on_fire_arrow = source
                .and_then(|source| source.cast_any().downcast_ref::<ArrowEntity>())
                .is_some_and(ArrowEntity::is_on_fire);
            if damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_FIRE) || on_fire_arrow {
                self.prime_time(false);
            } else if damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_EXPLOSION) {
                self.prime_time(true);
            }
        }
        if damage_type.has_tag(&tag::DamageType::MINECRAFT_SULFUR_CUBE_WITH_BLOCK_IMMUNE_TO) {
            if !damage_type.has_tag(&tag::DamageType::MINECRAFT_NO_KNOCKBACK) {
                self.deal_default_knockback(damage_type, position, source, cause, amount);
            }
            return Some(true);
        }
        None
    }

    fn knockback_override(
        &self,
        power: f64,
        dx: f64,
        dz: f64,
        damage: f32,
        attacker: Option<&dyn EntityBase>,
    ) -> bool {
        self.apply_cube_knockback(power, dx, dz, damage, attacker, false)
    }

    // sulfur cube: only an adult without a body picks up swallowable items (canPickUpLoot, canHoldItem)
    fn can_pick_up_loot(&self, _brain: Option<&crate::entity::ai::brain::BrainState>) -> bool {
        !self.has_body_item()
    }

    fn can_hold_item(&self, stack: &ItemStack) -> bool {
        !self.has_body_item() && Self::is_swallowable_item(stack) && !self.is_baby_cube()
    }

    // sulfur cube: pickUpItem
    fn mob_pick_up_item(&self, item: &crate::entity::item::ItemEntity, stack: &ItemStack) -> bool {
        if self.can_hold_item(stack) && self.pickup_timer() <= 0 {
            let mut single = stack.clone();
            single.item_count = 1;
            self.mob_entity.set_item_slot(&EquipmentSlot::BODY, single);
            self.mob_entity.set_guaranteed_drop(&EquipmentSlot::BODY);
            self.sync_body_effects();
            self.play_sound_at(Sound::EntitySulfurCubeAbsorb, 1.0, 1.0);
            if self.mob_entity.living_entity.pickup(item.get_entity(), 1) {
                let emptied = {
                    let mut ground = item
                        .get_item_stack()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    ground.item_count = ground.item_count.saturating_sub(1);
                    ground.is_empty()
                };
                if emptied {
                    item.get_entity().remove();
                } else {
                    item.init_data_tracker();
                }
            }
        }
        true
    }

    // sulfur cube: canFreeze false with a body item
    fn mob_can_freeze(&self) -> bool {
        !self.has_body_item()
    }

    // sulfur cube: a primed cube is hit by spears despite its invulnerability
    fn mob_invulnerable_to_piercing_weapon(&self) -> bool {
        self.get_entity().invulnerable.load(Ordering::Relaxed) && !self.is_primed()
    }

    // sulfur cube: a ray from inside only picks a cube without a body item
    fn mob_can_be_picked_from_inside(&self) -> bool {
        !self.has_body_item()
    }

    fn mob_player_collision(&self, player: &Arc<Player>) {
        self.player_push(player);
    }

    // sulfur cube: doPush applies contact damage
    fn mob_entity_collision(&self, entity: &dyn EntityBase) {
        self.apply_contact_damage(entity);
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if self.is_baby_cube() {
            if Self::is_food(item_stack) && self.can_age_up() {
                let age = self.get_age();
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                self.age_up(Self::get_speed_up_seconds_when_feeding(-age), true);
                self.play_sound_at(Sound::EntitySmallSulfurCubeEat, 1.0, 1.0);
                return true;
            }
            return false;
        }
        if self.is_primed() {
            return false;
        }
        let item = item_stack.item;
        let igniter = item == &Item::FLINT_AND_STEEL || item == &Item::FIRE_CHARGE;
        if !(self.can_explode() && igniter) {
            if item == &Item::SHEARS && self.has_body_item() {
                let entity = self.get_entity();
                let world = entity.world.load();
                if let Some(server) = world.server.upgrade() {
                    let mut event = crate::plugin::api::events::player::player_shear_entity::PlayerShearEntityEvent {
                        player: player.clone(),
                        entity_id: entity.entity_id,
                        hand: 0,
                        cancelled: false,
                    };
                    server.plugin_manager.fire_blocking(&server, &mut event);
                    if event.cancelled {
                        return false;
                    }
                }
                self.shear();
                world.emit_game_event(GameEvent::Shear.name(), entity.pos.load());
                let _ = player.damage_item_stack(item_stack, 1);
                return true;
            }
            if Self::is_swallowable_item(item_stack) {
                let worked = self.equip_item(item_stack);
                if worked {
                    item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                    let entity = self.get_entity();
                    entity
                        .world
                        .load()
                        .emit_game_event(GameEvent::EntityInteract.name(), entity.pos.load());
                }
                return worked;
            }
            // empty bucket pickup is handled by the bucket item (Bucketable.bucketMobPickup)
            return false;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        if !world.level_info.load().game_rules.tnt_explodes {
            player.send_system_message_raw(
                &pumpkin_util::text::TextComponent::translate("block.minecraft.tnt.disabled", []),
                true,
            );
            return false;
        }
        self.prime_time(false);
        if item == &Item::FLINT_AND_STEEL {
            let _ = player.damage_item_stack(item_stack, 1);
        } else {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;
    use pumpkin_data::entity::EntityType;

    fn tag_item(tag: &tag::Tag) -> &'static Item {
        Item::from_id(tag.1[0]).expect("archetype item")
    }

    fn body_attribute(cube: &SulfurCubeEntity, attribute: &Attributes) -> f64 {
        cube.mob_entity.living_entity.get_attribute_value(attribute)
    }

    #[tokio::test]
    async fn swapping_the_body_item_replaces_the_archetype_modifiers()
    -> Result<(), Box<dyn std::error::Error>> {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let cube = SulfurCubeEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.0, 64.0, 0.0),
            &EntityType::SULFUR_CUBE,
        ));
        let regular = tag_item(&tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_REGULAR);
        let bouncy = tag_item(&tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_BOUNCY);

        cube.mob_entity
            .set_item_slot(&EquipmentSlot::BODY, ItemStack::new(1, regular));
        cube.sync_body_effects();
        let regular_bounciness = body_attribute(&cube, &Attributes::BOUNCINESS);
        cube.mob_entity
            .set_item_slot(&EquipmentSlot::BODY, ItemStack::new(1, bouncy));
        cube.sync_body_effects();
        let bouncy_bounciness = body_attribute(&cube, &Attributes::BOUNCINESS);
        cube.mob_entity
            .set_item_slot(&EquipmentSlot::BODY, ItemStack::EMPTY.clone());
        cube.sync_body_effects();
        let empty_bounciness = body_attribute(&cube, &Attributes::BOUNCINESS);
        world.level.shutdown().await;

        assert_eq!(regular_bounciness, 0.5);
        assert!((bouncy_bounciness - 0.9).abs() < 1e-6);
        assert_eq!(empty_bounciness, 0.0);
        Ok(())
    }

    #[test]
    fn wrap_degrees_90_snaps_to_the_nearest_right_angle() {
        use super::super::sulfur_cube_ai::wrap_degrees_90;
        assert_eq!(wrap_degrees_90(44.0), 44.0);
        assert_eq!(wrap_degrees_90(45.0), -45.0);
        assert_eq!(wrap_degrees_90(100.0), 10.0);
        assert_eq!(wrap_degrees_90(-100.0), -10.0);
    }
}
