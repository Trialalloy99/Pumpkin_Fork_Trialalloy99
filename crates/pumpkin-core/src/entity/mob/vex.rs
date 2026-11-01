use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::brain::memory::codec::{block_pos_from_tag, block_pos_to_tag};
use crate::entity::ai::control::move_control::Operation;
use crate::entity::ai::control::{Control, MoveControlTrait};
use crate::entity::ai::goal::track_target::TrackTargetGoal;
use crate::entity::ai::goal::{Controls, Goal, to_goal_ticks};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::mob::equipment::RegionalDifficulty;
use crate::entity::mob::raider;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::projectile::owner::EntityReference;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{look_at_entity::LookAtEntityGoal, swim::SwimGoal},
    mob::{Mob, MobEntity},
};
use crate::plugin::api::events::entity::entity_target::TargetReason;
use crate::world::World;

const VEX_HUNTS: &[Hunt] = &[Hunt::new(3, TargetMatch::Type(&EntityType::PLAYER), true)];

const FLAG_IS_CHARGING: u8 = 1;
const TICKS_PER_FLAP: i32 = 4;
const LIMITED_LIFE_DAMAGE_INTERVAL: i32 = 20;

pub struct VexEntity {
    pub mob_entity: MobEntity,
    owner: EntityReference,
    bound_origin: AtomicCell<Option<BlockPos>>,
    has_limited_life: AtomicBool,
    limited_life_ticks: AtomicI32,
    charging: AtomicBool,
}

impl VexEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let vex = Self {
            mob_entity,
            owner: EntityReference::none(),
            bound_origin: AtomicCell::new(None),
            has_limited_life: AtomicBool::new(false),
            limited_life_ticks: AtomicI32::new(0),
            charging: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(vex);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            *mob_arc
                .mob_entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Box::new(VexMoveControl::default());

            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                4,
                Box::new(VexChargeAttackGoal::new(Arc::downgrade(&mob_arc))),
            );
            goal_selector.add_goal(
                8,
                Box::new(VexRandomMoveGoal::new(Arc::downgrade(&mob_arc))),
            );
            goal_selector.add_goal(
                9,
                Box::new(LookAtEntityGoal::new(
                    mob_weak,
                    &EntityType::PLAYER,
                    3.0,
                    1.0,
                    false,
                )),
            );

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, raider::revenge_goal());
            target_selector.add_goal(
                2,
                Box::new(VexCopyOwnerTargetGoal::new(Arc::downgrade(&mob_arc))),
            );
            hunting::apply(target_selector, &mob_arc.mob_entity, VEX_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn is_charging(&self) -> bool {
        self.charging.load(Ordering::Relaxed)
    }

    pub fn set_is_charging(&self, charging: bool) {
        self.charging.store(charging, Ordering::Relaxed);
        self.sync_flags();
    }

    fn sync_flags(&self) {
        let flags: u8 = if self.is_charging() {
            FLAG_IS_CHARGING
        } else {
            0
        };
        self.get_entity()
            .set_synced_data(tracked_data::vex::DATA_FLAGS_ID, flags);
    }

    #[must_use]
    pub fn get_owner(&self) -> Option<Arc<dyn EntityBase>> {
        let world = self.get_entity().world.load();
        self.owner.get(&world)
    }

    pub fn set_owner(&self, owner: &Entity) {
        self.owner.set(Some(owner));
    }

    #[must_use]
    pub fn get_bound_origin(&self) -> Option<BlockPos> {
        self.bound_origin.load()
    }

    pub fn set_bound_origin(&self, bound_origin: Option<BlockPos>) {
        self.bound_origin.store(bound_origin);
    }

    pub fn set_limited_life(&self, life_ticks: i32) {
        self.has_limited_life.store(true, Ordering::Relaxed);
        self.limited_life_ticks.store(life_ticks, Ordering::Relaxed);
    }
}

impl Mob for VexEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_is_flapping(&self) -> bool {
        self.get_entity().tick_count.load(Ordering::Relaxed) % TICKS_PER_FLAP == 0
    }

    fn mob_is_affected_by_blocks(&self) -> bool {
        !self.get_entity().is_removed()
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.get_entity().no_physics.store(true, Ordering::Relaxed);
    }

    fn post_tick(&self) {
        let entity = self.get_entity();
        entity.no_physics.store(false, Ordering::Relaxed);
        entity.set_has_no_gravity(true);
        if self.has_limited_life.load(Ordering::Relaxed)
            && self.limited_life_ticks.fetch_sub(1, Ordering::Relaxed) - 1 <= 0
        {
            self.limited_life_ticks
                .store(LIMITED_LIFE_DAMAGE_INTERVAL, Ordering::Relaxed);
            self.damage(self, 1.0, DamageType::STARVE);
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let difficulty = RegionalDifficulty::at(world, self.get_entity().pos.load());
        self.populate_default_equipment_slots(world, &difficulty);
        self.populate_default_equipment_enchantments(&difficulty);
        self.finalize_spawn_super(group_data)
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        self.mob_entity.set_item_slot(
            &EquipmentSlot::MAIN_HAND,
            ItemStack::new(1, &Item::IRON_SWORD),
        );
        self.mob_entity
            .set_drop_chance(&EquipmentSlot::MAIN_HAND, 0.0);
    }

    fn mob_init_data_tracker(&self) {
        self.sync_flags();
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityVexAmbient)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        if let Some(bound_origin) = self.get_bound_origin() {
            nbt.put("bound_pos", block_pos_to_tag(bound_origin));
        }
        if self.has_limited_life.load(Ordering::Relaxed) {
            nbt.put_int(
                "life_ticks",
                self.limited_life_ticks.load(Ordering::Relaxed),
            );
        }
        self.owner.write_nbt(nbt, "owner");
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_bound_origin(nbt.get("bound_pos").and_then(block_pos_from_tag));
        if let Some(life_ticks) = nbt.get_int("life_ticks") {
            self.set_limited_life(life_ticks);
        } else {
            self.has_limited_life.store(false, Ordering::Relaxed);
        }
        self.owner.clear();
        self.owner.read_nbt(nbt, "owner");
    }
}

fn is_living_target_alive(target: &Arc<dyn EntityBase>) -> bool {
    target
        .get_living_entity()
        .is_some_and(crate::entity::living::LivingEntity::is_alive)
}

#[derive(Default)]
struct VexMoveControl {
    wanted: Vector3<f64>,
    speed_modifier: f64,
    operation: Operation,
}

impl Control for VexMoveControl {}

impl MoveControlTrait for VexMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if self.operation != Operation::MoveTo {
            return;
        }
        let entity = mob.get_entity();
        let delta = self.wanted - entity.pos.load();
        let delta_length = delta.length();
        if delta_length < entity.bounding_box.load().get_average_side_length() {
            self.operation = Operation::Wait;
            entity.velocity.store(entity.velocity.load() * 0.5);
            return;
        }

        let velocity = entity.velocity.load() + delta * (self.speed_modifier * 0.05 / delta_length);
        entity.velocity.store(velocity);
        let (facing_x, facing_z) =
            mob.get_mob_entity()
                .get_target()
                .map_or((velocity.x, velocity.z), |target| {
                    let pos = entity.pos.load();
                    let target_pos = target.get_entity().pos.load();
                    (target_pos.x - pos.x, target_pos.z - pos.z)
                });
        let yaw = -facing_x.atan2(facing_z).to_degrees() as f32;
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.wanted = Vector3::new(x, y, z);
        self.speed_modifier = speed_modifier;
        self.operation = Operation::MoveTo;
    }

    fn has_wanted(&self) -> bool {
        self.operation == Operation::MoveTo
    }
}

fn set_wanted_position(mob: &MobEntity, pos: Vector3<f64>, speed_modifier: f64) {
    mob.move_control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_wanted_position(pos.x, pos.y, pos.z, speed_modifier);
}

fn has_wanted_position(mob: &MobEntity) -> bool {
    mob.move_control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .has_wanted()
}

struct VexChargeAttackGoal {
    vex: Weak<VexEntity>,
}

impl VexChargeAttackGoal {
    const fn new(vex: Weak<VexEntity>) -> Self {
        Self { vex }
    }
}

impl Goal for VexChargeAttackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let mob_entity = mob.get_mob_entity();
        let Some(target) = mob_entity.get_target() else {
            return false;
        };
        if !is_living_target_alive(&target)
            || has_wanted_position(mob_entity)
            || mob.get_random().random_range(0..to_goal_ticks(7)) != 0
        {
            return false;
        }
        mob.get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load())
            > 4.0
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let mob_entity = mob.get_mob_entity();
        has_wanted_position(mob_entity)
            && self.vex.upgrade().is_some_and(|vex| vex.is_charging())
            && mob_entity
                .get_target()
                .is_some_and(|target| is_living_target_alive(&target))
    }

    fn start(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        if let Some(target) = mob_entity.get_target() {
            set_wanted_position(mob_entity, target.get_entity().get_eye_pos(), 1.0);
        }
        if let Some(vex) = self.vex.upgrade() {
            vex.set_is_charging(true);
        }
        mob.play_entity_sound(Sound::EntityVexCharge, 1.0, 1.0);
    }

    fn stop(&mut self, _mob: &dyn Mob) {
        if let Some(vex) = self.vex.upgrade() {
            vex.set_is_charging(false);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        let Some(target) = mob_entity.get_target() else {
            return;
        };
        let entity = mob.get_entity();
        let target_entity = target.get_entity();
        if entity
            .bounding_box
            .load()
            .intersects(&target_entity.bounding_box.load())
        {
            mob_entity.try_attack(mob, target.as_ref());
            if let Some(vex) = self.vex.upgrade() {
                vex.set_is_charging(false);
            }
        } else if entity
            .pos
            .load()
            .squared_distance_to_vec(&target_entity.pos.load())
            < 9.0
        {
            set_wanted_position(mob_entity, target_entity.get_eye_pos(), 1.0);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

struct VexRandomMoveGoal {
    vex: Weak<VexEntity>,
}

impl VexRandomMoveGoal {
    const fn new(vex: Weak<VexEntity>) -> Self {
        Self { vex }
    }
}

impl Goal for VexRandomMoveGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        !has_wanted_position(mob.get_mob_entity())
            && mob.get_random().random_range(0..to_goal_ticks(7)) == 0
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let entity = mob.get_entity();
        let bound_origin = self
            .vex
            .upgrade()
            .and_then(|vex| vex.get_bound_origin())
            .unwrap_or_else(|| entity.block_pos.load());
        let world = entity.world.load();
        let mut random = mob.get_random();
        for _ in 0..3 {
            let test_pos = bound_origin.add(
                random.random_range(0..15) - 7,
                random.random_range(0..11) - 5,
                random.random_range(0..15) - 7,
            );
            if world.get_block_state(&test_pos).is_air() {
                let mob_entity = mob.get_mob_entity();
                let center = test_pos.to_centered_f64();
                set_wanted_position(mob_entity, center, 0.25);
                if mob_entity.get_target().is_none() {
                    mob_entity
                        .look_control
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .look_at_with_range(center.x, center.y, center.z, 180.0, 20.0);
                }
                break;
            }
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

struct VexCopyOwnerTargetGoal {
    vex: Weak<VexEntity>,
    track_target_goal: TrackTargetGoal,
    copy_owner_targeting: TargetPredicate,
}

impl VexCopyOwnerTargetGoal {
    fn new(vex: Weak<VexEntity>) -> Self {
        Self {
            vex,
            track_target_goal: TrackTargetGoal::with_default(false),
            copy_owner_targeting: TargetPredicate::create_non_attackable()
                .ignore_visibility()
                .ignore_distance_scaling_factor(),
        }
    }

    fn owner_target(&self) -> Option<Arc<dyn EntityBase>> {
        self.vex
            .upgrade()?
            .get_owner()?
            .get_mob()?
            .get_mob_entity()
            .get_target()
    }
}

impl Goal for VexCopyOwnerTargetGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let target = self.owner_target();
        self.track_target_goal
            .can_track(mob, target.as_deref(), &self.copy_owner_targeting)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.track_target_goal.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.track_target_goal.start_targeting(
            mob,
            self.owner_target(),
            TargetReason::OwnerAttackedTarget,
        );
        self.track_target_goal.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.track_target_goal.stop(mob);
    }

    fn controls(&self) -> Controls {
        self.track_target_goal.controls()
    }
}
