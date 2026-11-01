use pumpkin_data::BlockState;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::control::MoveControlTrait;
use crate::entity::ai::control::move_control::{MoveControl, Operation};
use crate::entity::ai::goal::move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal};
use crate::entity::ai::goal::ranged_attack::RangedAttackGoal;
use crate::entity::ai::goal::wander_around::WanderAroundGoal;
use crate::entity::ai::goal::zombie_attack::ZombieAttackGoal;
use crate::entity::ai::goal::{Controls, Goal, ParentHandle};
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::ai::pathfinder::{Navigator, NavigatorGoal};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::ai::util::default_random_pos;
use crate::entity::custom_sound::CustomSound;
use crate::entity::living::LivingEntity;
use crate::entity::mob::spawn::{SpawnGroupData, SpawnReason, finalize_spawn};
use crate::entity::mob::zombie::ZombieEntityBase;
use crate::entity::mob::{RangedAttackMob, equipment::RegionalDifficulty};
use crate::entity::projectile::arrow::ArrowPickup;
use crate::entity::projectile::trident::TridentEntity;
use crate::entity::r#type::from_type;
use crate::entity::{
    Entity, EntityBase,
    mob::{Mob, MobEntity},
};
use crate::world::World;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_nbt::compound::NbtCompound;

const DROWNED_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true).matching(ok_target),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
    Hunt::new(3, TargetMatch::Type(&EntityType::AXOLOTL), true),
    hunting::baby_turtles(5, true),
];

/// Vanilla `Drowned.NAUTILUS_SHELL_CHANCE`.
const NAUTILUS_SHELL_CHANCE: f32 = 0.03;
/// Vanilla `Drowned.ZOMBIE_NAUTILUS_JOCKEY_CHANCE`.
const ZOMBIE_NAUTILUS_JOCKEY_CHANCE: f32 = 0.5;

/// Vanilla `okTarget`: players only at night or in water.
fn ok_target(_hunter: &dyn Mob, target: &LivingEntity, world: &World) -> bool {
    !world.is_bright_outside() || target.entity.is_in_water()
}

fn ok_target_of(mob: &dyn Mob) -> bool {
    let world = mob.get_entity().world.load();
    mob.get_mob_entity()
        .get_target()
        .is_some_and(|target| !world.is_bright_outside() || target.get_entity().is_in_water())
}

pub struct DrownedEntity {
    entity: Arc<ZombieEntityBase>,
    searching_for_land: AtomicBool,
}

impl DrownedEntity {
    #[must_use]
    pub fn base(&self) -> &ZombieEntityBase {
        &self.entity
    }

    pub fn new(entity: Entity) -> Arc<Self> {
        Self::with_can_break_doors(entity, false)
    }

    #[must_use]
    pub fn with_can_break_doors(entity: Entity, can_break_doors: bool) -> Arc<Self> {
        let base = ZombieEntityBase::with_targets(
            entity,
            can_break_doors,
            ZombieEntityBase::revenge_goal()
                .ignoring(|entity_type| entity_type == &EntityType::DROWNED),
            DROWNED_HUNTS,
            false,
        );
        // drowned: amphibious navigation, no water malus (Drowned constructor)
        {
            let mut navigator = base
                .mob_entity
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *navigator = Navigator::amphibious(false);
            navigator.set_pathfinding_malus(PathType::Water, 0.0);
        }
        let drowned = Arc::new(Self {
            entity: base,
            searching_for_land: AtomicBool::new(false),
        });
        *drowned
            .entity
            .mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(DrownedMoveControl::new(Arc::downgrade(&drowned)));

        let ranged: Weak<dyn RangedAttackMob> = {
            let ranged: Arc<dyn RangedAttackMob> = drowned.clone();
            Arc::downgrade(&ranged)
        };
        let weak = Arc::downgrade(&drowned);
        // drowned: behaviour goals (Drowned.addBehaviourGoals)
        let goals = &drowned.entity.mob_entity.goals_selector;
        goals.add_goal(1, Box::new(DrownedGoToWaterGoal::new(1.0)));
        goals.add_goal(
            2,
            Box::new(DrownedTridentAttackGoal {
                inner: RangedAttackGoal::new(ranged, 1.0, 40, 10.0),
            }),
        );
        goals.add_goal(
            2,
            Box::new(DrownedAttackGoal {
                inner: ZombieAttackGoal::new(1.0, false),
            }),
        );
        goals.add_goal(
            5,
            Box::new(DrownedGoToBeachGoal {
                // SAFETY: points at a static, which never moves or drops.
                inner: MoveToTargetPosGoal::new(
                    unsafe { ParentHandle::new(&BEACH_TARGET) },
                    1.0,
                    8,
                    2,
                ),
                drowned: weak.clone(),
            }),
        );
        goals.add_goal(
            6,
            Box::new(DrownedSwimUpGoal {
                drowned: weak,
                speed: 1.0,
                stuck: false,
            }),
        );
        goals.add_goal(7, Box::new(WanderAroundGoal::new(1.0)));
        drowned
    }

    fn is_searching_for_land(&self) -> bool {
        self.searching_for_land.load(Ordering::Relaxed)
    }

    fn set_searching_for_land(&self, searching: bool) {
        self.searching_for_land.store(searching, Ordering::Relaxed);
    }

    // drowned: swim while looking for land or chasing a target in water (Drowned.wantsToSwim)
    fn wants_to_swim(&self) -> bool {
        self.is_searching_for_land()
            || self
                .entity
                .mob_entity
                .get_target()
                .is_some_and(|target| target.get_entity().is_in_water())
    }

    // drowned: path target within 2 blocks (Drowned.closeToNextPos)
    fn close_to_next_pos(&self) -> bool {
        let mob_entity = &self.entity.mob_entity;
        let navigator = mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator.get_path().is_some_and(|path| {
            let target = path.get_target();
            mob_entity
                .living_entity
                .entity
                .pos
                .load()
                .squared_distance_to_vec(&Vector3::new(
                    f64::from(target.0.x),
                    f64::from(target.0.y),
                    f64::from(target.0.z),
                ))
                < 4.0
        })
    }

    fn holds_trident(&self) -> bool {
        self.entity
            .mob_entity
            .with_main_hand(|stack| stack.item.id == Item::TRIDENT.id)
    }

    // drowned: maybe ride a new zombie nautilus (Drowned.finalizeSpawn)
    fn roll_nautilus_jockey(&self, world: &Arc<World>, reason: SpawnReason) {
        let entity = self.get_entity();
        if !matches!(reason, SpawnReason::Natural | SpawnReason::Structure)
            || !self.holds_trident()
            || rand::rng().random::<f32>() >= ZOMBIE_NAUTILUS_JOCKEY_CHANCE
            || self.entity.is_baby()
            || world
                .get_biome(&entity.block_pos.load())
                .has_tag(&tag::WorldgenBiome::MINECRAFT_MORE_FREQUENT_DROWNED_SPAWNS)
        {
            return;
        }
        let nautilus = from_type(
            &EntityType::ZOMBIE_NAUTILUS,
            entity.pos.load(),
            world,
            uuid::Uuid::new_v4(),
        );
        // zombie nautilus has no AI here yet; skip instead of spawning a dummy
        let Some(nautilus_mob) = nautilus.get_mob() else {
            return;
        };
        if reason == SpawnReason::Structure {
            nautilus_mob
                .get_mob_entity()
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        nautilus.get_entity().set_rotation(entity.yaw.load(), 0.0);
        finalize_spawn(&nautilus, world, None, reason);
        self.entity.mob_entity.set_pending_vehicle(nautilus, true);
    }
}

impl Mob for DrownedEntity {
    fn killed_entity(&self, victim: &dyn EntityBase) -> bool {
        crate::entity::mob::zombie::infect_villager(self.get_entity(), victim)
    }

    // drowned: zombie rolls, nautilus shell, nautilus jockey (Drowned.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.entity.finalize_zombie_spawn(self, world, group_data);
        let mob_entity = &self.entity.mob_entity;
        let offhand_empty = mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::OFF_HAND)
            .is_empty();
        if offhand_empty && rand::rng().random::<f32>() < NAUTILUS_SHELL_CHANCE {
            mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::OFF_HAND,
                ItemStack::new(1, &Item::NAUTILUS_SHELL),
            );
        }
        self.roll_nautilus_jockey(world, mob_entity.spawn_reason());
        group_data
    }

    // drowned: 10% trident (10/16) or fishing rod, no armor (Drowned.populateDefaultEquipmentSlots)
    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        let mut random = rand::rng();
        if random.random::<f32>() > 0.9 {
            let item = if random.random_range(0..16) < 10 {
                &Item::TRIDENT
            } else {
                &Item::FISHING_ROD
            };
            self.entity
                .mob_entity
                .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, item));
        }
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        self.entity.on_zombie_attack(self, target);
    }

    fn on_damage_with_context(
        &self,
        _damage_type: DamageType,
        _source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.entity.on_zombie_hurt(cause);
        true
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.entity
            .experience_reward(self.get_entity().entity_type.experience_reward)
    }

    // drowned: water variants (Drowned.getAmbientSound)
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.get_entity().is_in_water() {
            Sound::EntityDrownedAmbientWater
        } else {
            Sound::EntityDrownedAmbient
        })
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityDrownedStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity.mob_entity
    }

    fn spawn_as_baby(&self) -> bool {
        self.entity.spawn_as_baby()
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.entity.mob_write_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.entity.mob_read_nbt(nbt);
    }

    // drowned: no spears, no glow ink sacs (Drowned/Zombie.wantsToPickUp)
    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        stack.item != &Item::GLOW_INK_SAC && !stack.item.has_tag(&tag::Item::MINECRAFT_SPEARS)
    }

    fn preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        Some(&tag::Item::MINECRAFT_DROWNED_PREFERRED_WEAPONS)
    }

    // drowned: never swaps out its nautilus shell (Drowned.canReplaceCurrentItem)
    fn keeps_equipped_item(&self, current: &ItemStack) -> bool {
        current.item.id == Item::NAUTILUS_SHELL.id
    }

    // drowned: not pushed by water while swimming (Drowned.isPushedByFluid)
    fn mob_is_pushed_by_fluids(&self) -> bool {
        !self.get_entity().is_swimming()
    }

    // drowned: swim pose when under water and wanting to swim (Drowned.updateSwimming)
    fn post_tick(&self) {
        let entity = self.get_entity();
        entity.set_swimming(
            !self.entity.mob_entity.is_no_ai() && entity.is_under_water() && self.wants_to_swim(),
        );
        // drowned: body follows the ridden mob (Drowned.rideTick)
        if let Some(vehicle) = entity.get_vehicle()
            && vehicle.get_mob().is_some()
        {
            entity.body_yaw.store(vehicle.get_entity().body_yaw.load());
        }
    }

    // drowned: slow swim under water (Drowned.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, gravity: f64, falling: bool) {
        let living = &self.entity.mob_entity.living_entity;
        if living.entity.is_under_water() && self.wants_to_swim() {
            living
                .entity
                .update_velocity_from_input(living.movement_input.load(), 0.01);
            living.make_move(caller);
            living
                .entity
                .velocity
                .store(living.entity.velocity.load() * 0.9);
        } else {
            living.travel_in_water(caller, gravity, falling);
        }
    }
}

impl CustomSound for DrownedEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.get_entity().is_in_water() {
            Sound::EntityDrownedDeathWater
        } else {
            Sound::EntityDrownedDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.get_entity().is_in_water() {
            Sound::EntityDrownedHurtWater
        } else {
            Sound::EntityDrownedHurt
        })
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl RangedAttackMob for DrownedEntity {
    // drowned: throw the held trident or a copy (Drowned.performRangedAttack)
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, _power: f32) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let trident_stack = self.entity.mob_entity.with_main_hand(|stack| {
            if stack.item.id == Item::TRIDENT.id {
                stack.clone()
            } else {
                ItemStack::new(1, &Item::TRIDENT)
            }
        });
        let trident_entity = Entity::new(world.clone(), entity.pos.load(), &EntityType::TRIDENT);
        let trident = TridentEntity::new_shot(
            trident_entity,
            entity,
            trident_stack.clone(),
            ArrowPickup::Disallowed,
        );
        trident.apply_on_projectile_spawned(&trident_stack);
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();
        let dx = target_pos.x - entity.pos.load().x;
        let dy = target_pos.y + f64::from(target_entity.height()) / 3.0
            - trident.get_entity().pos.load().y;
        let dz = target_pos.z - entity.pos.load().z;
        let distance = dx.hypot(dz);
        let difficulty = world.level_info.load().difficulty as i32;
        trident.set_velocity(
            dx,
            distance.mul_add(0.2, dy),
            dz,
            1.6,
            f64::from(14 - difficulty * 4),
        );
        world.spawn_entity(Arc::new(trident));
        let pitch = 1.0 / rand::rng().random::<f32>().mul_add(0.4, 0.8);
        self.play_entity_sound(Sound::EntityDrownedShoot, 1.0, pitch);
    }
}

// drowned: swim control in water, sink faster on land (Drowned.DrownedMoveControl)
struct DrownedMoveControl {
    drowned: Weak<DrownedEntity>,
    base: MoveControl,
}

impl DrownedMoveControl {
    fn new(drowned: Weak<DrownedEntity>) -> Self {
        Self {
            drowned,
            base: MoveControl::default(),
        }
    }
}

impl crate::entity::ai::control::Control for DrownedMoveControl {}

impl MoveControlTrait for DrownedMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let Some(drowned) = self.drowned.upgrade() else {
            return;
        };
        let mob_entity = mob.get_mob_entity();
        let living = &mob_entity.living_entity;
        let entity = &living.entity;
        if !(drowned.wants_to_swim() && entity.is_in_water()) {
            if !entity.on_ground.load(Ordering::Relaxed) {
                entity
                    .velocity
                    .store(entity.velocity.load().add_raw(0.0, -0.008, 0.0));
            }
            self.base.tick(mob);
            return;
        }
        let target_higher = mob_entity
            .get_target()
            .is_some_and(|target| target.get_entity().pos.load().y > entity.pos.load().y);
        if target_higher || drowned.is_searching_for_land() {
            entity
                .velocity
                .store(entity.velocity.load().add_raw(0.0, 0.002, 0.0));
        }
        if self.base.operation != Operation::MoveTo || mob.is_navigator_idle() {
            mob_entity.set_speed(0.0);
            return;
        }
        let pos = entity.pos.load();
        let xd = self.base.wanted_x - pos.x;
        let mut yd = self.base.wanted_y - pos.y;
        let zd = self.base.wanted_z - pos.z;
        let dd = (xd * xd + yd * yd + zd * zd).sqrt();
        yd /= dd;
        let y_rot_d = (zd.atan2(xd).to_degrees() as f32) - 90.0;
        let yaw = MoveControl::rotlerp(entity.yaw.load(), y_rot_d, 90.0);
        entity.yaw.store(yaw);
        entity.body_yaw.store(yaw);
        let target_speed = (self.base.speed_modifier
            * living.get_attribute_value(&pumpkin_data::attributes::Attributes::MOVEMENT_SPEED))
            as f32;
        let speed = (target_speed - living.get_speed()).mul_add(0.125, living.get_speed());
        mob_entity.set_speed(speed);
        let speed = f64::from(speed);
        entity.velocity.store(entity.velocity.load().add_raw(
            speed * xd * 0.005,
            speed * yd * 0.1,
            speed * zd * 0.005,
        ));
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, speed_modifier: f64) {
        self.base.set_wanted_position(x, y, z, speed_modifier);
    }

    fn strafe(&mut self, forward: f32, right: f32) {
        self.base.strafe(forward, right);
    }

    fn has_wanted(&self) -> bool {
        self.base.has_wanted()
    }
}

// drowned: by day on land, walk to nearby water (Drowned.DrownedGoToWaterGoal)
struct DrownedGoToWaterGoal {
    speed: f64,
    wanted: Vector3<f64>,
}

impl DrownedGoToWaterGoal {
    const fn new(speed: f64) -> Self {
        Self {
            speed,
            wanted: Vector3::new(0.0, 0.0, 0.0),
        }
    }
}

impl Goal for DrownedGoToWaterGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        if !world.is_bright_outside() || entity.is_in_water() {
            return false;
        }
        let origin = entity.block_pos.load();
        let mut random = mob.get_random();
        for _ in 0..10 {
            let candidate = BlockPos::new(
                origin.0.x + random.random_range(0..20) - 10,
                origin.0.y + 2 - random.random_range(0..8),
                origin.0.z + random.random_range(0..20) - 10,
            );
            if world.get_block(&candidate) == &pumpkin_data::Block::WATER {
                self.wanted = Vector3::new(
                    f64::from(candidate.0.x) + 0.5,
                    f64::from(candidate.0.y),
                    f64::from(candidate.0.z) + 0.5,
                );
                return true;
            }
        }
        false
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        !mob.is_navigator_idle()
    }

    fn start(&mut self, mob: &dyn Mob) {
        let pos = mob.get_entity().pos.load();
        mob.get_mob_entity()
            .steered()
            .navigator()
            .set_progress(NavigatorGoal::new(pos, self.wanted, self.speed));
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// drowned: trident throw while holding one (Drowned.DrownedTridentAttackGoal)
struct DrownedTridentAttackGoal {
    inner: RangedAttackGoal,
}

fn holds_trident(mob: &dyn Mob) -> bool {
    mob.get_mob_entity()
        .with_main_hand(|stack| stack.item.id == Item::TRIDENT.id)
}

impl Goal for DrownedTridentAttackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start(mob) && holds_trident(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
        let mob_entity = mob.get_mob_entity();
        mob_entity.set_attacking(true);
        let stack = mob_entity.with_main_hand(Clone::clone);
        mob_entity
            .living_entity
            .set_active_hand(Hand::Right, stack, 72_000);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
        let mob_entity = mob.get_mob_entity();
        mob_entity.living_entity.clear_active_hand();
        mob_entity.set_attacking(false);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// drowned: melee only on an ok target (Drowned.DrownedAttackGoal)
struct DrownedAttackGoal {
    inner: Box<ZombieAttackGoal>,
}

impl Goal for DrownedAttackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start(mob) && ok_target_of(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob) && ok_target_of(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// drowned: a beach block with two air blocks above (DrownedGoToBeachGoal.isValidTarget)
struct BeachTarget;

static BEACH_TARGET: BeachTarget = BeachTarget;

impl MoveToTargetPos for BeachTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        let above = block_pos.up();
        world.get_block_state(&above).is_air()
            && world.get_block_state(&above.up()).is_air()
            && world.get_block_state(&block_pos).is_solid()
    }
}

// drowned: at night in shallow water, swim to a beach (Drowned.DrownedGoToBeachGoal)
struct DrownedGoToBeachGoal {
    inner: MoveToTargetPosGoal<BeachTarget>,
    drowned: Weak<DrownedEntity>,
}

impl Goal for DrownedGoToBeachGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        self.inner.can_start(mob)
            && !world.is_bright_outside()
            && entity.is_in_water()
            && entity.pos.load().y >= f64::from(world.sea_level - 3)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(drowned) = self.drowned.upgrade() {
            drowned.set_searching_for_land(false);
        }
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// drowned: at night deep in water, swim up to the surface (Drowned.DrownedSwimUpGoal)
struct DrownedSwimUpGoal {
    drowned: Weak<DrownedEntity>,
    speed: f64,
    stuck: bool,
}

impl DrownedSwimUpGoal {
    fn applies(mob: &dyn Mob) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        !world.is_bright_outside()
            && entity.is_in_water()
            && entity.pos.load().y < f64::from(world.sea_level - 2)
    }
}

impl Goal for DrownedSwimUpGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        Self::applies(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        Self::applies(mob) && !self.stuck
    }

    fn start(&mut self, _mob: &dyn Mob) {
        if let Some(drowned) = self.drowned.upgrade() {
            drowned.set_searching_for_land(true);
        }
        self.stuck = false;
    }

    fn stop(&mut self, _mob: &dyn Mob) {
        if let Some(drowned) = self.drowned.upgrade() {
            drowned.set_searching_for_land(false);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(drowned) = self.drowned.upgrade() else {
            return;
        };
        let entity = mob.get_entity();
        let sea_level = entity.world.load().sea_level;
        let pos = entity.pos.load();
        if pos.y < f64::from(sea_level - 1)
            && (mob.is_navigator_idle() || drowned.close_to_next_pos())
        {
            let Some(next) = default_random_pos::get_pos_towards(
                mob,
                4,
                8,
                Vector3::new(pos.x, f64::from(sea_level - 1), pos.z),
                std::f64::consts::FRAC_PI_2,
            ) else {
                self.stuck = true;
                return;
            };
            mob.get_mob_entity()
                .steered()
                .navigator()
                .set_progress(NavigatorGoal::new(pos, next, self.speed));
        }
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}
