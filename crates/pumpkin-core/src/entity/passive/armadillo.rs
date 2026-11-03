mod ai;

use std::sync::{
    Arc,
    atomic::{AtomicI32, AtomicI64, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::{boundingbox::EntityDimensions, position::BlockPos};
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::brain::{BrainState, memory::PackedMemories, memory::types};
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::gift_loot::drop_from_loot_table;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

pub const SCARE_CHECK_INTERVAL: i32 = 80;
pub const SCARE_DISTANCE_HORIZONTAL: f64 = 7.0;
pub const SCARE_DISTANCE_VERTICAL: f64 = 2.0;
const BABY_SCALE: f32 = 0.6;
const BABY_EYE_HEIGHT: f32 = 0.21875;
const MAX_HEAD_ROTATION: f32 = 32.0;
const SCUTE_DROP_TICKS: i32 = 20 * 60 * 5;

// armadillo: Armadillo.pickNextScuteDropTime
fn pick_next_scute_drop_time() -> i32 {
    rand::rng().random_range(0..SCUTE_DROP_TICKS) + SCUTE_DROP_TICKS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum ArmadilloState {
    #[default]
    Idle = 0,
    Rolling = 1,
    Scared = 2,
    Unrolling = 3,
}

impl ArmadilloState {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::Rolling,
            2 => Self::Scared,
            3 => Self::Unrolling,
            _ => Self::Idle,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub const fn is_threatened(self) -> bool {
        !matches!(self, Self::Idle)
    }

    #[must_use]
    pub const fn animation_duration(self) -> u64 {
        match self {
            Self::Idle => 0,
            Self::Rolling => 10,
            Self::Scared => 50,
            Self::Unrolling => 30,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Rolling => "rolling",
            Self::Scared => "scared",
            Self::Unrolling => "unrolling",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "rolling" => Self::Rolling,
            "scared" => Self::Scared,
            "unrolling" => Self::Unrolling,
            _ => Self::Idle,
        }
    }
}

pub struct ArmadilloEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub state: AtomicI32,
    pub in_state_ticks: AtomicI64,
    pub scute_time: AtomicI32,
}

impl ArmadilloEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        // armadillo: getNavigation().setCanFloat(true)
        mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_float(true);
        Arc::new(Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            state: AtomicI32::new(ArmadilloState::Idle.id()),
            in_state_ticks: AtomicI64::new(0),
            scute_time: AtomicI32::new(pick_next_scute_drop_time()),
        })
    }

    #[must_use]
    pub fn get_state(&self) -> ArmadilloState {
        ArmadilloState::from_id(self.state.load(Ordering::Relaxed))
    }

    // armadillo: isScared, any state but IDLE
    #[must_use]
    pub fn is_scared(&self) -> bool {
        self.get_state() != ArmadilloState::Idle
    }

    // armadillo: shouldSwitchToScaredState
    #[must_use]
    pub fn should_switch_to_scared_state(&self) -> bool {
        self.get_state() == ArmadilloState::Rolling
            && self.in_state_ticks.load(Ordering::Relaxed)
                > ArmadilloState::Rolling.animation_duration() as i64
    }

    // armadillo: switchToState, onSyncedDataUpdated resets inStateTicks
    pub fn switch_to_state(&self, state: ArmadilloState) {
        if self.get_state() == state {
            return;
        }
        self.state.store(state.id(), Ordering::Relaxed);
        self.in_state_ticks.store(0, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::armadillo::ARMADILLO_STATE,
            VarInt(state.id()),
        );
    }

    fn emit_game_event(&self, event: GameEvent) {
        let entity = self.get_entity();
        entity
            .world
            .load()
            .emit_game_event(event.name(), entity.pos.load());
    }

    // armadillo: rollUp
    pub fn roll_up(&self) {
        if self.is_scared() {
            return;
        }
        self.mob_entity.stop_in_place();
        self.reset_love();
        self.emit_game_event(GameEvent::EntityAction);
        self.make_sound(Sound::EntityArmadilloRoll);
        self.switch_to_state(ArmadilloState::Rolling);
    }

    // armadillo: rollOut
    pub fn roll_out(&self) {
        if self.is_scared() {
            self.emit_game_event(GameEvent::EntityAction);
            self.make_sound(Sound::EntityArmadilloUnrollFinish);
            self.switch_to_state(ArmadilloState::Idle);
        }
    }

    // armadillo: isInLiquid
    #[must_use]
    pub fn is_in_liquid(&self) -> bool {
        let entity = self.get_entity();
        entity.is_in_water() || entity.touching_lava.load(Ordering::Relaxed)
    }

    // armadillo: canStayRolledUp, panic comes from the brain memory
    #[must_use]
    pub fn can_stay_rolled_up_in(&self, brain: &BrainState) -> bool {
        let entity = self.get_entity();
        !brain.has_memory_value(types::IS_PANICKING.id())
            && !self.is_in_liquid()
            && !entity.is_leashed()
            && !entity.has_vehicle()
            && !entity.has_passengers()
    }

    // armadillo: brushOffScute, brush/armadillo loot table and brush sound
    pub fn brush_off_scute(&self, player: Option<&Player>, item_stack: &mut ItemStack) -> bool {
        if self.is_baby() {
            return false;
        }
        drop_from_loot_table(
            self,
            "minecraft:brush/armadillo",
            player.map(|player| player as &dyn EntityBase),
            Some(item_stack.clone()),
        );
        self.play_entity_sound(Sound::EntityArmadilloBrush, 1.0, 1.0);
        self.emit_game_event(GameEvent::EntityInteract);
        if player.is_none_or(|player| player.gamemode.load() != pumpkin_util::GameMode::Creative) {
            let _ = item_stack.damage_item(16);
        }
        true
    }

    // armadillo: isScaredBy
    #[must_use]
    pub fn is_scared_by(&self, living_entity: &dyn EntityBase) -> bool {
        let entity = self.get_entity();
        let inflated = entity.bounding_box.load().expand(
            SCARE_DISTANCE_HORIZONTAL,
            SCARE_DISTANCE_VERTICAL,
            SCARE_DISTANCE_HORIZONTAL,
        );
        if !inflated.intersects(&living_entity.get_entity().bounding_box.load()) {
            return false;
        }
        if living_entity
            .get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_UNDEAD)
        {
            return true;
        }
        if self
            .mob_entity
            .living_entity
            .get_last_hurt_by_mob()
            .is_some_and(|attacker| {
                attacker.get_entity().entity_id == living_entity.get_entity().entity_id
            })
        {
            return true;
        }
        if let Some(player) = living_entity.get_player() {
            return !player.is_spectator()
                && (player.get_entity().is_sprinting() || player.get_entity().has_vehicle());
        }
        false
    }
}

impl AgeableMob for ArmadilloEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for ArmadilloEntity {
    fn eating_sound(&self) -> Option<Sound> {
        Some(Sound::EntityArmadilloEat)
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack
            .item
            .has_tag(&tag::Item::MINECRAFT_ARMADILLO_FOOD)
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        from_type(
            &EntityType::ARMADILLO,
            entity.pos.load(),
            &entity.world.load(),
            Uuid::new_v4(),
        )
    }
}

impl CustomSound for ArmadilloEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityArmadilloDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        // armadillo: reduced hurt sound when rolled up (Armadillo.getHurtSound)
        Some(if self.is_scared() {
            Sound::EntityArmadilloHurtReduced
        } else {
            Sound::EntityArmadilloHurt
        })
    }
}

impl Mob for ArmadilloEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        if self.is_scared() {
            None
        } else {
            Some(Sound::EntityArmadilloAmbient)
        }
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityArmadilloStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_string("state", self.get_state().name().to_string());
        nbt.put_int("scute_time", self.scute_time.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.switch_to_state(
            nbt.get_string("state")
                .map_or(ArmadilloState::Idle, ArmadilloState::from_name),
        );
        if let Some(scute_time) = nbt.get_int("scute_time") {
            self.scute_time.store(scute_time, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // armadillo: BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<EntityDimensions> {
        let adult = Entity::type_dimensions(self.get_entity().entity_type);
        Some(EntityDimensions {
            width: adult.width * BABY_SCALE,
            height: adult.height * BABY_SCALE,
            eye_height: BABY_EYE_HEIGHT,
        })
    }

    fn make_brain(&self, packed: &PackedMemories) -> crate::entity::ai::brain::Brain {
        ai::make_brain(self, packed)
    }

    // armadillo: hurtServer, shell damage reduction while rolled up
    fn modify_incoming_damage(&self, amount: f32, _damage_type: DamageType) -> f32 {
        if self.is_scared() {
            (amount - 1.0) / 2.0
        } else {
            amount
        }
    }

    // armadillo: actuallyHurt
    fn on_damage(&self, damage_type: DamageType, source: Option<&dyn EntityBase>) {
        if self.mob_entity.is_no_ai() || self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }
        if source.is_some_and(|source| source.get_living_entity().is_some()) {
            let can_stay_rolled_up = self.mob_entity.with_brain(self, |brain| {
                brain.set_with_expiry(
                    types::DANGER_DETECTED_RECENTLY,
                    true,
                    i64::from(SCARE_CHECK_INTERVAL),
                );
                self.can_stay_rolled_up_in(brain)
            });
            if can_stay_rolled_up {
                self.roll_up();
            }
        } else if damage_type.has_tag(&tag::DamageType::MINECRAFT_PANIC_ENVIRONMENTAL_CAUSES) {
            self.roll_out();
        }
    }

    // armadillo: tick, clamp the head while scared and count the state ticks
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        if self.is_scared() {
            let entity = self.get_entity();
            let max = self.get_max_head_rotation();
            let head = entity.head_yaw.load();
            let diff = pumpkin_util::math::wrap_degrees(entity.body_yaw.load() - head);
            let clamped = diff.clamp(-max, max);
            entity.head_yaw.store(head + diff - clamped);
        }
        self.in_state_ticks.fetch_add(1, Ordering::Relaxed);
    }

    // armadillo: customServerAiStep, brain, activity and the scute shed
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.mob_entity.tick_brain(self);
        self.mob_entity.with_brain(self, ai::update_activity);
        if self.get_entity().is_alive()
            && self.scute_time.fetch_sub(1, Ordering::Relaxed) - 1 <= 0
            && self.mob_entity.living_entity.should_drop_loot()
        {
            if drop_from_loot_table(self, "minecraft:gameplay/armadillo_shed", None, None) {
                self.play_entity_sound(
                    Sound::EntityArmadilloScuteDrop,
                    1.0,
                    (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0,
                );
                self.emit_game_event(GameEvent::EntityPlace);
            }
            self.scute_time
                .store(pick_next_scute_drop_time(), Ordering::Relaxed);
        }
    }

    // armadillo: getMaxHeadYRot
    fn get_max_head_rotation(&self) -> f32 {
        if self.is_scared() {
            0.0
        } else {
            MAX_HEAD_ROTATION
        }
    }

    // armadillo: createBodyControl, no body turning while rolled up
    fn custom_body_rotation(&self) -> bool {
        self.is_scared()
    }

    // armadillo: canFallInLove
    fn can_fall_in_love(&self) -> bool {
        !self.is_scared()
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::armadillo::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::armadillo::ARMADILLO_STATE,
            VarInt(self.get_state().id()),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.item == &Item::BRUSH && self.brush_off_scute(Some(player), item_stack) {
            return true;
        }
        if self.is_scared() {
            return false;
        }
        self.animal_interact(player, item_stack, Sound::EntityArmadilloAmbient)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use pumpkin_util::math::vector3::Vector3;

    use super::*;
    use crate::entity::ai::test_support::world;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn danger_rolls_the_armadillo_up_and_it_unrolls_when_the_danger_ends() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let armadillo = ArmadilloEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::ARMADILLO,
        ));
        armadillo
            .get_entity()
            .on_ground
            .store(true, Ordering::Relaxed);
        armadillo
            .mob_entity
            .with_brain(armadillo.as_ref(), |brain| {
                brain.set_with_expiry(types::DANGER_DETECTED_RECENTLY, true, 200);
            });
        let step = |ticks: i32| {
            for _ in 0..ticks {
                armadillo.custom_server_ai_step(armadillo.as_ref());
                armadillo.mob_tick(armadillo.as_ref());
            }
        };
        step(2);
        assert_eq!(armadillo.get_state(), ArmadilloState::Rolling);
        step(20);
        assert_eq!(armadillo.get_state(), ArmadilloState::Scared);
        armadillo
            .mob_entity
            .with_brain(armadillo.as_ref(), |brain| {
                brain.erase(types::DANGER_DETECTED_RECENTLY.id());
            });
        step(2);
        assert_eq!(armadillo.get_state(), ArmadilloState::Idle);
        world.level.shutdown().await;
        Ok(())
    }
}
