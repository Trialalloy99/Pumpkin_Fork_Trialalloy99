use std::sync::{
    Arc, Weak,
    atomic::{AtomicI32, AtomicU64, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::target_match::{SearchArea, TargetMatch};
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    item::ItemEntity,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

pub const ARMADILLO_FOOD: &[&Item] = &[&Item::SPIDER_EYE];
pub const ARMADILLO_BABY_START_AGE: i32 = -48000;
pub const SCARE_CHECK_INTERVAL: i32 = 80;
pub const SCARE_DISTANCE_HORIZONTAL: f64 = 7.0;
pub const SCARE_DISTANCE_VERTICAL: f64 = 2.0;

// armadillo: periodic scute drop time 20..40 minutes (Armadillo.pickNextScuteDropTime)
fn pick_next_scute_drop_time() -> i32 {
    let mut rng = rand::rng();
    rng.random_range(24000..48000)
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
    pub const fn should_hide_in_shell(self, ticks_in_state: u64) -> bool {
        match self {
            Self::Idle => false,
            Self::Rolling => ticks_in_state > 5,
            Self::Scared => true,
            Self::Unrolling => ticks_in_state < 26,
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
    pub in_state_ticks: AtomicU64,
    pub scute_time: AtomicI32,
    pub danger_detected_recently_ticks: AtomicI32,
}

impl ArmadilloEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let armadillo = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            state: AtomicI32::new(ArmadilloState::Idle.id()),
            in_state_ticks: AtomicU64::new(0),
            scute_time: AtomicI32::new(pick_next_scute_drop_time()),
            danger_detected_recently_ticks: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(armadillo);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // armadillo: vanilla goal priorities (ArmadilloAi.getActivities)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(2.0));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, ARMADILLO_FOOD, false)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.1)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn get_state(&self) -> ArmadilloState {
        ArmadilloState::from_id(self.state.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn is_scared(&self) -> bool {
        self.get_state()
            .should_hide_in_shell(self.in_state_ticks.load(Ordering::Relaxed))
    }

    pub fn switch_to_state(&self, state: ArmadilloState) {
        let old_state = self.get_state();
        if old_state == state {
            return;
        }

        self.state.store(state.id(), Ordering::Relaxed);
        self.in_state_ticks.store(0, Ordering::Relaxed);

        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::armadillo::ARMADILLO_STATE,
            VarInt(state.id()),
        );

        let world = entity.world.load();
        let pos = entity.pos.load();
        if state == ArmadilloState::Unrolling {
            world.play_sound(
                Sound::EntityArmadilloUnrollStart,
                SoundCategory::Neutral,
                &pos,
            );
        } else if old_state == ArmadilloState::Unrolling && state == ArmadilloState::Idle {
            world.play_sound(
                Sound::EntityArmadilloUnrollFinish,
                SoundCategory::Neutral,
                &pos,
            );
        }
    }

    pub fn roll_up(&self) {
        if !self.can_stay_rolled_up() {
            return;
        }
        let current_state = self.get_state();
        if current_state == ArmadilloState::Idle {
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityArmadilloRoll,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            self.switch_to_state(ArmadilloState::Rolling);
        }
    }

    pub fn roll_out(&self) {
        if self.get_state().is_threatened() {
            self.switch_to_state(ArmadilloState::Idle);
        }
    }

    pub fn can_stay_rolled_up(&self) -> bool {
        let entity = self.get_entity();
        !entity.touching_water.load(Ordering::Relaxed)
            && !entity.touching_lava.load(Ordering::Relaxed)
            && entity
                .leashed_to
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .is_none()
            && entity.vehicle.lock().ok().and_then(|g| g.clone()).is_none()
    }

    // armadillo: brush interaction to drop scute (Armadillo.brushOffScute)
    pub fn brush_off_scute(&self, player: &Player, item_stack: &mut ItemStack) -> bool {
        if self.is_baby() || !self.get_entity().is_alive() {
            return false;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        let item_entity = Arc::new(ItemEntity::new(
            Entity::new(world.clone(), pos, &EntityType::ITEM),
            ItemStack::new(1, &Item::ARMADILLO_SCUTE),
        ));
        world.spawn_entity(item_entity);
        world.play_sound(Sound::EntityArmadilloBrush, SoundCategory::Neutral, &pos);
        if player.gamemode.load() != pumpkin_util::GameMode::Creative {
            let _ = item_stack.damage_item(16);
        }
        true
    }

    // armadillo: check scare conditions (Armadillo.isScaredBy)
    pub fn is_scared_by(&self, living_entity: &dyn EntityBase) -> bool {
        let t = living_entity.get_entity().entity_type;
        if t == &EntityType::ZOMBIE
            || t == &EntityType::HUSK
            || t == &EntityType::DROWNED
            || t == &EntityType::ZOMBIE_VILLAGER
            || t == &EntityType::SKELETON
            || t == &EntityType::STRAY
            || t == &EntityType::WITHER_SKELETON
            || t == &EntityType::PHANTOM
            || t == &EntityType::ZOMBIFIED_PIGLIN
        {
            return true;
        }

        if let Some(player) = living_entity.cast_any().downcast_ref::<Player>()
            && player.living_entity.entity.is_sprinting()
        {
            return true;
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
            || ARMADILLO_FOOD.iter().any(|i| i.id == item_stack.item.id)
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby_base = from_type(
            &EntityType::ARMADILLO,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        baby_base
            .get_entity()
            .age
            .store(ARMADILLO_BABY_START_AGE, Ordering::Relaxed);
        baby_base
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
        if let Some(state_name) = nbt.get_string("state") {
            self.switch_to_state(ArmadilloState::from_name(state_name));
        } else if let Some(state_id) = nbt.get_int("state") {
            self.switch_to_state(ArmadilloState::from_id(state_id));
        }
        if let Some(scute_time) = nbt.get_int("scute_time") {
            self.scute_time.store(scute_time, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // armadillo: shell damage reduction (Armadillo.hurtServer)
    fn modify_incoming_damage(
        &self,
        amount: f32,
        _damage_type: pumpkin_data::damage::DamageType,
    ) -> f32 {
        if self.is_scared() {
            (amount - 1.0).max(0.0) / 2.0
        } else {
            amount
        }
    }

    fn on_damage(
        &self,
        _damage_type: pumpkin_data::damage::DamageType,
        source: Option<&dyn EntityBase>,
    ) {
        if self.get_entity().is_alive()
            && let Some(src) = source
            && src.get_entity().entity_type != &EntityType::ITEM
        {
            self.danger_detected_recently_ticks
                .store(SCARE_CHECK_INTERVAL, Ordering::Relaxed);
            if self.can_stay_rolled_up() {
                self.roll_up();
            }
        }
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.in_state_ticks.fetch_add(1, Ordering::Relaxed);
        let danger_ticks = self.danger_detected_recently_ticks.load(Ordering::Relaxed);
        if danger_ticks > 0 {
            self.danger_detected_recently_ticks
                .store(danger_ticks - 1, Ordering::Relaxed);
        }

        let entity = self.get_entity();
        let world = entity.world.load();

        // Check nearby threats every few ticks
        if self.get_state() == ArmadilloState::Idle
            && entity.tick_count.load(Ordering::Relaxed) % 20 == 0
        {
            let area =
                SearchArea::from_feet(entity, SCARE_DISTANCE_HORIZONTAL, SCARE_DISTANCE_VERTICAL);
            let scared = TargetMatch::Any.find_nearest(&world, &area, |candidate| {
                if candidate.get_entity().entity_id == entity.entity_id {
                    return false;
                }
                self.is_scared_by(candidate)
            });
            if scared.is_some() {
                self.roll_up();
                self.danger_detected_recently_ticks
                    .store(80, Ordering::Relaxed);
            }
        }

        // Periodic scute drop
        if entity.is_alive() && !self.is_baby() {
            let scute_time = self.scute_time.fetch_sub(1, Ordering::Relaxed) - 1;
            if scute_time <= 0 {
                let pos = entity.pos.load();
                world.drop_stack(
                    &pos.to_block_pos(),
                    ItemStack::new(1, &Item::ARMADILLO_SCUTE),
                );
                world.play_sound(
                    Sound::EntityArmadilloScuteDrop,
                    SoundCategory::Neutral,
                    &pos,
                );
                self.scute_time
                    .store(pick_next_scute_drop_time(), Ordering::Relaxed);
            }
        }

        let state = self.get_state();
        let ticks_in_state = self.in_state_ticks.load(Ordering::Relaxed);

        match state {
            ArmadilloState::Rolling => {
                if ticks_in_state > ArmadilloState::Rolling.animation_duration() {
                    self.switch_to_state(ArmadilloState::Scared);
                }
            }
            ArmadilloState::Scared => {
                if !self.can_stay_rolled_up() {
                    self.roll_out();
                } else if ticks_in_state > ArmadilloState::Scared.animation_duration()
                    && self.danger_detected_recently_ticks.load(Ordering::Relaxed) == 0
                {
                    self.switch_to_state(ArmadilloState::Unrolling);
                }
            }
            ArmadilloState::Unrolling => {
                if ticks_in_state > ArmadilloState::Unrolling.animation_duration() {
                    self.roll_out();
                }
            }
            ArmadilloState::Idle => {}
        }
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
        if item_stack.item == &Item::BRUSH && self.brush_off_scute(player, item_stack) {
            return true;
        }
        if self.is_scared() {
            return false;
        }
        self.animal_interact(player, item_stack, Sound::EntityArmadilloAmbient)
    }
}
