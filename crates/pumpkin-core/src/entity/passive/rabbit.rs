use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::Block;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::block_properties::WheatLikeProperties;
use pumpkin_data::entity::EntityStatus;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::world::WorldEvent;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use pumpkin_world::world::BlockFlags;

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::sound::SoundCategory;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use rand::RngExt;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::goal::revenge::RevengeGoal;
use crate::world::World;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::behavior::fleeing::{self, Flee},
    ai::goal::{
        Controls, Goal, ParentHandle, breed::BreedGoal,
        climb_on_top_of_powder_snow::ClimbOnTopOfPowderSnowGoal,
        escape_danger::EscapeDangerGoal,
        look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal,
        move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal},
        swim::SwimGoal,
        tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    ai::control::{
        Control, MoveControlTrait,
        move_control::{MoveControl, Operation},
    },
    attributes::{Modifier, ModifierOperation},
    mob::spawn::SpawnGroupData,
    ai::target_match::TargetMatch,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};

// rabbit: killer bunny attack bonus (Rabbit.EVIL_ATTACK_POWER_MODIFIER)
const EVIL_ATTACK_POWER_MODIFIER: &str = "minecraft:evil";
const JUMP_DURATION: i32 = 15;

const TEMPT_ITEMS: &[&Item] = &[&Item::CARROT, &Item::GOLDEN_CARROT, &Item::DANDELION];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum RabbitVariant {
    #[default]
    Brown = 0,
    White = 1,
    Black = 2,
    WhiteSplotched = 3,
    Gold = 4,
    Salt = 5,
    Evil = 99,
}

impl RabbitVariant {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::White,
            2 => Self::Black,
            3 => Self::WhiteSplotched,
            4 => Self::Gold,
            5 => Self::Salt,
            99 => Self::Evil,
            _ => Self::Brown,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    // rabbit: biome variant (Rabbit.getRandomRabbitVariant)
    #[must_use]
    pub fn random_for_biome(biome: &pumpkin_data::chunk::Biome) -> Self {
        let random_val = rand::rng().random_range(0..100);
        if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_WHITE_RABBITS) {
            if random_val < 80 {
                Self::White
            } else {
                Self::WhiteSplotched
            }
        } else if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_GOLD_RABBITS) {
            Self::Gold
        } else if random_val < 50 {
            Self::Brown
        } else if random_val < 90 {
            Self::Salt
        } else {
            Self::Black
        }
    }
}

const RABBIT_FLEES: &[Flee] = &[
    Flee::new(4, TargetMatch::Type(&EntityType::PLAYER), 8.0, 2.2, 2.2).when(is_not_evil),
    Flee::new(4, TargetMatch::Type(&EntityType::WOLF), 10.0, 2.2, 2.2).when(is_not_evil),
    Flee::new(4, TargetMatch::Monster, 4.0, 2.2, 2.2).when(is_not_evil),
];

fn is_not_evil(mob: &dyn Mob) -> bool {
    mob.cast_any()
        .downcast_ref::<RabbitEntity>()
        .is_some_and(|rabbit| rabbit.get_variant() != RabbitVariant::Evil)
}

const KILLER_BUNNY_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true),
    Hunt::new(2, TargetMatch::Type(&EntityType::WOLF), true),
];

pub struct RabbitEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub more_carrot_ticks: AtomicI32,
    jump_ticks: AtomicI32,
    jump_duration: AtomicI32,
    was_on_ground: AtomicBool,
    jump_delay_ticks: AtomicI32,
    can_jump: AtomicBool,
    move_speed: AtomicCell<f64>,
    move_wanted: AtomicCell<Vector3<f64>>,
    wants_to_raid: AtomicBool,
    can_raid: AtomicBool,
}

impl RabbitEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let rabbit = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(RabbitVariant::Brown.id()),
            more_carrot_ticks: AtomicI32::new(0),
            jump_ticks: AtomicI32::new(0),
            jump_duration: AtomicI32::new(0),
            was_on_ground: AtomicBool::new(false),
            jump_delay_ticks: AtomicI32::new(0),
            can_jump: AtomicBool::new(false),
            move_speed: AtomicCell::new(0.0),
            move_wanted: AtomicCell::new(Vector3::default()),
            wants_to_raid: AtomicBool::new(false),
            can_raid: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(rabbit);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // rabbit: goals (Rabbit.registerGoals)
            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(ClimbOnTopOfPowderSnowGoal));
            goal_selector.add_goal(1, EscapeDangerGoal::new(2.2));
            goal_selector.add_goal(2, BreedGoal::new(0.8));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.0, TEMPT_ITEMS, false)));
            fleeing::apply(goal_selector, RABBIT_FLEES);
            goal_selector.add_goal(
                5,
                Box::new(RaidGardenGoal::new(Arc::downgrade(&mob_arc))),
            );
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::water_avoiding(0.6)));
            goal_selector.add_goal(
                11,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 10.0),
            );
        };

        *mob_arc
            .mob_entity
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(RabbitMoveControl::new(Arc::downgrade(&mob_arc)));
        mob_arc.set_speed_modifier(0.0);

        mob_arc
    }

    // rabbit: nav + move speed (Rabbit.setSpeedModifier)
    pub fn set_speed_modifier(&self, speed: f64) {
        self.mob_entity.navigator.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_speed(speed);
        let wanted = self.move_wanted.load();
        self.mob_entity.move_control.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_wanted_position(wanted.x, wanted.y, wanted.z, speed);
    }

    // rabbit: jump flag + jump sound (Rabbit.setJumping)
    fn set_jumping(&self, jump: bool) {
        self.mob_entity.living_entity.jumping.store(jump, Ordering::SeqCst);
        if jump {
            let mut random = self.get_random();
            let pitch = (random.random::<f32>() - random.random::<f32>()).mul_add(0.2, 1.0) * 0.8;
            self.play_entity_sound(Sound::EntityRabbitJump, 1.0, pitch);
        }
    }

    // rabbit: hop (Rabbit.startJumping)
    fn start_jumping(&self) {
        self.set_jumping(true);
        self.jump_duration.store(JUMP_DURATION, Ordering::Relaxed);
        self.jump_ticks.store(0, Ordering::Relaxed);
    }

    // rabbit: face xz (Rabbit.facePoint)
    fn face_point(&self, x: f64, z: f64) {
        let entity = self.get_entity();
        let pos = entity.pos.load();
        entity
            .yaw
            .store((z - pos.z).atan2(x - pos.x).to_degrees() as f32 - 90.0);
    }

    // rabbit: landing delay (Rabbit.checkLandingDelay)
    fn check_landing_delay(&self) {
        let delay = if self.move_speed.load() < 2.2 { 10 } else { 3 };
        self.jump_delay_ticks.store(delay, Ordering::Relaxed);
        self.can_jump.store(false, Ordering::Relaxed);
    }

    #[must_use]
    pub fn get_variant(&self) -> RabbitVariant {
        RabbitVariant::from_id(self.variant.load(Ordering::Relaxed))
    }

    pub fn set_variant(&self, variant: RabbitVariant) {
        self.variant.store(variant.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::rabbit::DATA_TYPE_ID,
            VarInt(variant.id()),
        );

        // rabbit: killer bunny armor, attack, goals, name (Rabbit.setVariant)
        let living = &self.mob_entity.living_entity;
        if variant == RabbitVariant::Evil {
            living.set_attribute_base(&Attributes::ARMOR, 8.0);
            self.add_killer_bunny_goals();
            living.update_attribute(&Attributes::ATTACK_DAMAGE, |inst| {
                inst.add_or_replace_modifier(Modifier {
                    id: EVIL_ATTACK_POWER_MODIFIER.to_string(),
                    amount: 5.0,
                    operation: ModifierOperation::Add,
                });
            });
            if entity.custom_name.load().is_none() {
                entity.set_custom_name(TextComponent::translate(
                    "entity.minecraft.killer_bunny",
                    [],
                ));
            }
        } else {
            living.update_attribute(&Attributes::ATTACK_DAMAGE, |inst| {
                inst.remove_modifier(EVIL_ATTACK_POWER_MODIFIER);
            });
        }
    }

    fn add_killer_bunny_goals(&self) {
        let goal_selector = &self.mob_entity.goals_selector;
        goal_selector.add_goal(4, Box::new(MeleeAttackGoal::new(1.4, true)));

        let target_selector = &self.mob_entity.target_selector;
        target_selector.add_goal(1, Box::new(RevengeGoal::new(true).alerting_others()));
        hunting::apply(target_selector, &self.mob_entity, KILLER_BUNNY_HUNTS);
    }
}

impl AgeableMob for RabbitEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for RabbitEntity {
    // rabbit: parent variant, 1/20 biome roll (Rabbit.getBreedOffspring)
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let baby = Self::new(Entity::new(
            world.clone(),
            entity.pos.load(),
            &EntityType::RABBIT,
        ));
        let biome = world.get_biome(&entity.block_pos.load());
        let mut variant = RabbitVariant::random_for_biome(biome);
        let mut random = self.get_random();
        if random.random_range(0..20) != 0 {
            variant = match partner.cast_any().downcast_ref::<Self>() {
                Some(partner) if random.random::<bool>() => partner.get_variant(),
                _ => self.get_variant(),
            };
        }
        baby.set_variant(variant);
        baby
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_RABBIT_FOOD)
            || TEMPT_ITEMS.iter().any(|i| i.id == item_stack.item.id)
    }
}

impl Mob for RabbitEntity {
    // rabbit: own baby hitbox (Rabbit.BABY_DIMENSIONS)
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.24,
            height: 0.4,
            eye_height: 0.39,
        })
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityRabbitAmbient)
    }

    // rabbit: killer bunny attack sound (Rabbit.playAttackSound)
    fn on_attack(&self, _target: &dyn EntityBase) {
        if self.get_variant() == RabbitVariant::Evil {
            let mut random = self.get_random();
            let pitch = (random.random::<f32>() - random.random::<f32>()).mul_add(0.2, 1.0);
            self.play_entity_sound(Sound::EntityRabbitAttack, 1.0, pitch);
        }
    }

    // rabbit: group variant, babies after the first (Rabbit.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let (variant, group_size) = match group_data {
            Some(SpawnGroupData::Rabbit {
                variant,
                group_size,
            }) => (RabbitVariant::from_id(variant), group_size),
            _ => {
                let biome = world.get_biome(&self.get_entity().block_pos.load());
                (RabbitVariant::random_for_biome(biome), 0)
            }
        };
        self.set_variant(variant);
        let group_size = match self.finalize_spawn_super(Some(SpawnGroupData::Ageable {
            group_size,
            should_spawn_baby: true,
            baby_spawn_chance: 1.0,
        })) {
            Some(SpawnGroupData::Ageable { group_size, .. }) => group_size,
            _ => group_size + 1,
        };
        Some(SpawnGroupData::Rabbit {
            variant: variant.id(),
            group_size,
        })
    }

    fn jump_control_tick(&self, jump: bool) -> bool {
        if jump {
            self.start_jumping();
        }
        true
    }

    // rabbit: hop height (Rabbit.getJumpPower)
    fn jump_power_multiplier(&self) -> f64 {
        let entity = self.get_entity();
        let y = entity.pos.load().y;
        let mut power: f32 = if self.move_speed.load() <= 0.6 { 0.2 } else { 0.3 };
        {
            let navigator = self.mob_entity.navigator.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(path) = navigator.get_path()
                && !path.is_done()
                && path
                    .get_next_entity_pos(entity.width())
                    .is_some_and(|next| next.y > y + 0.5)
            {
                power = 0.5;
            }
        }
        if entity.horizontal_collision.load(Ordering::Relaxed)
            || self.mob_entity.living_entity.jumping.load(Ordering::SeqCst)
                && self.move_wanted.load().y > y + 0.5
        {
            power = 0.5;
        }
        f64::from(power / 0.42)
    }

    // rabbit: forward push + hop event (Rabbit.jumpFromGround)
    fn on_jump_from_ground(&self) {
        let entity = self.get_entity();
        if self.move_speed.load() > 0.0 {
            let velocity = entity.velocity.load();
            if velocity.x * velocity.x + velocity.z * velocity.z < 0.01 {
                let up = if self.is_baby() { 0.5 } else { 1.5 };
                entity.update_velocity_from_input(Vector3::new(0.0, up, 1.0), 0.1);
            }
        }
        entity
            .world
            .load()
            .send_entity_status(entity, EntityStatus::Jump, ActorEventID::Jump);
    }

    // rabbit: hop timer (Rabbit.aiStep)
    fn mob_ai_step(&self) {
        let ticks = self.jump_ticks.load(Ordering::Relaxed);
        let duration = self.jump_duration.load(Ordering::Relaxed);
        if ticks != duration {
            self.jump_ticks.store(ticks + 1, Ordering::Relaxed);
        } else if duration != 0 {
            self.jump_ticks.store(0, Ordering::Relaxed);
            self.jump_duration.store(0, Ordering::Relaxed);
            self.set_jumping(false);
        }
    }

    // rabbit: hop toward wanted pos (Rabbit.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let delay = self.jump_delay_ticks.load(Ordering::Relaxed);
        if delay > 0 {
            self.jump_delay_ticks.store(delay - 1, Ordering::Relaxed);
        }
        let carrot = self.more_carrot_ticks.load(Ordering::Relaxed);
        if carrot > 0 {
            let next = carrot - self.get_random().random_range(0..3);
            self.more_carrot_ticks.store(next.max(0), Ordering::Relaxed);
        }
        let entity = self.get_entity();
        let on_ground = entity.on_ground.load(Ordering::Relaxed);
        if on_ground {
            if !self.was_on_ground.load(Ordering::Relaxed) {
                self.set_jumping(false);
                self.check_landing_delay();
            }
            if self.get_variant() == RabbitVariant::Evil
                && self.jump_delay_ticks.load(Ordering::Relaxed) == 0
                && let Some(target) = self.mob_entity.get_target()
            {
                let target_pos = target.get_entity().pos.load();
                if entity.pos.load().squared_distance_to_vec(&target_pos) < 16.0 {
                    self.face_point(target_pos.x, target_pos.z);
                    let speed = self.move_speed.load();
                    self.mob_entity.move_control.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_wanted_position(
                        target_pos.x,
                        target_pos.y,
                        target_pos.z,
                        speed,
                    );
                    self.start_jumping();
                    self.was_on_ground.store(true, Ordering::Relaxed);
                }
            }
            let wants_jump = self
                .mob_entity
                .jump_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .wants_jump();
            if !wants_jump {
                let has_wanted = self.mob_entity.move_control.lock().unwrap_or_else(std::sync::PoisonError::into_inner).has_wanted();
                if has_wanted && self.jump_delay_ticks.load(Ordering::Relaxed) == 0 {
                    let mut pos = self.move_wanted.load();
                    {
                        let navigator = self.mob_entity.navigator.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(path) = navigator.get_path()
                            && !path.is_done()
                            && let Some(next) = path.get_next_entity_pos(entity.width())
                        {
                            pos = next;
                        }
                    }
                    self.face_point(pos.x, pos.z);
                    self.start_jumping();
                }
            } else if !self.can_jump.load(Ordering::Relaxed) {
                self.can_jump.store(true, Ordering::Relaxed);
            }
        }
        self.was_on_ground.store(on_ground, Ordering::Relaxed);
    }

    fn mob_sound_source(&self) -> SoundCategory {
        if self.get_variant() == RabbitVariant::Evil {
            SoundCategory::Hostile
        } else {
            SoundCategory::Neutral
        }
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_int("RabbitType", self.get_variant().id());
        nbt.put_int(
            "MoreCarrotTicks",
            self.more_carrot_ticks.load(Ordering::Relaxed),
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(rabbit_type) = nbt.get_int("RabbitType") {
            self.set_variant(RabbitVariant::from_id(rabbit_type));
        }
        if let Some(ticks) = nbt.get_int("MoreCarrotTicks") {
            self.more_carrot_ticks.store(ticks, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::rabbit::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::rabbit::DATA_TYPE_ID,
            VarInt(self.get_variant().id()),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityRabbitAmbient)
    }
}

// rabbit: speed follows hops (Rabbit.RabbitMoveControl)
struct RabbitMoveControl {
    inner: MoveControl,
    rabbit: Weak<RabbitEntity>,
    next_jump_speed: f64,
}

impl RabbitMoveControl {
    fn new(rabbit: Weak<RabbitEntity>) -> Self {
        Self {
            inner: MoveControl::default(),
            rabbit,
            next_jump_speed: 0.0,
        }
    }

    fn sync(&self) {
        if let Some(rabbit) = self.rabbit.upgrade() {
            rabbit.move_speed.store(self.inner.speed_modifier);
            rabbit.move_wanted.store(Vector3::new(
                self.inner.wanted_x,
                self.inner.wanted_y,
                self.inner.wanted_z,
            ));
        }
    }

    // rabbit: setSpeedModifier without relocking move control
    fn apply_speed(&mut self, mob: &dyn Mob, speed: f64) {
        mob.get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_speed(speed);
        let (x, y, z) = (self.inner.wanted_x, self.inner.wanted_y, self.inner.wanted_z);
        self.set_wanted_position(x, y, z, speed);
    }
}

impl Control for RabbitMoveControl {}

impl MoveControlTrait for RabbitMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        let living = &mob_entity.living_entity;
        let wants_jump = mob_entity
            .jump_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wants_jump();
        if living.entity.on_ground.load(Ordering::Relaxed)
            && !living.jumping.load(Ordering::SeqCst)
            && !wants_jump
        {
            self.apply_speed(mob, 0.0);
        } else if self.inner.has_wanted() || self.inner.operation == Operation::Jumping {
            self.apply_speed(mob, self.next_jump_speed);
        }
        MoveControlTrait::tick(&mut self.inner, mob);
        self.sync();
    }

    fn set_wanted_position(&mut self, x: f64, y: f64, z: f64, mut speed_modifier: f64) {
        if self
            .rabbit
            .upgrade()
            .is_some_and(|rabbit| rabbit.get_entity().touching_water.load(Ordering::Relaxed))
        {
            speed_modifier = 1.5;
        }
        self.inner.set_wanted_position(x, y, z, speed_modifier);
        if speed_modifier > 0.0 {
            self.next_jump_speed = speed_modifier;
        }
        self.sync();
    }

    fn strafe(&mut self, forward: f32, right: f32) {
        self.inner.strafe(forward, right);
    }

    fn has_wanted(&self) -> bool {
        self.inner.has_wanted()
    }
}

// rabbit: carrot field target (Rabbit.RaidGardenGoal.isValidTarget)
struct RaidTarget;

static RAID_TARGET: RaidTarget = RaidTarget;

impl MoveToTargetPos for RaidTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        if !world
            .get_block(&block_pos)
            .has_tag(&tag::Block::MINECRAFT_SUPPORTS_CROPS)
        {
            return false;
        }
        let state = world.get_block_state_id(&block_pos.up());
        Block::from_state_id(state) == &Block::CARROTS
            && WheatLikeProperties::from_state_id(state).age == 7
    }
}

// rabbit: eat carrots off farmland (Rabbit.RaidGardenGoal)
struct RaidGardenGoal {
    inner: MoveToTargetPosGoal<RaidTarget>,
    rabbit: Weak<RabbitEntity>,
}

impl RaidGardenGoal {
    fn new(rabbit: Weak<RabbitEntity>) -> Self {
        Self {
            // SAFETY: RAID_TARGET is a static.
            inner: MoveToTargetPosGoal::with_default(
                unsafe { ParentHandle::new(&RAID_TARGET) },
                0.7,
                16,
            ),
            rabbit,
        }
    }
}

impl Goal for RaidGardenGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(rabbit) = self.rabbit.upgrade() else {
            return false;
        };
        if self.inner.cooldown <= 0 {
            if !mob.get_entity().world.load().level_info.load().game_rules.mob_griefing {
                return false;
            }
            rabbit.can_raid.store(false, Ordering::Relaxed);
            rabbit.wants_to_raid.store(
                rabbit.more_carrot_ticks.load(Ordering::Relaxed) <= 0,
                Ordering::Relaxed,
            );
        }
        if rabbit.wants_to_raid.load(Ordering::Relaxed) && !rabbit.can_raid.load(Ordering::Relaxed)
        {
            let found = self.inner.can_start(mob);
            if found {
                rabbit.can_raid.store(true, Ordering::Relaxed);
            }
            return found;
        }
        // rabbit: no valid target, still advance the search timer (MoveToBlockGoal.canUse)
        if self.inner.cooldown > 0 {
            self.inner.cooldown -= 1;
        } else {
            self.inner.cooldown = MoveToTargetPosGoal::<RaidTarget>::get_interval(mob);
        }
        false
    }

    // rabbit: isValidTarget fails once canRaid is set (Rabbit.RaidGardenGoal.canContinueToUse)
    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
        let Some(rabbit) = self.rabbit.upgrade() else {
            return;
        };
        let farmland = self.inner.target_pos;
        mob.get_mob_entity()
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_with_range(
                f64::from(farmland.0.x) + 0.5,
                f64::from(farmland.0.y) + 1.0,
                f64::from(farmland.0.z) + 0.5,
                10.0,
                mob.get_max_look_pitch_change(),
            );
        if !self.inner.is_reached_target() {
            return;
        }
        let world = mob.get_entity().world.load();
        let crops_pos = farmland.up();
        let state = world.get_block_state_id(&crops_pos);
        if rabbit.can_raid.load(Ordering::Relaxed) && Block::from_state_id(state) == &Block::CARROTS
        {
            let mut props = WheatLikeProperties::from_state_id(state);
            if props.age == 0 {
                world.set_block_state(&crops_pos, Block::AIR.default_state.id, BlockFlags::NOTIFY_LISTENERS);
                world.break_block(&crops_pos, None, BlockFlags::empty());
            } else {
                props.age -= 1;
                world.set_block_state(
                    &crops_pos,
                    props.to_state_id(&Block::CARROTS),
                    BlockFlags::NOTIFY_LISTENERS,
                );
                world.emit_game_event(GameEvent::BlockChange.name(), crops_pos.to_centered_f64());
                world.sync_world_event(
                    WorldEvent::ParticlesDestroyBlock,
                    crops_pos,
                    i32::from(state.as_u16()),
                );
            }
            rabbit.more_carrot_ticks.store(40, Ordering::Relaxed);
        }
        rabbit.can_raid.store(false, Ordering::Relaxed);
        self.inner.cooldown = 10;
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}
