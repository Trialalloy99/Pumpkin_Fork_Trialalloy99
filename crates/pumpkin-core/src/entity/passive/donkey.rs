use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
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
            ABSTRACT_HORSES, FLAG_TAME, HORSE_SOUND_VOLUME, HorseCore, HorseLike, MAX_HEALTH,
            MAX_JUMP_STRENGTH, MAX_MOVEMENT_SPEED, MIN_HEALTH, MIN_JUMP_STRENGTH,
            MIN_MOVEMENT_SPEED, RandomStandGoal, controlling_passenger_horse,
            create_offspring_attribute, generate_max_health, tick_ridden_horse,
        },
        animal::Animal,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

// donkey: HORSE_TEMPT_ITEMS (AbstractHorse.addBehaviourGoals)
const TEMPT_ITEMS: &[&Item] = &[
    &Item::GOLDEN_APPLE,
    &Item::ENCHANTED_GOLDEN_APPLE,
    &Item::GOLDEN_CARROT,
];

// donkey: AbstractChestedHorse constants
const CHEST_COLUMNS: usize = 5;
const CHEST_SLOTS: usize = 15;

/// Donkey and mule, vanilla `AbstractChestedHorse` with `Donkey` and `Mule`.
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
    core: HorseCore,
}

impl DonkeyEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);

        // donkey: AbstractChestedHorse.getQuadLeashOffsets
        mob_entity
            .living_entity
            .entity
            .quad_leash_params
            .store(Some([0.04, 0.41, 0.18, 0.73]));
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
            core: HorseCore::default(),
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
            goal_selector.add_goal(9, Box::new(RandomStandGoal::default()));
        };

        mob_arc
    }

    fn is_mule(&self) -> bool {
        self.get_entity().entity_type == &EntityType::MULE
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

    // donkey: AbstractChestedHorse.randomizeAttributes
    fn randomize_attributes(&self) {
        let max_health = generate_max_health(&mut self.get_random());
        self.mob_entity.living_entity.set_max_health(max_health);
        self.mob_entity.living_entity.health.store(max_health);
    }

    // donkey: AbstractChestedHorse.equipChest
    fn equip_chest(&self, player: &Player, stack: &mut ItemStack) {
        self.set_has_chest(true);
        let pitch = (self.get_random().random::<f32>() - self.get_random().random::<f32>())
            .mul_add(0.2, 1.0);
        let sound = if self.is_mule() {
            Sound::EntityMuleChest
        } else {
            Sound::EntityDonkeyChest
        };
        self.play_entity_sound(sound, 1.0, pitch);
        stack.decrement_unless_creative(player.gamemode.load(), 1);
    }
}

impl HorseLike for DonkeyEntity {
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
            pumpkin_data::tracked_data::donkey::DATA_ID_FLAGS,
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
        if self.is_mule() {
            Sound::EntityMuleEat
        } else {
            Sound::EntityDonkeyEat
        }
    }

    fn angry_sound(&self) -> Sound {
        if self.is_mule() {
            Sound::EntityMuleAngry
        } else {
            Sound::EntityDonkeyAngry
        }
    }

    fn jump_sound(&self) -> Sound {
        if self.is_mule() {
            Sound::EntityMuleJump
        } else {
            Sound::EntityDonkeyJump
        }
    }

    // chested horses never gallop
    fn can_gallop(&self) -> bool {
        false
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

    // donkey: Donkey.getBreedOffspring, a mule when the partner is a horse; Mule.getBreedOffspring
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let mut rng = self.get_random();
        let baby_type = if self.is_mule() || partner.get_entity().entity_type == &EntityType::HORSE
        {
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
        if self.is_mule() {
            return baby;
        }
        // AbstractHorse.setOffspringAttributes
        let own = &self.mob_entity.living_entity;
        let other = partner
            .get_mob()
            .map(|mob| &mob.get_mob_entity().living_entity);
        let parent_values = |attribute: &'static Attributes| {
            (
                own.get_attribute_base(attribute),
                other.map_or_else(
                    || own.get_attribute_base(attribute),
                    |living| living.get_attribute_base(attribute),
                ),
            )
        };
        if let Some(mob) = baby.get_mob() {
            let living = &mob.get_mob_entity().living_entity;
            let (a, b) = parent_values(&Attributes::MAX_HEALTH);
            let health = create_offspring_attribute(a, b, MIN_HEALTH, MAX_HEALTH, &mut rng);
            living.set_max_health(health as f32);
            living.health.store(health as f32);
            let (a, b) = parent_values(&Attributes::JUMP_STRENGTH);
            living.set_attribute_base(
                &Attributes::JUMP_STRENGTH,
                create_offspring_attribute(a, b, MIN_JUMP_STRENGTH, MAX_JUMP_STRENGTH, &mut rng),
            );
            let (a, b) = parent_values(&Attributes::MOVEMENT_SPEED);
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
        Some(if self.is_mule() {
            Sound::EntityMuleDeath
        } else {
            Sound::EntityDonkeyDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_mule() {
            Sound::EntityMuleHurt
        } else {
            Sound::EntityDonkeyHurt
        })
    }

    // donkey: AbstractHorse.getSoundVolume
    fn sound_volume(&self) -> f32 {
        HORSE_SOUND_VOLUME
    }
}

impl Mob for DonkeyEntity {
    crate::horse_mob_hooks!();
    crate::horse_mob_tick!();

    fn mob_play_step_sound(&self, pos: &BlockPos, state: &BlockState) {
        self.horse_step_sound(pos, state);
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
        Some(if self.is_mule() {
            Sound::EntityMuleAmbient
        } else {
            Sound::EntityDonkeyAmbient
        })
    }

    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        controlling_passenger_horse(self, self.is_saddled())
    }

    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        tick_ridden_horse(self, controller);
        if self.get_entity().on_ground.load(Ordering::Relaxed) {
            self.core.jump_scale.store(0.0);
        }
    }

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

    // donkey: Donkey.canMate, a mule never mates
    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        if self.is_mule() || partner.get_entity().entity_id == self.get_entity().entity_id {
            return false;
        }
        let partner_type = partner.get_entity().entity_type;
        if partner_type != &EntityType::DONKEY && partner_type != &EntityType::HORSE {
            return false;
        }
        self.can_parent()
            && partner.get_mob().is_some_and(|mob| {
                let entity = partner.get_entity();
                let living = &mob.get_mob_entity().living_entity;
                mob.is_tamed()
                    && mob.get_mob_entity().is_in_love()
                    && !mob.is_baby_mob()
                    && !entity.has_passengers()
                    && !entity.has_vehicle()
                    && living.health.load() >= living.get_max_health()
            })
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        self.write_horse_nbt(nbt);
        nbt.put_bool("ChestedHorse", self.has_chest());
        if self.has_chest() {
            self.chest_inventory.write_inventory_nbt(nbt, false);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.read_horse_nbt(nbt);
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
            if !self.has_chest() && item_stack.get_item() == &Item::CHEST {
                self.equip_chest(player, item_stack);
                return true;
            }
        }
        self.abstract_horse_interact(player, item_stack, |_, _, _| false)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;
    use crate::entity::passive::abstract_horse::STAND_TICKS;
    use pumpkin_util::math::vector3::Vector3;

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
        assert!(donkey.is_horse_standing());
        assert!(donkey.is_mob_immobile());
        for _ in 0..STAND_TICKS - 1 {
            donkey.mob_tick(donkey.as_ref());
        }
        assert!(donkey.is_horse_standing());
        donkey.mob_tick(donkey.as_ref());
        assert!(!donkey.is_horse_standing());
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
