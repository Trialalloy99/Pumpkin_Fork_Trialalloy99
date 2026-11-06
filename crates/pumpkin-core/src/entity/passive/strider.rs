use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::fluid::Fluid;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::pathfinder::{Navigator, node::PathType};
use crate::entity::attributes::{Modifier, ModifierOperation};
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::{SpawnGroupData, SpawnReason};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        Controls, Goal, ParentHandle,
        breed::BreedGoal,
        escape_danger::EscapeDangerGoal,
        follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal,
        move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal},
        tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    item_steerable::{ItemBasedSteering, ItemSteerable},
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::WARPED_FUNGUS, &Item::WARPED_FUNGUS_ON_A_STICK];
// strider: SUFFOCATING_MODIFIER, StriderGoToLavaGoal range
const SUFFOCATING_MODIFIER_ID: &str = "minecraft:suffocating";
const SUFFOCATING_MODIFIER_AMOUNT: f64 = -0.340_000_003_576_278_7;
const GO_TO_LAVA_RANGE: i32 = 8;
const GO_TO_LAVA_Y_RANGE: i32 = 2;
const JOCKEY_PIGLIN_ODDS: i32 = 30;
const JOCKEY_STRIDER_ODDS: i32 = 10;
const BABY_JOCKEY_AGE: i32 = -24000;
const ZOMBIE_BABY_ODDS: f32 = 0.05;

// strider: Strider.StriderGoToLavaGoal target, a lava block with room above
struct LavaTarget;

static LAVA_TARGET: LavaTarget = LavaTarget;

impl MoveToTargetPos for LavaTarget {
    fn is_target_pos(&self, world: Arc<crate::world::World>, block_pos: BlockPos) -> bool {
        if world.get_block(&block_pos) != &pumpkin_data::Block::LAVA {
            return false;
        }
        let above = world.get_block_state(&block_pos.up());
        !above.is_liquid() && above.get_block_collision_shapes().next().is_none()
    }
}

// strider: Strider.StriderGoToLavaGoal
struct StriderGoToLavaGoal {
    inner: MoveToTargetPosGoal<LavaTarget>,
}

impl StriderGoToLavaGoal {
    fn new(speed: f64) -> Self {
        // SAFETY: LAVA_TARGET is a static.
        let mut inner = MoveToTargetPosGoal::new(
            unsafe { ParentHandle::new(&LAVA_TARGET) },
            speed,
            GO_TO_LAVA_RANGE,
            GO_TO_LAVA_Y_RANGE,
        );
        inner.lowest_y = 0;
        inner.reset_path_interval = 20;
        inner.targets_block_itself = true;
        Self { inner }
    }
}

impl Goal for StriderGoToLavaGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        !mob.get_entity().touching_lava.load(Ordering::Relaxed) && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let world = mob.get_entity().world.load_full();
        !mob.get_entity().touching_lava.load(Ordering::Relaxed)
            && LAVA_TARGET.is_target_pos(world, self.inner.target_pos)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::JUMP
    }
}

pub struct StriderEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub steering: ItemBasedSteering,
    pub suffocating: AtomicBool,
}

impl StriderEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);

        // strider: attributes (Strider.createAttributes)
        mob_entity.living_entity.set_max_health(20.0);
        mob_entity.living_entity.health.store(20.0);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.175);

        let strider = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            steering: ItemBasedSteering::default(),
            suffocating: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(strider);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // strider: Strider.registerGoals
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.65));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.4, TEMPT_ITEMS, false)));
            goal_selector.add_goal(4, Box::new(StriderGoToLavaGoal::new(1.0)));
            goal_selector.add_goal(5, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::with_interval(1.0, 60)));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(
                9,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::STRIDER, 8.0),
            );
        };

        {
            // strider: StriderPathNavigation and the constructor pathfinding malus
            let mut navigator = mob_arc
                .mob_entity
                .navigator
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *navigator = Navigator::strider();
            navigator.set_pathfinding_malus(PathType::Water, -1.0);
            navigator.set_pathfinding_malus(PathType::Lava, 0.0);
            navigator.set_pathfinding_malus(PathType::DangerFire, 0.0);
            navigator.set_pathfinding_malus(PathType::DamageFire, 0.0);
        };

        // strider: Strider.getLeashOffset
        mob_arc
            .mob_entity
            .living_entity
            .entity
            .leash_offset_factors
            .store(Some((0.6, 0.4)));

        mob_arc
    }

    // strider: TemptGoal.isRunning
    fn is_being_tempted(&self) -> bool {
        self.mob_entity.goals_selector.is_running::<TemptGoal>()
    }

    fn is_panicking_goal(&self) -> bool {
        self.mob_entity
            .goals_selector
            .is_running::<EscapeDangerGoal>()
    }

    // strider: isEyeInFluid(LAVA)
    fn is_eye_in_lava(&self) -> bool {
        let entity = self.get_entity();
        let eye = entity.get_eye_pos();
        let pos = BlockPos::floored_v(eye);
        let world = entity.world.load();
        let (fluid, state) = world.get_fluid_and_fluid_state(&pos);
        fluid.has_tag(&tag::Fluid::MINECRAFT_LAVA)
            && f64::from(pos.0.y) + f64::from(world.get_fluid_height(&pos, fluid, &state)) > eye.y
    }

    #[must_use]
    pub fn is_suffocating(&self) -> bool {
        self.suffocating.load(Ordering::Relaxed)
    }

    pub fn set_suffocating(&self, suffocating: bool) {
        self.suffocating.store(suffocating, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_SUFFOCATING,
            suffocating,
        );
        let living = &self.mob_entity.living_entity;
        living.update_attribute(&Attributes::MOVEMENT_SPEED, |speed| {
            if suffocating {
                speed.add_or_replace_modifier(Modifier {
                    id: SUFFOCATING_MODIFIER_ID.to_string(),
                    amount: SUFFOCATING_MODIFIER_AMOUNT,
                    operation: ModifierOperation::MultiplyBase,
                });
            } else {
                speed.remove_modifier(SUFFOCATING_MODIFIER_ID);
            }
        });
        crate::entity::attributes::send_attribute_updates_for_living(
            living,
            vec![Attributes::MOVEMENT_SPEED],
        );
    }
}

impl AgeableMob for StriderEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::STRIDER_BABY_SPAWN_CHANCE
    }
}

impl Animal for StriderEntity {
    fn eating_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderEat)
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_STRIDER_FOOD)
            || item_stack.item == &Item::WARPED_FUNGUS
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby_base = from_type(
            &EntityType::STRIDER,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        baby_base.get_entity().age.store(-24000, Ordering::Relaxed);
        baby_base
    }
}

impl CustomSound for StriderEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderHurt)
    }
}

impl Mob for StriderEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    // strider: Strider.getAmbientSound, quiet while panicking or tempted
    fn get_ambient_sound(&self) -> Option<Sound> {
        (!self.is_panicking_goal() && !self.is_being_tempted())
            .then_some(Sound::EntityStriderAmbient)
    }

    // strider: Strider.canStandOnFluid, lava
    fn mob_can_stand_on_fluid(&self, fluid: &Fluid) -> bool {
        fluid.has_tag(&tag::Fluid::MINECRAFT_LAVA)
    }

    // strider: Strider.getWalkTargetValue
    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let entity = self.get_entity();
        if entity
            .world
            .load()
            .get_fluid(pos)
            .has_tag(&tag::Fluid::MINECRAFT_LAVA)
        {
            10.0
        } else if entity.touching_lava.load(Ordering::Relaxed) {
            f32::NEG_INFINITY
        } else {
            0.0
        }
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        let sound = if self.get_entity().touching_lava.load(Ordering::Relaxed) {
            Sound::EntityStriderStepLava
        } else {
            Sound::EntityStriderStep
        };
        self.play_entity_sound(sound, 1.0, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    // strider: player holding warped fungus on a stick steers (Strider.getControllingPassenger)
    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        let first = self.get_entity().passengers.lock().ok()?.first().cloned()?;
        if let Some(player) = first.get_player() {
            let holds_stick = |stack: ItemStack| stack.item == &Item::WARPED_FUNGUS_ON_A_STICK;
            return (self.is_saddled()
                && (holds_stick(player.living_entity.held_item(player))
                    || holds_stick(player.living_entity.off_hand_item(player))))
            .then_some(first);
        }
        (!self.mob_entity.is_no_ai() && first.get_mob().is_some() && first.can_control_vehicle())
            .then_some(first)
    }

    // strider: tick boost and face rider (Strider.tickRidden)
    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        let rider = controller.get_entity();
        let yaw = rider.yaw.load();
        let entity = self.get_entity();
        entity.set_rotation(yaw, rider.pitch.load() * 0.5);
        entity.head_yaw.store(yaw);
        entity.body_yaw.store(yaw);
        self.steering.tick_boost();
    }

    // strider: Strider.tick before super.tick
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let mut rng = self.get_random();
        if self.is_being_tempted() && rng.random_range(0..140) == 0 {
            self.play_entity_sound(Sound::EntityStriderHappy, 1.0, 1.0);
        } else if self.is_panicking_goal() && rng.random_range(0..60) == 0 {
            self.play_entity_sound(Sound::EntityStriderRetreat, 1.0, 1.0);
        }

        if !self.mob_entity.is_no_ai() {
            let world = entity.world.load();
            let block_pos = entity.block_pos.load();
            let warm = |state: &BlockState| {
                state
                    .id
                    .to_block()
                    .has_tag(&tag::Block::MINECRAFT_STRIDER_WARM_BLOCKS)
            };
            let in_warm_blocks = warm(world.get_block_state(&block_pos))
                || warm(world.get_block_state(&BlockPos::floored_v(
                    entity.pos.load().add_raw(0.0, -0.2, 0.0),
                )))
                || entity.touching_lava.load(Ordering::Relaxed);
            let on_warm_strider = entity
                .vehicle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .and_then(|vehicle| {
                    vehicle
                        .cast_any()
                        .downcast_ref::<Self>()
                        .map(Self::is_suffocating)
                })
                .is_some_and(|suffocating| !suffocating);
            let suffocating = !in_warm_blocks && !on_warm_strider;
            if suffocating != self.is_suffocating() {
                self.set_suffocating(suffocating);
            }
        }

        // strider: LivingEntity.aiStep, isSensitiveToWater
        if entity.is_in_water_or_rain() {
            let _ = entity.damage(entity, 1.0, pumpkin_data::damage::DamageType::DROWN);
        }
    }

    // strider: Strider.tick after super.tick, floatStrider and checkFallDamage in lava
    fn post_tick(&self) {
        let entity = self.get_entity();
        if !entity.touching_lava.load(Ordering::Relaxed) {
            return;
        }
        self.mob_entity.living_entity.fall_distance.store(0.0);
        let world = entity.world.load();
        let block_pos = entity.block_pos.load();
        let above_lava = world
            .get_fluid(&block_pos.up())
            .has_tag(&tag::Fluid::MINECRAFT_LAVA);
        if entity.pos.load().y > f64::from(block_pos.0.y) + 0.5 - 1.0e-5 && !above_lava {
            entity.on_ground.store(true, Ordering::Relaxed);
        } else {
            let velocity = entity.velocity.load();
            entity.velocity.store(Vector3::new(
                velocity.x * 0.5,
                velocity.y * 0.5 + 0.05,
                velocity.z * 0.5,
            ));
        }
    }

    // strider: Strider.finalizeSpawn, zombified piglin or baby strider jockey
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if self.is_baby() {
            return self.finalize_spawn_super(group_data);
        }
        let group_data;
        let (spawn_monsters, peaceful) = {
            let info = world.level_info.load();
            (
                info.game_rules.spawn_monsters && info.game_rules.spawn_mobs,
                info.difficulty == pumpkin_util::Difficulty::Peaceful,
            )
        };
        let mut rng = rand::rng();
        let pos = self.get_entity().pos.load();
        let yaw = self.get_entity().yaw.load();
        if rng.random_range(0..JOCKEY_PIGLIN_ODDS) == 0 && spawn_monsters && !peaceful {
            let jockey = from_type(&EntityType::ZOMBIFIED_PIGLIN, pos, world, Uuid::new_v4());
            jockey.get_entity().set_rotation(yaw, 0.0);
            let baby = rng.random::<f32>() < ZOMBIE_BABY_ODDS;
            crate::entity::mob::spawn::finalize_spawn(
                &jockey,
                world,
                Some(SpawnGroupData::Zombie {
                    baby,
                    can_spawn_jockey: false,
                    tried_camel_husk: false,
                }),
                SpawnReason::Jockey,
            );
            if let Some(mob) = jockey.get_mob() {
                mob.get_mob_entity().set_item_slot(
                    &EquipmentSlot::MAIN_HAND,
                    ItemStack::new(1, &Item::WARPED_FUNGUS_ON_A_STICK),
                );
            }
            self.mob_entity.add_pending_rider(jockey);
            group_data = Some(SpawnGroupData::Ageable {
                group_size: 0,
                should_spawn_baby: true,
                baby_spawn_chance: 0.0,
            });
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::SADDLE,
                ItemStack::new(1, &Item::SADDLE),
            );
        } else if rng.random_range(0..JOCKEY_STRIDER_ODDS) == 0 {
            let jockey = from_type(&EntityType::STRIDER, pos, world, Uuid::new_v4());
            jockey.get_entity().set_rotation(yaw, 0.0);
            crate::entity::mob::spawn::finalize_spawn(&jockey, world, None, SpawnReason::Jockey);
            if let Some(ageable) = jockey.get_mob().and_then(|mob| mob.as_ageable()) {
                ageable.set_age(BABY_JOCKEY_AGE);
            }
            self.mob_entity.add_pending_rider(jockey);
            group_data = Some(SpawnGroupData::Ageable {
                group_size: 0,
                should_spawn_baby: true,
                baby_spawn_chance: 0.0,
            });
        } else {
            group_data = Some(SpawnGroupData::Ageable {
                group_size: 0,
                should_spawn_baby: true,
                baby_spawn_chance: 0.5,
            });
        }
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("Saddle", self.is_saddled());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(saddle) = nbt.get_bool("Saddle") {
            self.set_saddled(saddle);
        } else if let Some(saddle_byte) = nbt.get_byte("Saddle") {
            self.set_saddled(saddle_byte == 1);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn get_item_steerable(&self) -> Option<&dyn ItemSteerable> {
        Some(self)
    }

    // strider: saddle lives in the SADDLE equipment slot (Strider.isSaddled)
    fn is_saddled(&self) -> bool {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .equipment
            .get(&EquipmentSlot::SADDLE)
            .is_some_and(|stack| !stack.is_empty())
    }

    fn can_be_saddled(&self) -> bool {
        self.mob_entity.living_entity.is_alive() && !self.is_baby()
    }

    fn set_saddled(&self, saddled: bool) {
        if saddled {
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::SADDLE,
                ItemStack::new(1, &Item::SADDLE),
            );
        } else {
            self.mob_entity
                .set_item_slot(&EquipmentSlot::SADDLE, ItemStack::EMPTY.clone());
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::strider::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_SUFFOCATING,
            self.is_suffocating(),
        );
    }

    // strider: Strider.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let entity = self.get_entity();
        let has_food = self.is_food(item_stack);
        if !has_food
            && self.is_saddled()
            && !entity.has_passengers()
            && !player.get_entity().is_sneaking()
        {
            // strider: canAddPassenger, not while the eyes are in lava
            if self.is_eye_in_lava() {
                return false;
            }
            let world = player.world();
            if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
            }
            return true;
        }
        if self.animal_interact(player, item_stack, Sound::EntityStriderAmbient) {
            return true;
        }
        // strider: equip saddle (Equippable.equipOnInteract)
        if item_stack.item == &Item::SADDLE && self.can_be_saddled() && !self.is_saddled() {
            let saddle = item_stack.copy_with_count(1);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            self.mob_entity
                .set_item_slot_and_drop_when_killed(&EquipmentSlot::SADDLE, saddle);
            let world = entity.world.load();
            let pos = entity.pos.load();
            world.play_sound(Sound::EntityStriderSaddle, SoundCategory::Neutral, &pos);
            world.emit_game_event(GameEvent::Equip.name(), pos);
            return true;
        }
        false
    }
}

impl ItemSteerable for StriderEntity {
    // strider: sync boost time (ItemBasedSteering.boost)
    fn boost(&self) -> bool {
        if !self.steering.boost() {
            return false;
        }
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_BOOST_TIME,
            VarInt(self.steering.boost_time_total.load(Ordering::Relaxed)),
        );
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
