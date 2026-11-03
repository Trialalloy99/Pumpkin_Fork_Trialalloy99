use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::Block;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::entity_status::EntityStatus;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        run_around_like_crazy::RunAroundLikeCrazyGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::{
        abstract_horse::{
            FLAG_BRED, FLAG_EATING, FLAG_OPEN_MOUTH, FLAG_STANDING, FLAG_TAME, MAX_HEALTH,
            MAX_JUMP_STRENGTH, MAX_MOVEMENT_SPEED, MIN_HEALTH, MIN_JUMP_STRENGTH,
            MIN_MOVEMENT_SPEED, controlling_passenger_horse, create_offspring_attribute,
            generate_max_health, play_horse_step_sound, tick_ridden_horse,
        },
        animal::Animal,
    },
    player::{Player, advancement::trigger::AdvancementTrigger},
    r#type::from_type,
};
use crate::world::World;

// donkey: HORSE_TEMPT_ITEMS (AbstractHorse.addBehaviourGoals)
const TEMPT_ITEMS: &[&Item] = &[
    &Item::GOLDEN_APPLE,
    &Item::ENCHANTED_GOLDEN_APPLE,
    &Item::GOLDEN_CARROT,
];

// donkey: every AbstractHorse, the partner class of the breed goal
const ABSTRACT_HORSES: &[&EntityType] = &[
    &EntityType::HORSE,
    &EntityType::DONKEY,
    &EntityType::MULE,
    &EntityType::SKELETON_HORSE,
    &EntityType::ZOMBIE_HORSE,
    &EntityType::CAMEL,
    &EntityType::CAMEL_HUSK,
    &EntityType::LLAMA,
    &EntityType::TRADER_LLAMA,
];

// donkey: constants (AbstractHorse, AbstractChestedHorse)
const CHEST_COLUMNS: usize = 5;
const CHEST_SLOTS: usize = 15;
const MAX_TEMPER: i32 = 100;
const STAND_TICKS: i32 = 20;
const EATING_GRASS_TICKS: i32 = 50;
const MOUTH_OPEN_TICKS: i32 = 30;
const AMBIENT_SOUND_INTERVAL: i32 = 400;
const SOUND_VOLUME: f32 = 0.8;
const MOMMY_RANGE: f64 = 16.0;
const JUMP_SOUND_VOLUME: f32 = 0.4;
const LAND_SOUND_VOLUME: f32 = 0.4;

pub struct DonkeyEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub flags: AtomicU8,
    pub has_chest: AtomicBool,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
    pub chest_inventory: Arc<SimpleInventory>,
    eating_counter: AtomicI32,
    mouth_counter: AtomicI32,
    stand_counter: AtomicI32,
    allow_stand_sliding: AtomicBool,
    player_jump_pending_scale: AtomicCell<f32>,
}

impl DonkeyEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);

        let donkey = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            flags: AtomicU8::new(0),
            has_chest: AtomicBool::new(false),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
            armor_inventory: Arc::new(SimpleInventory::new(1)),
            chest_inventory: Arc::new(SimpleInventory::new(CHEST_SLOTS)),
            eating_counter: AtomicI32::new(0),
            mouth_counter: AtomicI32::new(0),
            stand_counter: AtomicI32::new(0),
            allow_stand_sliding: AtomicBool::new(false),
            player_jump_pending_scale: AtomicCell::new(0.0),
        };

        let mob_arc = Arc::new(donkey);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            // donkey: AbstractHorse.registerGoals and addBehaviourGoals
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            // donkey: MountPanicGoal, a plain panic as nothing mob controlled rides a donkey
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(1, Box::new(RunAroundLikeCrazyGoal::new(1.2)));
            goal_selector.add_goal(2, BreedGoal::with_partner_types(1.0, ABSTRACT_HORSES));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::water_avoiding(0.7)));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(9, Box::new(RandomStandGoal::new()));
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
            pumpkin_data::tracked_data::donkey::DATA_ID_FLAGS,
            new_flags as i8,
        );
    }

    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.has_flag(FLAG_TAME)
    }

    pub fn set_tame(&self, val: bool) {
        self.set_flag(FLAG_TAME, val);
    }

    #[must_use]
    pub fn is_bred(&self) -> bool {
        self.has_flag(FLAG_BRED)
    }

    #[must_use]
    pub fn is_eating(&self) -> bool {
        self.has_flag(FLAG_EATING)
    }

    pub fn set_eating(&self, val: bool) {
        self.set_flag(FLAG_EATING, val);
    }

    #[must_use]
    pub fn is_standing(&self) -> bool {
        self.has_flag(FLAG_STANDING)
    }

    // donkey: saddle lives in SADDLE slot (AbstractHorse.isSaddled)
    #[must_use]
    pub fn is_saddled(&self) -> bool {
        let equipment = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        equipment
            .equipment
            .get(&EquipmentSlot::SADDLE)
            .is_some_and(|stack| !stack.is_empty())
    }

    pub fn set_saddled(&self, val: bool) {
        if val {
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::SADDLE,
                ItemStack::new(1, &Item::SADDLE),
            );
            self.saddle_inventory
                .set_stack(0, ItemStack::new(1, &Item::SADDLE));
        } else {
            self.mob_entity
                .set_item_slot(&EquipmentSlot::SADDLE, ItemStack::EMPTY.clone());
            self.saddle_inventory.set_stack(0, ItemStack::EMPTY.clone());
        }
    }

    #[must_use]
    pub fn has_chest(&self) -> bool {
        self.has_chest.load(Ordering::Relaxed)
    }

    pub fn set_has_chest(&self, val: bool) {
        self.has_chest.store(val, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::donkey::DATA_ID_CHEST, val);
    }

    // donkey: AbstractHorse.canParent
    #[must_use]
    pub fn can_parent(&self) -> bool {
        let entity = self.get_entity();
        let living = &self.mob_entity.living_entity;
        !entity.has_passengers()
            && !entity.has_vehicle()
            && self.is_tame()
            && !self.is_baby()
            && living.health.load() >= living.get_max_health()
            && self.mob_entity.is_in_love()
    }

    // donkey: AbstractHorse.canUseSlot(SADDLE)
    fn can_use_saddle_slot(&self) -> bool {
        self.mob_entity.living_entity.is_alive() && !self.is_baby() && self.is_tame()
    }

    // donkey: AbstractChestedHorse.randomizeAttributes
    fn randomize_attributes(&self) {
        let max_health = generate_max_health(&mut self.get_random());
        self.mob_entity.living_entity.set_max_health(max_health);
        self.mob_entity.living_entity.health.store(max_health);
    }

    // donkey: AbstractHorse.openMouth
    fn open_mouth(&self) {
        self.mouth_counter.store(1, Ordering::Relaxed);
        self.set_flag(FLAG_OPEN_MOUTH, true);
    }

    // donkey: AbstractHorse.eating
    fn eating(&self) {
        self.open_mouth();
        if !self.get_entity().is_silent() {
            let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
                .mul_add(0.2, 1.0);
            self.play_entity_sound(Sound::EntityDonkeyEat, 1.0, pitch);
        }
    }

    // donkey: AbstractHorse.setStanding
    fn set_standing(&self, ticks: i32) {
        self.set_eating(false);
        self.set_flag(FLAG_STANDING, true);
        self.stand_counter.store(ticks, Ordering::Relaxed);
    }

    // donkey: AbstractHorse.clearStanding
    fn clear_standing(&self) {
        self.set_flag(FLAG_STANDING, false);
        self.stand_counter.store(0, Ordering::Relaxed);
    }

    // donkey: AbstractHorse.standIfPossible, canPerformRearing is always true
    pub fn stand_if_possible(&self) {
        if !self.mob_entity.is_no_ai() {
            self.set_standing(STAND_TICKS);
        }
    }

    // donkey: AbstractHorse.handleEating
    fn handle_eating(&self, player: &Player, stack: &ItemStack) -> bool {
        let item = stack.get_item();
        let mut item_used = false;
        let mut heal = 0.0f32;
        let mut age_up = 0;
        let mut temper = 0;
        let mut in_love_food = false;
        if item == &Item::WHEAT {
            heal = 2.0;
            age_up = 20;
            temper = 3;
        } else if item == &Item::SUGAR {
            heal = 1.0;
            age_up = 30;
            temper = 3;
        } else if item == &Item::HAY_BLOCK {
            heal = 20.0;
            age_up = 180;
        } else if item == &Item::APPLE {
            heal = 3.0;
            age_up = 60;
            temper = 3;
        } else if item == &Item::RED_MUSHROOM {
            heal = 3.0;
            temper = 3;
        } else if item == &Item::CARROT {
            heal = 3.0;
            age_up = 60;
            temper = 3;
        } else if item == &Item::GOLDEN_CARROT {
            heal = 4.0;
            age_up = 60;
            temper = 5;
            in_love_food = true;
        } else if item == &Item::GOLDEN_APPLE || item == &Item::ENCHANTED_GOLDEN_APPLE {
            heal = 10.0;
            age_up = 240;
            temper = 10;
            in_love_food = true;
        }
        if in_love_food && self.is_tame() && self.get_age() == 0 && !self.mob_entity.is_in_love() {
            item_used = true;
            self.mob_entity
                .set_love_ticks(600, Some(player.gameprofile.id));
        }

        let living = &self.mob_entity.living_entity;
        if living.health.load() < living.get_max_health() && heal > 0.0 {
            living.heal(heal);
            item_used = true;
        }
        if self.is_baby() && age_up > 0 && !self.is_age_locked() {
            self.age_up(age_up, false);
            item_used = true;
        }
        if temper > 0
            && (item_used || !self.is_tame())
            && self.temper.load(Ordering::Relaxed) < MAX_TEMPER
        {
            self.modify_temper(temper);
            item_used = true;
        }
        if item_used {
            self.eating();
            let entity = self.get_entity();
            entity
                .world
                .load()
                .emit_game_event(GameEvent::Eat.name(), entity.pos.load());
        }
        item_used
    }

    // donkey: AbstractHorse.fedFood
    fn fed_food(&self, player: &Player, stack: &mut ItemStack) -> bool {
        let ate = self.handle_eating(player, stack);
        if ate {
            stack.decrement_unless_creative(player.gamemode.load(), 1);
        }
        ate
    }

    // donkey: AbstractHorse.doPlayerRide
    fn do_player_ride(&self, player: &Arc<Player>) -> bool {
        self.set_eating(false);
        self.clear_standing();
        let entity = &self.mob_entity.living_entity.entity;
        let player_entity = player.get_entity();
        player_entity.set_rotation(entity.yaw.load(), entity.pitch.load());
        let world = player.world();
        if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
            && let Some(passenger) = world.get_player_by_id(player.entity_id())
        {
            entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
        }
        true
    }

    // donkey: AbstractHorse.mobInteract
    fn abstract_horse_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let entity = self.get_entity();
        if entity.has_passengers() || self.is_baby() {
            return self.animal_interact(player, item_stack, Sound::EntityDonkeyEat);
        }
        if self.is_tame() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }
        if !item_stack.is_empty() {
            // Equippable.interactLivingEntity, the saddle
            if item_stack.get_item() == &Item::SADDLE
                && self.can_use_saddle_slot()
                && !self.is_saddled()
            {
                self.set_saddled(true);
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                self.play_entity_sound(Sound::EntityHorseSaddle, 1.0, 1.0);
                return true;
            }
        }
        self.do_player_ride(player)
    }

    // donkey: AbstractChestedHorse.equipChest
    fn equip_chest(&self, player: &Player, stack: &mut ItemStack) {
        self.set_has_chest(true);
        let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
            .mul_add(0.2, 1.0);
        self.play_entity_sound(Sound::EntityDonkeyChest, 1.0, pitch);
        stack.decrement_unless_creative(player.gamemode.load(), 1);
    }

    // donkey: AbstractHorse.followMommy, a bred foal looks for a bred parent but only plans the path
    fn follow_mommy(&self) {
        if !(self.is_bred() && self.is_baby() && !self.is_eating()) {
            return;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        let bounds = entity.bounding_box.load();
        let area = BoundingBox::new(
            Vector3::new(
                bounds.min.x - MOMMY_RANGE,
                bounds.min.y - MOMMY_RANGE,
                bounds.min.z - MOMMY_RANGE,
            ),
            Vector3::new(
                bounds.max.x + MOMMY_RANGE,
                bounds.max.y + MOMMY_RANGE,
                bounds.max.z + MOMMY_RANGE,
            ),
        );
        let conditions = TargetPredicate::create_non_attackable()
            .ignore_visibility()
            .set_base_max_distance(MOMMY_RANGE);
        let mommy = world
            .get_all_at_box(&area)
            .into_iter()
            .filter(|other| {
                other.get_entity().entity_id != entity.entity_id
                    && ABSTRACT_HORSES.contains(&other.get_entity().entity_type)
                    && other.get_mob().is_some_and(Mob::horse_is_bred)
                    && conditions.test(&world, Some(self), other.as_ref())
            })
            .min_by(|a, b| {
                a.get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&pos)
                    .total_cmp(&b.get_entity().pos.load().squared_distance_to_vec(&pos))
            });
        if let Some(mommy) = mommy {
            let mommy_pos = mommy.get_entity().pos.load();
            if mommy_pos.squared_distance_to_vec(&pos) > 4.0 {
                let _ = self
                    .mob_entity
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .create_path(&self.mob_entity.living_entity, mommy_pos, 0);
            }
        }
    }
}

impl AgeableMob for DonkeyEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for DonkeyEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_HORSE_FOOD)
    }

    // donkey: Donkey.getBreedOffspring, a mule when the partner is a horse
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let mut rng = self.get_random();
        let baby_type = if partner.get_entity().entity_type == &EntityType::HORSE {
            &EntityType::MULE
        } else {
            &EntityType::DONKEY
        };
        let baby = from_type(
            baby_type,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        // AbstractHorse.setOffspringAttributes
        let own = &self.mob_entity.living_entity;
        let other = partner
            .get_mob()
            .map(|mob| &mob.get_mob_entity().living_entity);
        let parent_value = |attribute: &'static pumpkin_data::attributes::Attributes| {
            (
                own.get_attribute_base(attribute),
                other.map_or_else(
                    || own.get_attribute_base(attribute),
                    |l| l.get_attribute_base(attribute),
                ),
            )
        };
        if let Some(mob) = baby.get_mob() {
            let living = &mob.get_mob_entity().living_entity;
            let (a, b) = parent_value(&Attributes::MAX_HEALTH);
            let health = create_offspring_attribute(a, b, MIN_HEALTH, MAX_HEALTH, &mut rng);
            living.set_max_health(health as f32);
            living.health.store(health as f32);
            let (a, b) = parent_value(&Attributes::JUMP_STRENGTH);
            living.set_attribute_base(
                &Attributes::JUMP_STRENGTH,
                create_offspring_attribute(a, b, MIN_JUMP_STRENGTH, MAX_JUMP_STRENGTH, &mut rng),
            );
            let (a, b) = parent_value(&Attributes::MOVEMENT_SPEED);
            living.set_attribute_base(
                &Attributes::MOVEMENT_SPEED,
                create_offspring_attribute(a, b, MIN_MOVEMENT_SPEED, MAX_MOVEMENT_SPEED, &mut rng),
            );
        }
        baby
    }
}

impl CustomSound for DonkeyEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityDonkeyDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityDonkeyHurt)
    }

    // donkey: AbstractHorse.getSoundVolume
    fn sound_volume(&self) -> f32 {
        SOUND_VOLUME
    }
}

impl Mob for DonkeyEntity {
    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn horse_is_bred(&self) -> bool {
        self.is_bred()
    }

    // donkey: AbstractHorse.isPushable
    fn mob_is_pushable(&self) -> bool {
        !self.get_entity().has_passengers()
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        // canGallop is false for every chested horse
        play_horse_step_sound(self, pos, state, false);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityDonkeyAmbient)
    }

    // donkey: AbstractHorse.getAmbientSoundInterval
    fn get_ambient_sound_interval(&self) -> i32 {
        AMBIENT_SOUND_INTERVAL
    }

    // donkey: AbstractHorse.isImmobile
    fn is_mob_immobile(&self) -> bool {
        (self.mob_entity.living_entity.is_dead_or_dying()
            && self.get_entity().has_passengers()
            && self.is_saddled())
            || self.is_eating()
            || self.is_standing()
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.is_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
        if self.get_entity().on_ground.load(Ordering::Relaxed) {
            self.player_jump_pending_scale.store(0.0);
        }
    }

    // donkey: AbstractHorse.canJump, onPlayerJump and handleStartJump with Donkey.playJumpSound
    fn on_player_start_jump(&self, jump_scale: i32) {
        if !self.is_saddled() || jump_scale <= 0 {
            return;
        }
        let scale = if jump_scale >= 90 {
            1.0
        } else {
            0.4 + 0.4 * jump_scale as f32 / 90.0
        };
        self.player_jump_pending_scale.store(scale);
        self.allow_stand_sliding.store(true, Ordering::Relaxed);
        self.stand_if_possible();
        self.play_entity_sound(Sound::EntityDonkeyJump, JUMP_SOUND_VOLUME, 1.0);
    }

    fn on_player_stop_jump(&self) {}

    // donkey: AbstractChestedHorse.openCustomInventoryScreen, 5 chest columns when chested
    fn open_custom_inventory(&self, player: &Arc<Player>) {
        let entity = self.get_entity();
        if (!entity.has_passengers() || entity.has_passenger(player.entity_id())) && self.is_tamed()
        {
            let columns = if self.has_chest() { CHEST_COLUMNS } else { 0 };
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                0,
                &player.inventory,
                self.chest_inventory.clone(),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                columns,
            )));
            player.open_mount_screen(handler, columns, entity.entity_id);
        }
    }

    fn get_temper(&self) -> i32 {
        self.temper.load(Ordering::Relaxed)
    }

    fn max_temper(&self) -> i32 {
        MAX_TEMPER
    }

    // donkey: AbstractHorse.modifyTemper
    fn modify_temper(&self, delta: i32) {
        let cur = self.temper.load(Ordering::Relaxed);
        self.temper
            .store((cur + delta).clamp(0, MAX_TEMPER), Ordering::Relaxed);
    }

    // donkey: AbstractHorse.tameWithName
    fn tame_with_name(&self, player: &Player) -> bool {
        self.owner.store(Some(player.gameprofile.id));
        self.set_tame(true);
        let entity = self.get_entity();
        player.trigger_advancement(AdvancementTrigger::TameAnimal {
            entity_type: format!("minecraft:{}", entity.entity_type.resource_name),
        });
        entity.world.load().broadcast_entity_event(
            entity,
            EntityStatus::TamingSucceeded,
            ActorEventID::TamingSucceeded,
        );
        true
    }

    // donkey: AbstractHorse.makeMad
    fn make_mad(&self) {
        if !self.is_standing() {
            self.stand_if_possible();
            self.make_sound(Sound::EntityDonkeyAngry);
        }
    }

    // donkey: AbstractHorse.hurtServer, a third of the hits rear the donkey up
    fn pre_damage(
        &self,
        _damage_type: pumpkin_data::damage::DamageType,
        _source: Option<&dyn EntityBase>,
    ) -> bool {
        if self.get_random().random_range(0..3) == 0 {
            self.stand_if_possible();
        }
        true
    }

    // donkey: AbstractHorse.causeFallDamage landing sound
    fn on_cause_fall_damage(&self, fall_distance: f32) {
        if fall_distance > 1.0 {
            let sound = if self.is_baby() {
                Sound::EntityBabyHorseLand
            } else {
                Sound::EntityHorseLand
            };
            self.play_entity_sound(sound, LAND_SOUND_VOLUME, 1.0);
        }
    }

    // donkey: AbstractHorse.aiStep, slow healing, grazing and the foal following its mother
    fn mob_ai_step(&self) {
        let living = &self.mob_entity.living_entity;
        if !living.is_alive() {
            return;
        }
        let entity = self.get_entity();
        if self.get_random().random_range(0..900) == 0
            && living.death_time.load(Ordering::Relaxed) == 0
        {
            living.heal(1.0);
        }
        if !self.is_eating()
            && !entity.has_passengers()
            && self.get_random().random_range(0..300) == 0
            && entity
                .world
                .load()
                .get_block(&entity.block_pos.load().down())
                == &Block::GRASS_BLOCK
        {
            self.set_eating(true);
        }
        if self.is_eating() {
            let counter = self.eating_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if counter > EATING_GRASS_TICKS {
                self.eating_counter.store(0, Ordering::Relaxed);
                self.set_eating(false);
            }
        }
        self.follow_mommy();
    }

    // donkey: AbstractHorse.tick counters
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let mouth = self.mouth_counter.load(Ordering::Relaxed);
        if mouth > 0 {
            if mouth + 1 > MOUTH_OPEN_TICKS {
                self.mouth_counter.store(0, Ordering::Relaxed);
                self.set_flag(FLAG_OPEN_MOUTH, false);
            } else {
                self.mouth_counter.store(mouth + 1, Ordering::Relaxed);
            }
        }
        let stand = self.stand_counter.load(Ordering::Relaxed);
        if stand > 0 {
            self.stand_counter.store(stand - 1, Ordering::Relaxed);
            if stand - 1 <= 0 {
                self.clear_standing();
            }
        }
        if !self.is_standing() {
            self.allow_stand_sliding.store(false, Ordering::Relaxed);
        }
    }

    // donkey: AbstractChestedHorse.dropEquipment, the chest and what it holds
    fn drop_inventory(&self) {
        let entity = self.get_entity();
        for slot in 0..self.chest_inventory.size() {
            let stack = self.chest_inventory.remove_stack(slot);
            entity.spawn_at_location(stack);
        }
        if self.has_chest() {
            entity.spawn_at_location(ItemStack::new(1, &Item::CHEST));
            self.set_has_chest(false);
        }
    }

    // donkey: AbstractHorse.finalizeSpawn, randomizeAttributes
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.randomize_attributes();
        self.finalize_spawn_super(group_data)
    }

    // donkey: Donkey.canMate
    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        if partner.get_entity().entity_id == self.get_entity().entity_id {
            return false;
        }
        let partner_type = partner.get_entity().entity_type;
        if partner_type != &EntityType::DONKEY && partner_type != &EntityType::HORSE {
            return false;
        }
        self.can_parent()
            && partner.cast_any().downcast_ref::<Self>().map_or_else(
                || {
                    partner.get_mob().is_some_and(|mob| {
                        mob.is_tamed()
                            && mob.get_mob_entity().is_in_love()
                            && !mob.is_baby_mob()
                            && !partner.get_entity().has_passengers()
                            && mob.get_mob_entity().living_entity.health.load()
                                >= mob.get_mob_entity().living_entity.get_max_health()
                    })
                },
                Self::can_parent,
            )
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("EatingHaystack", self.is_eating());
        nbt.put_bool("Bred", self.is_bred());
        nbt.put_int("Temper", self.temper.load(Ordering::Relaxed));
        nbt.put_bool("Tame", self.is_tame());
        if let Some(owner) = self.owner.load() {
            nbt.put_uuid("Owner", owner);
        }
        nbt.put_bool("ChestedHorse", self.has_chest());
        if self.has_chest() {
            self.chest_inventory.write_inventory_nbt(nbt, false);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.set_eating(nbt.get_bool("EatingHaystack").unwrap_or(false));
        self.set_flag(FLAG_BRED, nbt.get_bool("Bred").unwrap_or(false));
        self.temper
            .store(nbt.get_int("Temper").unwrap_or(0), Ordering::Relaxed);
        self.set_tame(nbt.get_bool("Tame").unwrap_or(false));
        if let Some(owner) = nbt.get_uuid("Owner") {
            self.owner.store(Some(owner));
        }
        let chested = nbt.get_bool("ChestedHorse").unwrap_or(false);
        self.set_has_chest(chested);
        let mut stacks = vec![ItemStack::EMPTY.clone(); self.chest_inventory.size()];
        if chested {
            self.chest_inventory.read_data(nbt, &mut stacks);
        }
        for (slot, stack) in stacks.into_iter().enumerate() {
            self.chest_inventory.set_stack(slot, stack);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::donkey::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::donkey::DATA_ID_CHEST,
            self.has_chest(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::donkey::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    // donkey: AbstractChestedHorse.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let sneaking = player.living_entity.entity.is_sneaking();
        let should_open_inventory = !self.is_baby() && self.is_tame() && sneaking;
        let holds_golden_dandelion = player.inventory().held_item().get_item()
            == &Item::GOLDEN_DANDELION
            || player.inventory().off_hand_item().get_item() == &Item::GOLDEN_DANDELION;
        if !self.get_entity().has_passengers()
            && !should_open_inventory
            && (!self.is_baby() || !holds_golden_dandelion)
            && !item_stack.is_empty()
        {
            if self.is_food(item_stack) {
                return self.fed_food(player, item_stack);
            }
            if !self.is_tame() {
                self.make_mad();
                return true;
            }
            if !self.has_chest() && item_stack.get_item() == &Item::CHEST {
                self.equip_chest(player, item_stack);
                return true;
            }
        }
        self.abstract_horse_interact(player, item_stack)
    }
}

// donkey: RandomStandGoal
struct RandomStandGoal {
    next_stand: i32,
}

impl RandomStandGoal {
    const fn new() -> Self {
        Self {
            next_stand: -AMBIENT_SOUND_INTERVAL,
        }
    }
}

impl Goal for RandomStandGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(donkey) = mob.cast_any().downcast_ref::<DonkeyEntity>() else {
            return false;
        };
        self.next_stand += 1;
        if self.next_stand > 0 && mob.get_random().random_range(0..1000) < self.next_stand {
            self.next_stand = -AMBIENT_SOUND_INTERVAL;
            return !donkey.is_mob_immobile() && mob.get_random().random_range(0..10) == 0;
        }
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(donkey) = mob.cast_any().downcast_ref::<DonkeyEntity>() {
            donkey.stand_if_possible();
            donkey.make_sound(Sound::EntityDonkeyAmbient);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn donkey_in(world: &Arc<World>) -> Arc<DonkeyEntity> {
        DonkeyEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::DONKEY,
        ))
    }

    #[tokio::test]
    async fn a_new_donkey_takes_its_attributes_from_the_data_and_randomizes_health_on_spawn()
    -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let donkey = donkey_in(&world);
        let living = &donkey.mob_entity.living_entity;
        assert!((living.get_max_health() - 53.0).abs() < f32::EPSILON);
        assert!((living.get_attribute_value(&Attributes::JUMP_STRENGTH) - 0.5).abs() < 1.0e-6);
        assert!((living.get_attribute_value(&Attributes::MOVEMENT_SPEED) - 0.175).abs() < 1.0e-6);
        donkey.randomize_attributes();
        assert!((15.0..=30.0).contains(&living.get_max_health()));
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_angry_donkey_rears_for_twenty_ticks_then_stands_on_all_fours() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let donkey = donkey_in(&world);
        donkey.make_mad();
        assert!(donkey.is_standing());
        assert!(donkey.is_mob_immobile());
        for _ in 0..STAND_TICKS - 1 {
            donkey.mob_tick(donkey.as_ref());
        }
        assert!(donkey.is_standing());
        donkey.mob_tick(donkey.as_ref());
        assert!(!donkey.is_standing());
        assert!(!donkey.is_mob_immobile());
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn the_chest_and_its_items_survive_a_save() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let donkey = donkey_in(&world);
        donkey.set_tame(true);
        donkey.set_has_chest(true);
        donkey
            .chest_inventory
            .set_stack(3, ItemStack::new(7, &Item::COBBLESTONE));
        let mut nbt = NbtCompound::new();
        donkey.mob_write_nbt(&mut nbt);

        let loaded = donkey_in(&world);
        loaded.mob_read_nbt(&nbt);
        assert!(loaded.is_tame());
        assert!(loaded.has_chest());
        let stack = loaded.chest_inventory.get_stack(3);
        assert_eq!(stack.item_count, 7);
        assert_eq!(stack.get_item(), &Item::COBBLESTONE);
        world.level.shutdown().await;
        Ok(())
    }

    #[test]
    fn offspring_attributes_stay_inside_the_vanilla_ranges() {
        let mut rng = rand::rng();
        for _ in 0..2000 {
            let health = create_offspring_attribute(53.0, 15.0, MIN_HEALTH, MAX_HEALTH, &mut rng);
            assert!((MIN_HEALTH..=MAX_HEALTH).contains(&health));
            let speed = create_offspring_attribute(
                0.175,
                0.3,
                MIN_MOVEMENT_SPEED,
                MAX_MOVEMENT_SPEED,
                &mut rng,
            );
            assert!((MIN_MOVEMENT_SPEED..=MAX_MOVEMENT_SPEED).contains(&speed));
        }
    }
}
