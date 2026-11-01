use std::sync::{Arc, atomic::Ordering};

use pumpkin_data::{
    Block,
    block_properties::{CaveVinesLikeProperties, NetherWartLikeProperties},
    entity::EntityType,
    item::Item,
    item_stack::ItemStack,
    sound::{Sound, SoundCategory},
};
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use pumpkin_world::chunk::ChunkHeightmapType;
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

use super::FoxEntity;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        Controls, Goal, ParentHandle,
        move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal},
        to_goal_ticks,
    },
    ai::target_match::TargetMatch,
    ai::util::land_random_pos,
    item::ItemEntity,
    mob::Mob,
};
use crate::world::World;

// fox: constants (Fox)
const WAIT_TIME_BEFORE_SLEEP: i32 = 140;
const BERRY_WAIT_TICKS: i32 = 40;
const BERRY_SEARCH_RANGE: i32 = 12;
const BERRY_VERTICAL_RANGE: i32 = 1;
const ITEM_SEARCH_RANGE: f64 = 8.0;
const ALERT_RANGE: f64 = 12.0;
const ALERT_HEIGHT: f64 = 6.0;
const SHELTER_INTERVAL: i32 = 100;
const VILLAGE_STROLL_INTERVAL: i32 = 200;
const VILLAGE_STROLL_DISTANCE_THRESHOLD: f64 = 10.0;
const SPIT_PICKUP_DELAY: u8 = 40;
pub const TICKS_TO_EAT: i32 = 600;

fn fox(mob: &dyn Mob) -> Option<&FoxEntity> {
    mob.cast_any().downcast_ref::<FoxEntity>()
}

fn nav_stop(mob: &dyn Mob) {
    mob.get_mob_entity()
        .navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stop();
}

fn nav_move_to(mob: &dyn Mob, x: f64, y: f64, z: f64, speed: f64) -> bool {
    mob.get_mob_entity()
        .navigator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .move_to_coords(x, y, z, speed, &mob.get_mob_entity().living_entity)
}

// fox: FoxBehaviorGoal.hasShelter
fn has_shelter(mob: &dyn Mob) -> bool {
    let entity = mob.get_entity();
    let world = entity.world.load();
    let top = BlockPos::new(
        entity.pos.load().x.floor() as i32,
        entity.bounding_box.load().max.y.floor() as i32,
        entity.pos.load().z.floor() as i32,
    );
    !world.can_see_sky(&top) && mob.get_walk_target_value(&top) >= 0.0
}

// fox: FoxBehaviorGoal.alertable with the FoxAlertableEntitiesSelector
fn alertable(mob: &dyn Mob) -> bool {
    let entity = mob.get_entity();
    let world = entity.world.load();
    let pos = entity.pos.load();
    let area = BoundingBox::new(
        Vector3::new(
            pos.x - ALERT_RANGE,
            pos.y - ALERT_HEIGHT,
            pos.z - ALERT_RANGE,
        ),
        Vector3::new(
            pos.x + ALERT_RANGE,
            pos.y + ALERT_HEIGHT,
            pos.z + ALERT_RANGE,
        ),
    );
    let Some(own) = fox(mob) else { return false };
    world.get_all_at_box(&area).iter().any(|other| {
        let other_entity = other.get_entity();
        if other_entity.entity_id == entity.entity_id
            || other_entity.entity_type == &EntityType::FOX
            || other.get_living_entity().is_none()
        {
            return false;
        }
        if other_entity.pos.load().squared_distance_to_vec(&pos) > ALERT_RANGE * ALERT_RANGE {
            return false;
        }
        let kind = other_entity.entity_type;
        if kind == &EntityType::CHICKEN
            || kind == &EntityType::RABBIT
            || TargetMatch::Enemy.matches(kind)
        {
            return true;
        }
        if let Some(tamable) = other
            .get_mob()
            .and_then(|mob| mob.as_tamable())
            .filter(|_| true)
        {
            return !tamable.is_tame();
        }
        if let Some(player) = other.get_player()
            && (player.is_spectator() || player.is_creative())
        {
            return false;
        }
        !own.trusts(other_entity.entity_uuid)
            && !other
                .get_player()
                .is_some_and(crate::entity::player::Player::is_sleeping)
            && !other_entity.is_sneaking()
    })
}

// fox: Fox.SleepGoal
pub struct SleepGoal {
    countdown: i32,
}

impl SleepGoal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            countdown: rand::random_range(0..to_goal_ticks(WAIT_TIME_BEFORE_SLEEP)),
        }
    }

    fn can_sleep(&mut self, mob: &dyn Mob) -> bool {
        if self.countdown > 0 {
            self.countdown -= 1;
            return false;
        }
        mob.get_entity().world.load().is_bright_outside()
            && has_shelter(mob)
            && !alertable(mob)
            && !mob.get_entity().is_in_powder_snow()
    }
}

impl Default for SleepGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for SleepGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let input = mob.get_mob_entity().living_entity.movement_input.load();
        if input.x != 0.0 || input.y != 0.0 || input.z != 0.0 {
            return false;
        }
        self.can_sleep(mob) || fox(mob).is_some_and(FoxEntity::is_sleeping)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_sleep(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(fox) = fox(mob) else { return };
        fox.set_sitting(false);
        fox.set_crouching(false);
        fox.set_interested(false);
        mob.get_mob_entity()
            .living_entity
            .jumping
            .store(false, Ordering::Relaxed);
        fox.set_sleeping(true);
        nav_stop(mob);
        let pos = mob.get_entity().pos.load();
        mob.get_mob_entity()
            .steered()
            .move_control()
            .set_wanted_position(pos.x, pos.y, pos.z, 0.0);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.countdown = rand::random_range(0..to_goal_ticks(WAIT_TIME_BEFORE_SLEEP));
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK | Controls::JUMP
    }
}

// fox: Fox.PerchAndSearchGoal
pub struct PerchAndSearchGoal {
    rel_x: f64,
    rel_z: f64,
    look_time: i32,
    looks_remaining: i32,
}

impl PerchAndSearchGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rel_x: 0.0,
            rel_z: 0.0,
            look_time: 0,
            looks_remaining: 0,
        }
    }

    fn reset_look(&mut self, mob: &dyn Mob) {
        let mut rng = mob.get_random();
        let angle = std::f64::consts::TAU * rng.random::<f64>();
        self.rel_x = angle.cos();
        self.rel_z = angle.sin();
        self.look_time = to_goal_ticks(80 + rng.random_range(0..20));
    }
}

impl Default for PerchAndSearchGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for PerchAndSearchGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(fox) = fox(mob) else { return false };
        mob.get_mob_entity()
            .living_entity
            .get_last_damage_source()
            .is_none()
            && mob.get_random().random::<f32>() < 0.02
            && !fox.is_sleeping()
            && mob.get_mob_entity().get_target().is_none()
            && mob.is_navigator_idle()
            && !alertable(mob)
            && !fox.is_pouncing()
            && !fox.is_crouching()
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.looks_remaining > 0
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.reset_look(mob);
        self.looks_remaining = 2 + mob.get_random().random_range(0..3);
        if let Some(fox) = fox(mob) {
            fox.set_sitting(true);
        }
        nav_stop(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(fox) = fox(mob) {
            fox.set_sitting(false);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.look_time -= 1;
        if self.look_time <= 0 {
            self.looks_remaining -= 1;
            self.reset_look(mob);
        }
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        mob.get_mob_entity()
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_with_range(
                pos.x + self.rel_x,
                pos.y + entity.get_eye_height(),
                pos.z + self.rel_z,
                mob.get_max_look_yaw_change(),
                mob.get_max_look_pitch_change(),
            );
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// fox: Fox.SeekShelterGoal (FleeSunGoal)
pub struct SeekShelterGoal {
    speed: f64,
    interval: i32,
    wanted: Option<Vector3<f64>>,
}

impl SeekShelterGoal {
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self {
            speed,
            interval: SHELTER_INTERVAL,
            wanted: None,
        }
    }

    // fox: FleeSunGoal.getHidePos, ten tries for a covered spot with a negative walk value
    fn set_wanted_pos(&mut self, mob: &dyn Mob) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let pos = entity.block_pos.load();
        let mut rng = mob.get_random();
        for _ in 0..10 {
            let candidate = BlockPos::new(
                pos.0.x + rng.random_range(0..20) - 10,
                pos.0.y + rng.random_range(0..6) - 3,
                pos.0.z + rng.random_range(0..20) - 10,
            );
            if !world.can_see_sky(&candidate) && mob.get_walk_target_value(&candidate) < 0.0 {
                self.wanted = Some(Vector3::new(
                    f64::from(candidate.0.x) + 0.5,
                    f64::from(candidate.0.y),
                    f64::from(candidate.0.z) + 0.5,
                ));
                return true;
            }
        }
        false
    }
}

impl Goal for SeekShelterGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(fox) = fox(mob) else { return false };
        if fox.is_sleeping() || mob.get_mob_entity().get_target().is_some() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load();
        let pos = entity.block_pos.load();
        if world.is_thundering() && world.can_see_sky(&pos) {
            return self.set_wanted_pos(mob);
        }
        if self.interval > 0 {
            self.interval -= 1;
            return false;
        }
        self.interval = SHELTER_INTERVAL;
        world.is_bright_outside()
            && world.can_see_sky(&pos)
            && !world.is_village(&pos)
            && self.set_wanted_pos(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        !mob.is_navigator_idle()
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
        if let Some(wanted) = self.wanted {
            nav_move_to(mob, wanted.x, wanted.y, wanted.z, self.speed);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// fox: Fox.FoxEatBerriesGoal target, ripe sweet berry bushes and glow berries
struct BerryTarget;
static BERRY_TARGET: BerryTarget = BerryTarget;

impl MoveToTargetPos for BerryTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        let (block, state_id) = world.get_block_and_state_id(&block_pos);
        if block == &Block::SWEET_BERRY_BUSH {
            return NetherWartLikeProperties::from_state_id(state_id).age >= 2;
        }
        (block == &Block::CAVE_VINES || block == &Block::CAVE_VINES_PLANT)
            && CaveVinesLikeProperties::from_state_id(state_id).berries
    }

    fn get_desired_distance_to_target(&self) -> f64 {
        2.0
    }
}

// fox: Fox.FoxEatBerriesGoal
pub struct EatBerriesGoal {
    inner: MoveToTargetPosGoal<BerryTarget>,
    ticks_waited: i32,
}

impl EatBerriesGoal {
    #[must_use]
    pub fn new(speed: f64) -> Self {
        let mut inner = MoveToTargetPosGoal::new(
            // SAFETY: BERRY_TARGET is a static.
            unsafe { ParentHandle::new(&BERRY_TARGET) },
            speed,
            BERRY_SEARCH_RANGE,
            BERRY_VERTICAL_RANGE,
        );
        inner.reset_path_interval = 100;
        Self {
            inner,
            ticks_waited: 0,
        }
    }

    // fox: pickSweetBerries / pickGlowBerry
    fn on_reached_target(&self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        let world = entity.world.load_full();
        if !world.level_info.load().game_rules.mob_griefing {
            return;
        }
        let pos = self.inner.target_pos;
        let (block, state_id) = world.get_block_and_state_id(&pos);
        if block == &Block::SWEET_BERRY_BUSH {
            let mut props = NetherWartLikeProperties::from_state_id(state_id);
            let age = props.age;
            let mut count = 1 + mob.get_random().random_range(0..2) + i32::from(age == 3);
            if let Some(fox) = fox(mob)
                && fox.mouth_item().is_empty()
            {
                fox.set_mouth_item(ItemStack::new(1, &Item::SWEET_BERRIES));
                count -= 1;
            }
            if count > 0 {
                let item = ItemEntity::new(
                    Entity::new(world.clone(), pos.to_centered_f64(), &EntityType::ITEM),
                    ItemStack::new(count as u8, &Item::SWEET_BERRIES),
                );
                world.spawn_entity(Arc::new(item));
            }
            entity.play_sound(Sound::BlockSweetBerryBushPickBerries);
            props.age = 1;
            world.set_block_state(&pos, props.to_state_id(block), BlockFlags::NOTIFY_LISTENERS);
        } else if block == &Block::CAVE_VINES || block == &Block::CAVE_VINES_PLANT {
            let mut props = CaveVinesLikeProperties::from_state_id(state_id);
            if props.berries {
                props.berries = false;
                let item = ItemEntity::new(
                    Entity::new(world.clone(), pos.to_centered_f64(), &EntityType::ITEM),
                    ItemStack::new(1, &Item::GLOW_BERRIES),
                );
                world.spawn_entity(Arc::new(item));
                world.play_sound_fine(
                    Sound::BlockCaveVinesPickBerries,
                    SoundCategory::Blocks,
                    &pos.to_centered_f64(),
                    1.0,
                    0.8 + rand::random::<f32>() * 0.4,
                );
                world.set_block_state(&pos, props.to_state_id(block), BlockFlags::NOTIFY_LISTENERS);
            }
        }
    }
}

impl Goal for EatBerriesGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        fox(mob).is_some_and(|fox| !fox.is_sleeping()) && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.ticks_waited = 0;
        if let Some(fox) = fox(mob) {
            fox.set_sitting(false);
        }
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        if self.inner.is_reached_target() {
            if self.ticks_waited >= BERRY_WAIT_TICKS {
                self.on_reached_target(mob);
            } else {
                self.ticks_waited += 1;
            }
        } else if mob.get_random().random::<f32>() < 0.05 {
            mob.get_entity().play_sound(Sound::EntityFoxSniff);
        }
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// fox: Fox.FoxSearchForItemsGoal, items without pickup delay within 8 blocks
pub struct SearchForItemsGoal;

impl SearchForItemsGoal {
    fn nearby_items(mob: &dyn Mob) -> Vec<Arc<dyn EntityBase>> {
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        let area = BoundingBox::new(
            Vector3::new(
                pos.x - ITEM_SEARCH_RANGE,
                pos.y - ITEM_SEARCH_RANGE,
                pos.z - ITEM_SEARCH_RANGE,
            ),
            Vector3::new(
                pos.x + ITEM_SEARCH_RANGE,
                pos.y + ITEM_SEARCH_RANGE,
                pos.z + ITEM_SEARCH_RANGE,
            ),
        );
        entity
            .world
            .load()
            .get_entities_at_box(&area)
            .into_iter()
            .filter(|candidate| {
                candidate
                    .cast_any()
                    .downcast_ref::<ItemEntity>()
                    .is_some_and(|item| {
                        item.get_pickup_delay() == 0 && item.get_entity().is_alive()
                    })
            })
            .collect()
    }
}

impl Goal for SearchForItemsGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(fox) = fox(mob) else { return false };
        if !fox.mouth_item().is_empty()
            || mob.get_mob_entity().get_target().is_some()
            || mob
                .get_mob_entity()
                .living_entity
                .get_last_damage_source()
                .is_some()
            || !fox.can_move()
            || mob.get_random().random_range(0..to_goal_ticks(10)) != 0
        {
            return false;
        }
        !Self::nearby_items(mob).is_empty()
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(item) = Self::nearby_items(mob).first() {
            let pos = item.get_entity().pos.load();
            nav_move_to(mob, pos.x, pos.y, pos.z, 1.2);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(fox) = fox(mob) else { return };
        if fox.mouth_item().is_empty()
            && let Some(item) = Self::nearby_items(mob).first()
        {
            let pos = item.get_entity().pos.load();
            nav_move_to(mob, pos.x, pos.y, pos.z, 1.2);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// fox: Fox.FoxStrollThroughVillageGoal (StrollThroughVillageGoal) at night
pub struct StrollThroughVillageGoal {
    interval: i32,
    wanted: Option<BlockPos>,
}

impl StrollThroughVillageGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            interval: VILLAGE_STROLL_INTERVAL,
            wanted: None,
        }
    }

    fn can_fox_move(mob: &dyn Mob) -> bool {
        fox(mob).is_some_and(|fox| !fox.is_sleeping() && !fox.is_sitting() && !fox.is_defending())
            && mob.get_mob_entity().get_target().is_none()
    }
}

impl Default for StrollThroughVillageGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for StrollThroughVillageGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if mob.get_entity().has_passengers() {
            return false;
        }
        let entity = mob.get_entity();
        let world = entity.world.load_full();
        if world.is_bright_outside() {
            return false;
        }
        if mob.get_random().random_range(0..self.interval) != 0 {
            return false;
        }
        let pos = entity.block_pos.load();
        if !world.is_close_to_village(&pos, 6) {
            return false;
        }
        let target = land_random_pos::get_pos_weighted(mob, 15, 7, |candidate| {
            -f64::from(world.sections_to_village(candidate))
        });
        self.wanted = target.map(|vec| {
            BlockPos::new(
                vec.x.floor() as i32,
                vec.y.floor() as i32,
                vec.z.floor() as i32,
            )
        });
        self.wanted.is_some() && Self::can_fox_move(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.wanted.is_some()
            && !mob.is_navigator_idle()
            && mob
                .get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_target_pos()
                == self.wanted
            && Self::can_fox_move(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(wanted) = self.wanted else { return };
        if !mob.is_navigator_idle() {
            return;
        }
        let pos = mob.get_entity().pos.load();
        let wanted_center = wanted.to_centered_f64();
        if wanted_center.squared_distance_to_vec(&pos)
            < VILLAGE_STROLL_DISTANCE_THRESHOLD * VILLAGE_STROLL_DISTANCE_THRESHOLD
        {
            return;
        }
        let long_distance = Vector3::new(
            f64::from(wanted.0.x) + 0.5,
            f64::from(wanted.0.y),
            f64::from(wanted.0.z) + 0.5,
        );
        let toward = (pos - long_distance) * 0.4 + long_distance;
        let move_target = (toward - pos).normalize() * 10.0 + pos;
        let world = mob.get_entity().world.load();
        let y = world.get_heightmap_height(
            ChunkHeightmapType::MotionBlockingNoLeaves,
            move_target.x.floor() as i32,
            move_target.z.floor() as i32,
        ) + 1;
        if !nav_move_to(
            mob,
            move_target.x.floor(),
            f64::from(y),
            move_target.z.floor(),
            1.0,
        ) {
            let mut rng = mob.get_random();
            let block = mob.get_entity().block_pos.load();
            let x = block.0.x - 8 + rng.random_range(0..16);
            let z = block.0.z - 8 + rng.random_range(0..16);
            let y =
                world.get_heightmap_height(ChunkHeightmapType::MotionBlockingNoLeaves, x, z) + 1;
            nav_move_to(mob, f64::from(x), f64::from(y), f64::from(z), 1.0);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

impl FoxEntity {
    // fox: Fox.spitOutItem
    pub fn spit_out_item(&self, stack: ItemStack) {
        if stack.is_empty() {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        let look =
            Vector3::rotation_vector(f64::from(entity.pitch.load()), f64::from(entity.yaw.load()));
        let item = ItemEntity::new(
            Entity::new(
                world.clone(),
                Vector3::new(pos.x + look.x, pos.y + 1.0, pos.z + look.z),
                &EntityType::ITEM,
            ),
            stack,
        );
        item.set_pickup_delay(SPIT_PICKUP_DELAY);
        item.set_thrower(entity.entity_uuid);
        entity.play_sound(Sound::EntityFoxSpit);
        world.spawn_entity(Arc::new(item));
    }

    // fox: Fox.canHoldItem
    #[must_use]
    pub fn can_hold(&self, stack: &ItemStack) -> bool {
        let held = self.mouth_item();
        held.is_empty()
            || (self.ticks_since_eaten.load(Ordering::Relaxed) > 0
                && Self::is_consumable_food(stack)
                && !Self::is_consumable_food(&held))
    }

    #[must_use]
    pub fn is_consumable_food(stack: &ItemStack) -> bool {
        stack
            .get_data_component::<pumpkin_data::data_component_impl::FoodImpl>()
            .is_some()
            && stack
                .get_data_component::<pumpkin_data::data_component_impl::ConsumableImpl>()
                .is_some()
    }
}

// fox: Fox.FoxMoveControl, only moves while it can (sleeping, sitting and faceplanting freeze it)
#[derive(Default)]
pub struct FoxMoveControl {
    inner: crate::entity::ai::control::move_control::MoveControl,
}

impl crate::entity::ai::control::Control for FoxMoveControl {}

impl crate::entity::ai::control::MoveControlTrait for FoxMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if fox(mob).is_some_and(FoxEntity::can_move) {
            self.inner.tick(mob);
        }
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.inner.set_wanted_position(x, y, z, speed_modifier);
    }

    fn strafe(&mut self, forwards: f32, right: f32) {
        self.inner.strafe(forwards, right);
    }

    fn has_wanted(&self) -> bool {
        self.inner.has_wanted()
    }

    fn speed_modifier(&self) -> f64 {
        self.inner.speed_modifier
    }
}

// fox: Fox.FoxLookControl, asleep foxes do not look around
#[derive(Default)]
pub struct FoxLookControl {
    inner: crate::entity::ai::control::look_control::LookControl,
}

impl crate::entity::ai::control::Control for FoxLookControl {}

impl crate::entity::ai::control::LookControlTrait for FoxLookControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if !fox(mob).is_some_and(FoxEntity::is_sleeping) {
            self.inner.tick(mob);
        }
    }

    fn looking_at(&self) -> Option<Vector3<f64>> {
        self.inner.looking_at()
    }

    fn look_at_with_range(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        max_yaw_change: f32,
        max_pitch_change: f32,
    ) {
        self.inner
            .look_at_with_range(x, y, z, max_yaw_change, max_pitch_change);
    }
}
