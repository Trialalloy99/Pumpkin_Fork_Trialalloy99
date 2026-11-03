use std::sync::{
    Arc, Weak,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::passive::abstract_horse::{HORSE_SOUND_VOLUME, HorseCore, HorseLike};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        escape_danger::EscapeDangerGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{
        Mob, MobEntity,
        spawn::{SpawnGroupData, SpawnReason},
    },
    passive::{
        abstract_horse::{FLAG_SADDLE, FLAG_TAME, controlling_passenger_horse, tick_ridden_horse},
        animal::Animal,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::RED_MUSHROOM, &Item::BAKED_POTATO];

fn generate_zombie_horse_jump_strength(rng: &mut impl rand::Rng) -> f64 {
    0.5 + rng.random::<f64>() * 0.06666666666666667
        + rng.random::<f64>() * 0.06666666666666667
        + rng.random::<f64>() * 0.06666666666666667
}

fn generate_zombie_horse_speed(rng: &mut impl rand::Rng) -> f64 {
    (9.0 + rng.random::<f64>() + rng.random::<f64>() + rng.random::<f64>()) / 42.16
}

pub struct ZombieHorseEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub flags: AtomicU8,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
    core: HorseCore,
}

impl ZombieHorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        // zombie_horse: ZombieHorse.getQuadLeashOffsets
        mob_entity
            .living_entity
            .entity
            .quad_leash_params
            .store(Some([0.04, 0.41, 0.18, 0.73]));
        let horse = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            flags: AtomicU8::new(0),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
            armor_inventory: Arc::new(SimpleInventory::new(1)),
            core: HorseCore::default(),
        };
        let mob_arc = Arc::new(horse);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::new(0.7)));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flag(flag)
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        self.put_flag(flag, val);
    }

    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.is_horse_tame()
    }

    pub fn set_tame(&self, val: bool) {
        self.put_flag(FLAG_TAME, val);
    }

    // zombie_horse: ZombieHorse.randomizeAttributes
    fn randomize_attributes(&self) {
        let mut rng = self.get_random();
        let living = &self.mob_entity.living_entity;
        living.set_attribute_base(
            &Attributes::JUMP_STRENGTH,
            generate_zombie_horse_jump_strength(&mut rng),
        );
        living.set_attribute_base(
            &Attributes::MOVEMENT_SPEED,
            generate_zombie_horse_speed(&mut rng),
        );
    }

    #[must_use]
    pub fn is_saddled(&self) -> bool {
        self.has_flag(FLAG_SADDLE)
    }

    pub fn set_saddled(&self, val: bool) {
        self.set_flag(FLAG_SADDLE, val);
        if val {
            self.saddle_inventory
                .set_stack(0, ItemStack::new(1, &Item::SADDLE));
        } else {
            self.saddle_inventory.set_stack(0, ItemStack::EMPTY.clone());
        }
    }
}

impl HorseLike for ZombieHorseEntity {
    fn core(&self) -> &HorseCore {
        &self.core
    }

    fn horse_flags(&self) -> &AtomicU8 {
        &self.flags
    }

    fn horse_temper(&self) -> &AtomicI32 {
        &self.temper
    }

    fn horse_owner(&self) -> &AtomicCell<Option<Uuid>> {
        &self.owner
    }

    fn store_flags(&self, flags: u8) {
        self.flags.store(flags, Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::zombie_horse::DATA_ID_FLAGS,
            flags as i8,
        );
    }

    fn horse_saddled(&self) -> bool {
        self.is_saddled()
    }

    fn set_horse_saddle(&self, saddled: bool) {
        self.set_saddled(saddled);
    }

    fn horse_eat_sound(&self) -> Sound {
        Sound::EntityZombieHorseEat
    }

    fn angry_sound(&self) -> Sound {
        Sound::EntityZombieHorseAngry
    }

    fn jump_sound(&self) -> Sound {
        Sound::EntityHorseJump
    }

    fn can_gallop(&self) -> bool {
        false
    }
}

impl AgeableMob for ZombieHorseEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for ZombieHorseEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack
            .item
            .has_tag(&tag::Item::MINECRAFT_ZOMBIE_HORSE_FOOD)
            || item_stack.get_item() == &Item::BAKED_POTATO
    }
}

impl CustomSound for ZombieHorseEntity {
    fn sound_volume(&self) -> f32 {
        HORSE_SOUND_VOLUME
    }

    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseHurt)
    }
}

impl Mob for ZombieHorseEntity {
    crate::horse_mob_hooks!();
    crate::horse_mob_tick!();

    // zombie horse: spear charge mount factor (ZombieHorse.chargeSpeedModifier)
    fn charge_speed_modifier(&self) -> f32 {
        1.4
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        true
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZombieHorseAmbient)
    }

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        self.horse_step_sound(pos, state);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn sun_protection_slot(&self) -> EquipmentSlot {
        EquipmentSlot::BODY
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.is_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
    }

    fn open_custom_inventory(&self, player: &Arc<Player>) {
        if !self.is_baby() && self.is_tamed() {
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                0,
                &player.inventory,
                Arc::new(SimpleInventory::new(0)),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                0,
            )));
            player.open_mount_screen(handler, 0, self.get_entity().entity_id);
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.randomize_attributes();
        let result = self.finalize_spawn_super(group_data);
        // zombie_horse: natural jockey spawn holding iron spear (ZombieHorse.finalizeSpawn)
        let reason = self.get_mob_entity().spawn_reason.load(Ordering::Relaxed);
        if reason == SpawnReason::Natural as u8 {
            let pos = self.get_entity().pos.load();
            let zombie = from_type(&EntityType::ZOMBIE, pos, world, Uuid::new_v4());
            if let Some(living) = zombie.get_living_entity() {
                if let Ok(mut eq) = living.entity_equipment.lock() {
                    eq.put(
                        &EquipmentSlot::MAIN_HAND,
                        ItemStack::new(1, &Item::IRON_SPEAR),
                    );
                }
                living.send_equipment_changes(&[(
                    EquipmentSlot::MAIN_HAND,
                    ItemStack::new(1, &Item::IRON_SPEAR),
                )]);
            }
            if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
                self.get_entity().add_passenger(this_base, zombie.clone());
            }
            world.spawn_entity(zombie);
        }
        result
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        self.write_horse_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.read_horse_nbt(nbt);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::zombie_horse::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::zombie_horse::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    // zombie_horse: ZombieHorse.mobInteract and AbstractHorse.mobInteract with the body armor
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let sneaking = player.living_entity.entity.is_sneaking();
        let should_open_inventory = !self.is_baby() && self.is_horse_tame() && sneaking;
        let holds_golden_dandelion = player.inventory().held_item().get_item()
            == &Item::GOLDEN_DANDELION
            || player.inventory().off_hand_item().get_item() == &Item::GOLDEN_DANDELION;
        if !self.get_entity().has_passengers()
            && !should_open_inventory
            && (!self.is_baby() || !holds_golden_dandelion)
            && !item_stack.is_empty()
        {
            if self.is_food(item_stack) {
                return self.horse_fed_food(player, item_stack);
            }
            if !self.is_horse_tame() {
                self.horse_make_mad();
                return true;
            }
        }
        self.abstract_horse_interact(player, item_stack, |horse, player, stack| {
            if super::horse::is_horse_armor(stack.get_item())
                && horse.armor_inventory.get_stack(0).is_empty()
            {
                let armor = ItemStack::new(1, stack.get_item());
                horse
                    .mob_entity
                    .set_item_slot_and_drop_when_killed(&EquipmentSlot::BODY, armor.clone());
                horse.armor_inventory.set_stack(0, armor);
                stack.decrement_unless_creative(player.gamemode.load(), 1);
                horse.play_entity_sound(Sound::EntityHorseArmor, 1.0, 1.0);
                return true;
            }
            false
        })
    }
}
