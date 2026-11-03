use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::particle::Particle;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::block::entities::beehive::BeehiveBlockEntity;
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::ai::util::air_random_pos;
use crate::entity::custom_sound::CustomSound;
use crate::entity::passive::bee_goals;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::control::{
        Control, LookControlTrait, flying_move_control::FlyingMoveControl,
        look_control::LookControl,
    },
    ai::goal::{
        active_target::TargetCondition, breed::BreedGoal, follow_parent::FollowParentGoal,
        melee_attack::MeleeAttackGoal, reset_universal_anger::ResetUniversalAngerGoal,
        revenge::RevengeGoal, swim::SwimGoal, tempt::TemptGoal,
    },
    ai::pathfinder::Navigator,
    mob::{
        Mob, MobEntity,
        neutral::{NeutralData, NeutralMob},
    },
    passive::animal::Animal,
    player::Player,
};

pub const FLAG_ROLL: u8 = 2;
pub const FLAG_HAS_STUNG: u8 = 4;
pub const FLAG_HAS_NECTAR: u8 = 8;

const COOLDOWN_BEFORE_LOCATING_NEW_HIVE: i32 = 200;
const TICKS_WITHOUT_NECTAR_BEFORE_GOING_HOME: i32 = 3600;
const TOO_FAR_DISTANCE: i32 = 48;

/// Vanilla `Bee.TICKS_PER_FLAP`, `Mth.ceil(1.4959966F)`.
const TICKS_PER_FLAP: i32 = 2;

// bee: TemptGoal item predicate, ItemTags.BEE_FOOD
fn is_bee_food(_mob: &dyn Mob, stack: &ItemStack) -> bool {
    stack.item.has_tag(&tag::Item::MINECRAFT_BEE_FOOD)
}

fn is_angry(mob: &dyn Mob) -> bool {
    mob.as_neutral().is_some_and(NeutralMob::is_angry)
}

/// Angry and still holding its stinger.
fn can_sting(mob: &dyn Mob) -> bool {
    is_angry(mob)
        && !mob
            .cast_any()
            .downcast_ref::<BeeEntity>()
            .is_some_and(BeeEntity::has_stung)
}

const BEE_HUNTS: &[Hunt] = &[Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true)
    .when(TargetCondition::AngryAt)
    .gated_by(can_sting)];

pub struct BeeEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub neutral_data: NeutralData,
    pub flags: AtomicU8,
    pub ticks_without_nectar: AtomicI32,
    pub cannot_enter_hive_ticks: AtomicI32,
    pub crops_grown_since_pollination: AtomicI32,
    pub time_since_sting: AtomicI32,
    pub under_water_ticks: AtomicI32,
    pub hive_pos: Mutex<Option<BlockPos>>,
    pub saved_flower_pos: Mutex<Option<BlockPos>>,
    pub cooldown_before_locating_new_hive: AtomicI32,
    pub cooldown_before_locating_new_flower: AtomicI32,
    pub pollinating: AtomicBool,
    pub blacklisted_hives: Mutex<Vec<BlockPos>>,
}

impl BeeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(FlyingMoveControl::new(20, true));
        *mob_entity
            .look_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(BeeLookControl::default());
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *navigator = Navigator::bee_flying();
        navigator.set_can_open_doors(false);
        navigator.set_can_float(false);
        navigator.set_required_path_length(48.0);
        // bee: pathfinding malus (Bee constructor)
        navigator.set_pathfinding_malus(PathType::DamageFire, -1.0);
        navigator.set_pathfinding_malus(PathType::Water, -1.0);
        navigator.set_pathfinding_malus(PathType::WaterBorder, 16.0);
        navigator.set_pathfinding_malus(PathType::Cocoa, -1.0);
        navigator.set_pathfinding_malus(PathType::Fence, -1.0);
        // bee: getLeashOffset, (0, 0.5 * eyeHeight, width * 0.2)
        mob_entity
            .living_entity
            .entity
            .leash_offset_factors
            .store(Some((0.5, 0.2)));
        let bee = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            neutral_data: NeutralData::default(),
            flags: AtomicU8::new(0),
            ticks_without_nectar: AtomicI32::new(0),
            cannot_enter_hive_ticks: AtomicI32::new(0),
            crops_grown_since_pollination: AtomicI32::new(0),
            time_since_sting: AtomicI32::new(0),
            under_water_ticks: AtomicI32::new(0),
            hive_pos: Mutex::new(None),
            saved_flower_pos: Mutex::new(None),
            cooldown_before_locating_new_hive: AtomicI32::new(0),
            cooldown_before_locating_new_flower: AtomicI32::new(rand::random_range(20..=60)),
            pollinating: AtomicBool::new(false),
            blacklisted_hives: Mutex::new(Vec::new()),
        };
        let mob_arc = Arc::new(bee);
        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(
                0,
                Box::new(MeleeAttackGoal::new(1.4, true).gated_by(can_sting)),
            );
            goal_selector.add_goal(1, Box::new(bee_goals::EnterHiveGoal));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(
                3,
                Box::new(TemptGoal::new(1.25, &[], false).with_item_predicate(is_bee_food)),
            );
            goal_selector.add_goal(3, Box::new(bee_goals::ValidateHiveGoal::new()));
            goal_selector.add_goal(3, Box::new(bee_goals::ValidateFlowerGoal::new()));
            goal_selector.add_goal(4, Box::new(bee_goals::PollinateGoal::new()));
            goal_selector.add_goal(5, Box::new(FollowParentGoal::new(1.25)));
            goal_selector.add_goal(5, Box::new(bee_goals::LocateHiveGoal));
            goal_selector.add_goal(5, Box::new(bee_goals::GoToHiveGoal::new()));
            goal_selector.add_goal(6, Box::new(bee_goals::GoToKnownFlowerGoal::new()));
            goal_selector.add_goal(7, Box::new(bee_goals::GrowCropGoal));
            goal_selector.add_goal(8, Box::new(bee_goals::BeeWanderGoal));
            goal_selector.add_goal(9, Box::new(SwimGoal::default()));
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;

            // Vanilla BeeHurtByOtherGoal.
            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .alerting_others()
                        .alerting_in_sight()
                        .continuing_while(is_angry),
                ),
            );
            hunting::apply(target_selector, &mob_arc.mob_entity, BEE_HUNTS);
            target_selector.add_goal(3, ResetUniversalAngerGoal::new(true));
        };

        mob_arc
    }

    #[must_use]
    pub fn has_flag(&self, flag: u8) -> bool {
        (self.flags.load(Ordering::Relaxed) & flag) != 0
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        let current = self.flags.load(Ordering::Relaxed);
        let new_flags = if val { current | flag } else { current & !flag };
        self.flags.store(new_flags, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::bee::DATA_FLAGS_ID,
            new_flags as i8,
        );
    }

    #[must_use]
    pub fn has_nectar(&self) -> bool {
        self.has_flag(FLAG_HAS_NECTAR)
    }

    pub fn set_has_nectar(&self, val: bool) {
        self.set_flag(FLAG_HAS_NECTAR, val);
    }

    #[must_use]
    pub fn has_stung(&self) -> bool {
        self.has_flag(FLAG_HAS_STUNG)
    }

    pub fn set_has_stung(&self, val: bool) {
        self.set_flag(FLAG_HAS_STUNG, val);
    }

    fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        cell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn hive_pos(&self) -> Option<BlockPos> {
        *Self::lock(&self.hive_pos)
    }

    pub fn set_hive_pos(&self, pos: BlockPos) {
        *Self::lock(&self.hive_pos) = Some(pos);
    }

    #[must_use]
    pub fn saved_flower_pos(&self) -> Option<BlockPos> {
        *Self::lock(&self.saved_flower_pos)
    }

    #[must_use]
    pub fn has_saved_flower_pos(&self) -> bool {
        self.saved_flower_pos().is_some()
    }

    pub fn set_saved_flower_pos(&self, pos: BlockPos) {
        *Self::lock(&self.saved_flower_pos) = Some(pos);
    }

    #[must_use]
    pub fn has_hive(&self) -> bool {
        self.hive_pos().is_some()
    }

    // bee: dropHive, 200 tick cooldown
    pub fn drop_hive(&self) {
        *Self::lock(&self.hive_pos) = None;
        self.cooldown_before_locating_new_hive
            .store(COOLDOWN_BEFORE_LOCATING_NEW_HIVE, Ordering::Relaxed);
    }

    // bee: dropFlower, 20 to 60 tick cooldown
    pub fn drop_flower(&self) {
        *Self::lock(&self.saved_flower_pos) = None;
        self.cooldown_before_locating_new_flower
            .store(rand::random_range(20..=60), Ordering::Relaxed);
    }

    pub fn set_stay_out_of_hive_countdown(&self, ticks: i32) {
        self.cannot_enter_hive_ticks.store(ticks, Ordering::Relaxed);
    }

    // bee: dropOffNectar
    pub fn drop_off_nectar(&self) {
        self.set_has_nectar(false);
        self.crops_grown_since_pollination
            .store(0, Ordering::Relaxed);
    }

    // bee: setHasNectar resets the nectar timer
    pub fn set_nectar(&self, val: bool) {
        if val {
            self.ticks_without_nectar.store(0, Ordering::Relaxed);
        }
        self.set_has_nectar(val);
    }

    // bee: BeehiveBlockEntity.setBeeReleaseData
    pub fn apply_release_data(&self, ticks_in_hive: i32) {
        if !self.is_age_locked() {
            let age = self.get_age();
            if age < 0 {
                self.set_age((age + ticks_in_hive).min(0));
            } else if age > 0 {
                self.set_age((age - ticks_in_hive).max(0));
            }
        }
        let love = &self.mob_entity.love_ticks;
        love.store(
            (love.load(Ordering::Relaxed) - ticks_in_hive).max(0),
            Ordering::Relaxed,
        );
    }

    #[must_use]
    pub fn is_pollinating(&self) -> bool {
        self.pollinating.load(Ordering::Relaxed)
    }

    // bee: Bee.closerThan, BlockPos.closerThan(Vec3i, distance)
    #[must_use]
    pub fn closer_than(&self, pos: &BlockPos, distance: i32) -> bool {
        let here = self.get_entity().block_pos.load();
        let (dx, dy, dz) = (
            i64::from(pos.0.x - here.0.x),
            i64::from(pos.0.y - here.0.y),
            i64::from(pos.0.z - here.0.z),
        );
        dx * dx + dy * dy + dz * dz < i64::from(distance) * i64::from(distance)
    }

    #[must_use]
    pub fn is_too_far_away(&self, pos: &BlockPos) -> bool {
        !self.closer_than(pos, TOO_FAR_DISTANCE)
    }

    // bee: getBeehiveBlockEntity, null when the hive is more than 48 blocks away
    pub fn with_hive<R>(&self, f: impl FnOnce(&BeehiveBlockEntity) -> R) -> Option<R> {
        let pos = self.hive_pos()?;
        if self.is_too_far_away(&pos) {
            return None;
        }
        let world = self.get_entity().world.load();
        let block_entity = world.get_block_entity(&pos)?;
        block_entity
            .as_any()
            .downcast_ref::<BeehiveBlockEntity>()
            .map(f)
    }

    #[must_use]
    pub fn is_hive_valid(&self) -> bool {
        self.with_hive(|_| ()).is_some()
    }

    // bee: isTiredOfLookingForNectar
    #[must_use]
    pub fn is_tired_of_looking_for_nectar(&self) -> bool {
        self.ticks_without_nectar.load(Ordering::Relaxed) > TICKS_WITHOUT_NECTAR_BEFORE_GOING_HOME
    }

    // bee: wantsToEnterHive
    #[must_use]
    pub fn wants_to_enter_hive(&self) -> bool {
        if self.cannot_enter_hive_ticks.load(Ordering::Relaxed) > 0
            || self.is_pollinating()
            || self.has_stung()
            || self.mob_entity.get_target().is_some()
        {
            return false;
        }
        let pos = self.get_entity().block_pos.load();
        let world = self.get_entity().world.load();
        (self.has_nectar()
            || self.is_tired_of_looking_for_nectar()
            || world.bees_stay_in_hive(&pos))
            && !self
                .with_hive(|hive| hive.is_fire_nearby(&world))
                .unwrap_or(false)
    }

    // bee: pathfindRandomlyTowards
    pub fn pathfind_randomly_towards(&self, target: &BlockPos) {
        let target_vec = Vector3::new(
            f64::from(target.0.x) + 0.5,
            f64::from(target.0.y),
            f64::from(target.0.z) + 0.5,
        );
        let here = self.get_entity().block_pos.load();
        let y_delta = target_vec.y as i32 - here.0.y;
        let y_adjust = if y_delta > 2 {
            4
        } else if y_delta < -2 {
            -4
        } else {
            0
        };
        let (mut xz_dist, mut y_dist) = (6, 8);
        let dist = here.manhattan_distance(*target);
        if dist < 15 {
            xz_dist = dist / 2;
            y_dist = dist / 2;
        }
        if let Some(next) = air_random_pos::get_pos_towards(
            self,
            xz_dist,
            y_dist,
            y_adjust,
            target_vec,
            std::f64::consts::PI / 10.0,
        ) {
            let mut navigator = self
                .mob_entity
                .navigator
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            navigator.set_max_visited_nodes_multiplier(0.5);
            navigator.move_to_coords(next.x, next.y, next.z, 1.0, &self.mob_entity.living_entity);
        }
    }

    #[must_use]
    pub fn is_rolling(&self) -> bool {
        self.has_flag(FLAG_ROLL)
    }

    pub fn set_rolling(&self, val: bool) {
        self.set_flag(FLAG_ROLL, val);
    }
}

impl AgeableMob for BeeEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for BeeEntity {
    // bee: Bee.getWalkTargetValue
    fn animal_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let world = self.get_entity().world.load();
        if world.get_block_state(pos).is_air() {
            10.0
        } else {
            0.0
        }
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_BEE_FOOD)
    }
}

// bee: getHurtSound, getDeathSound, getSoundVolume
impl CustomSound for BeeEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBeeDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBeeHurt)
    }

    fn sound_volume(&self) -> f32 {
        0.4
    }
}

impl NeutralMob for BeeEntity {
    fn updates_anger_in_ai_step(&self) -> bool {
        true
    }

    fn get_neutral_data(&self) -> &NeutralData {
        &self.neutral_data
    }

    /// Bees calm down once the target is gone, unlike the other neutral mobs.
    fn stays_angry_with_target(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct BeeLookControl(LookControl);

impl Control for BeeLookControl {}

impl LookControlTrait for BeeLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if !is_angry(mob) {
            self.0.tick(mob);
        }
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        max_yaw_change: f32,
        max_pitch_change: f32,
    ) {
        self.0
            .look_at_with_range(x, y, z, max_yaw_change, max_pitch_change);
    }
}

impl Mob for BeeEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    // bee: jumpInLiquid
    fn liquid_jump_boost(&self) -> f64 {
        0.01
    }

    fn mob_is_flapping(&self) -> bool {
        let entity = self.get_entity();
        !entity.on_ground.load(Ordering::Relaxed)
            && entity.tick_count.load(Ordering::Relaxed) % TICKS_PER_FLAP == 0
    }

    /// Vanilla `Bee` steps silently.
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {}

    fn ticks_navigation(&self) -> bool {
        !self.is_pollinating()
    }

    fn look_resets_pitch(&self) -> bool {
        !self.is_pollinating()
    }

    // bee: sting damage source (Bee.doHurtTarget)
    fn melee_damage_type(&self) -> DamageType {
        DamageType::STING
    }

    // bee: Bee.hurtServer stops pollinating
    fn pre_damage(&self, _damage_type: DamageType, _source: Option<&dyn EntityBase>) -> bool {
        self.pollinating.store(false, Ordering::Relaxed);
        true
    }

    // bee: Bee.doHurtTarget sting effects
    fn on_attack(&self, target: &dyn EntityBase) {
        if let Some(living) = target.get_living_entity() {
            living.set_stinger_count(living.get_stinger_count() + 1);
            let world = self.get_entity().world.load();
            let seconds = match world.level_info.load().difficulty {
                pumpkin_util::Difficulty::Normal => 10,
                pumpkin_util::Difficulty::Hard => 18,
                _ => 0,
            };
            if seconds > 0 {
                living.add_effect(Effect {
                    effect_type: &StatusEffect::POISON,
                    duration: seconds * 20,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                });
            }
        }
        self.set_has_stung(true);
        self.stop_being_angry();
        self.get_entity().world.load().play_sound_fine(
            Sound::EntityBeeSting,
            pumpkin_data::sound::SoundCategory::Neutral,
            &self.get_entity().pos.load(),
            1.0,
            1.0,
        );
    }

    fn omnidirectional_air_mover(&self) -> bool {
        true
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        if let Some(pos) = self.hive_pos() {
            nbt.put(
                "hive_pos",
                NbtTag::IntArray(vec![pos.0.x, pos.0.y, pos.0.z]),
            );
        }
        if let Some(pos) = self.saved_flower_pos() {
            nbt.put(
                "flower_pos",
                NbtTag::IntArray(vec![pos.0.x, pos.0.y, pos.0.z]),
            );
        }
        nbt.put_bool("HasNectar", self.has_nectar());
        nbt.put_bool("HasStung", self.has_stung());
        nbt.put_int(
            "TicksSincePollination",
            self.ticks_without_nectar.load(Ordering::Relaxed),
        );
        nbt.put_int(
            "CannotEnterHiveTicks",
            self.cannot_enter_hive_ticks.load(Ordering::Relaxed),
        );
        nbt.put_int(
            "CropsGrownSincePollination",
            self.crops_grown_since_pollination.load(Ordering::Relaxed),
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        let read_pos = |name: &str| {
            nbt.get_int_array(name)
                .and_then(|arr| (arr.len() == 3).then(|| BlockPos::new(arr[0], arr[1], arr[2])))
        };
        *Self::lock(&self.hive_pos) = read_pos("hive_pos");
        *Self::lock(&self.saved_flower_pos) = read_pos("flower_pos");
        if let Some(nectar) = nbt.get_bool("HasNectar") {
            self.set_has_nectar(nectar);
        }
        if let Some(stung) = nbt.get_bool("HasStung") {
            self.set_has_stung(stung);
        }
        if let Some(ticks) = nbt.get_int("TicksSincePollination") {
            self.ticks_without_nectar.store(ticks, Ordering::Relaxed);
        }
        if let Some(cannot) = nbt.get_int("CannotEnterHiveTicks") {
            self.cannot_enter_hive_ticks
                .store(cannot, Ordering::Relaxed);
        }
        if let Some(crops) = nbt.get_int("CropsGrownSincePollination") {
            self.crops_grown_since_pollination
                .store(crops, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // bee: Bee.customServerAiStep and the server half of Bee.aiStep
    fn custom_server_ai_step(&self, caller: &dyn EntityBase) {
        let entity = self.get_entity();
        if entity.touching_water.load(Ordering::Relaxed) {
            self.under_water_ticks.fetch_add(1, Ordering::Relaxed);
        } else {
            self.under_water_ticks.store(0, Ordering::Relaxed);
        }
        if self.under_water_ticks.load(Ordering::Relaxed) > 20 {
            caller.damage_with_context(caller, 1.0, DamageType::DROWN, None, None, None);
        }
        let living = &self.mob_entity.living_entity;
        if self.has_stung() {
            let time = self.time_since_sting.fetch_add(1, Ordering::Relaxed) + 1;
            if time % 5 == 0 && rand::random_range(0..(1200 - time).clamp(1, 1200)) == 0 {
                caller.damage_with_context(
                    caller,
                    living.health.load(),
                    DamageType::GENERIC,
                    None,
                    None,
                    None,
                );
            }
        }
        if !self.has_nectar() {
            self.ticks_without_nectar.fetch_add(1, Ordering::Relaxed);
        }
        self.update_persistent_anger();
        // bee: DATA_ANGER_END_TIME drives the angry pose on the client
        entity.set_synced_data(
            pumpkin_data::tracked_data::bee::DATA_ANGER_END_TIME,
            self.get_anger_end_time(),
        );

        for cooldown in [
            &self.cannot_enter_hive_ticks,
            &self.cooldown_before_locating_new_hive,
            &self.cooldown_before_locating_new_flower,
        ] {
            if cooldown.load(Ordering::Relaxed) > 0 {
                cooldown.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let rolling = is_angry(self)
            && !self.has_stung()
            && self.mob_entity.get_target().is_some_and(|target| {
                target
                    .get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&entity.pos.load())
                    < 4.0
            });
        self.set_rolling(rolling);
        if entity.tick_count.load(Ordering::Relaxed) % 20 == 0 && !self.is_hive_valid() {
            *Self::lock(&self.hive_pos) = None;
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::bee::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::bee::DATA_FLAGS_ID,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    // bee: Bee.mobInteract, flower with a bee interaction effect (eyeblossom)
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if self.is_food(item_stack)
            && (item_stack.item == &Item::OPEN_EYEBLOSSOM
                || item_stack.item == &Item::CLOSED_EYEBLOSSOM)
        {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            self.mob_entity.living_entity.add_effect(Effect {
                effect_type: &StatusEffect::POISON,
                duration: 25,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
            return true;
        }
        self.animal_interact(player, item_stack, Sound::EntityBeePollinate)
    }

    // bee: Bee.tick falling nectar particles
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        if self.has_nectar()
            && self.crops_grown_since_pollination.load(Ordering::Relaxed) < 10
            && rand::random::<f32>() < 0.05
        {
            let entity = self.get_entity();
            let world = entity.world.load();
            let pos = entity.pos.load();
            for _ in 0..=rand::random_range(0..2) {
                world.spawn_particle(
                    Vector3::new(
                        pos.x + f64::from(rand::random::<f32>() * 0.6 - 0.3),
                        pos.y + f64::from(entity.entity_type.dimension[1]) * 0.5,
                        pos.z + f64::from(rand::random::<f32>() * 0.6 - 0.3),
                    ),
                    Vector3::new(0.0, 0.0, 0.0),
                    0.0,
                    1,
                    Particle::FallingNectar,
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn bee_in(world: &Arc<crate::world::World>) -> Arc<BeeEntity> {
        BeeEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::BEE,
        ))
    }

    #[tokio::test]
    async fn only_bee_food_tempts_and_breeds_bees() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let bee = bee_in(&world);
        let dandelion = ItemStack::new(1, &Item::DANDELION);
        let closed = ItemStack::new(1, &Item::CLOSED_EYEBLOSSOM);
        let stick = ItemStack::new(1, &Item::STICK);
        assert!(bee.is_food(&dandelion));
        assert!(!bee.is_food(&closed));
        assert!(!bee.is_food(&stick));
        assert!(is_bee_food(bee.as_ref(), &dandelion));
        assert!(!is_bee_food(bee.as_ref(), &stick));
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn stingers_fall_out_one_at_a_time() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let bee = bee_in(&world);
        let living = &bee.mob_entity.living_entity;
        living.set_stinger_count(2);
        // 20 * (30 - 2) ticks until the first one drops
        for _ in 0..559 {
            living.tick_stingers();
        }
        assert_eq!(living.get_stinger_count(), 2);
        living.tick_stingers();
        assert_eq!(living.get_stinger_count(), 1);
        world.level.shutdown().await;
        Ok(())
    }
}
