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
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
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
            create_offspring_attribute, generate_jump_strength, generate_max_health,
            generate_speed, tick_ridden_horse,
        },
        animal::Animal,
    },
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[
    &Item::GOLDEN_APPLE,
    &Item::ENCHANTED_GOLDEN_APPLE,
    &Item::GOLDEN_CARROT,
];

pub struct HorseEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub flags: AtomicU8,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
    core: HorseCore,
}

impl HorseEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        // horse: AbstractHorse.getQuadLeashOffsets
        mob_entity
            .living_entity
            .entity
            .quad_leash_params
            .store(Some([0.04, 0.52, 0.23, 0.87]));
        let horse = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(0),
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
            // horse: vanilla goal priorities (AbstractHorse.registerGoals)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
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

    #[must_use]
    pub fn get_variant(&self) -> i32 {
        self.variant.load(Ordering::Relaxed)
    }

    pub fn set_variant(&self, variant: i32) {
        self.variant.store(variant, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_TYPE_VARIANT,
            VarInt(variant),
        );
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

    // horse: Horse.randomizeAttributes
    fn randomize_attributes(&self) {
        let mut rng = self.get_random();
        let living = &self.mob_entity.living_entity;
        let max_health = generate_max_health(&mut rng);
        living.set_max_health(max_health);
        living.health.store(max_health);
        living.set_attribute_base(&Attributes::JUMP_STRENGTH, generate_jump_strength(&mut rng));
        living.set_attribute_base(&Attributes::MOVEMENT_SPEED, generate_speed(&mut rng));
    }

    // horse: saddle lives in SADDLE slot (AbstractHorse.isSaddled)
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

    // horse: body armor in BODY slot (Horse.isWearingArmor)
    #[must_use]
    pub fn is_wearing_armor(&self) -> bool {
        let equipment = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        equipment
            .equipment
            .get(&EquipmentSlot::BODY)
            .is_some_and(|stack| !stack.is_empty())
    }

    pub fn equip_armor(&self, item_stack: &ItemStack) {
        self.mob_entity
            .set_item_slot_and_drop_when_killed(&EquipmentSlot::BODY, item_stack.clone());
        self.armor_inventory.set_stack(0, item_stack.clone());
        let entity = self.get_entity();
        let world = entity.world.load();
        world.play_sound(
            Sound::EntityHorseArmor,
            SoundCategory::Neutral,
            &entity.pos.load(),
        );
    }
}

#[must_use]
pub fn is_horse_armor(item: &Item) -> bool {
    item == &Item::LEATHER_HORSE_ARMOR
        || item == &Item::COPPER_HORSE_ARMOR
        || item == &Item::IRON_HORSE_ARMOR
        || item == &Item::GOLDEN_HORSE_ARMOR
        || item == &Item::DIAMOND_HORSE_ARMOR
        || item == &Item::NETHERITE_HORSE_ARMOR
}

impl AgeableMob for HorseEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl HorseLike for HorseEntity {
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
            pumpkin_data::tracked_data::horse::DATA_ID_FLAGS,
            flags as i8,
        );
    }

    fn horse_saddled(&self) -> bool {
        self.is_saddled()
    }

    fn set_horse_saddle(&self, saddled: bool) {
        self.set_saddled(saddled);
    }

    // horse: Horse.getEatingSound
    fn horse_eat_sound(&self) -> Sound {
        if self.is_baby() {
            Sound::EntityBabyHorseEat
        } else {
            Sound::EntityHorseEat
        }
    }

    // horse: Horse.getAngrySound
    fn angry_sound(&self) -> Sound {
        if self.is_baby() {
            Sound::EntityBabyHorseAngry
        } else {
            Sound::EntityHorseAngry
        }
    }

    fn jump_sound(&self) -> Sound {
        Sound::EntityHorseJump
    }
}

impl Animal for HorseEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_HORSE_FOOD)
    }

    // horse: AbstractHorse.setOffspringAttributes
    // horse: Horse.getBreedOffspring, a mule with a donkey, else a foal with mixed coat and markings
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let mut rng = self.get_random();
        let donkey_partner = partner.get_entity().entity_type == &EntityType::DONKEY;
        let baby = from_type(
            if donkey_partner {
                &EntityType::MULE
            } else {
                &EntityType::HORSE
            },
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        if !donkey_partner && let Some(foal) = baby.cast_any().downcast_ref::<Self>() {
            // coat: 4/9 either parent, 1/9 random; markings: 2/5 either parent, 1/5 random
            let partner_variant = partner
                .cast_any()
                .downcast_ref::<Self>()
                .map_or_else(|| self.get_variant(), Self::get_variant);
            let select_skin = rng.random_range(0..9);
            let color = if select_skin < 4 {
                self.get_variant() & 0xFF
            } else if select_skin < 8 {
                partner_variant & 0xFF
            } else {
                rng.random_range(0..7)
            };
            let select_marking = rng.random_range(0..5);
            let style = if select_marking < 2 {
                (self.get_variant() >> 8) & 0xFF
            } else if select_marking < 4 {
                (partner_variant >> 8) & 0xFF
            } else {
                rng.random_range(0..5)
            };
            foal.set_variant(color | (style << 8));
        }
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

impl CustomSound for HorseEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityBabyHorseDeath
        } else {
            Sound::EntityHorseDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityBabyHorseHurt
        } else {
            Sound::EntityHorseHurt
        })
    }

    // horse: AbstractHorse.getSoundVolume
    fn sound_volume(&self) -> f32 {
        HORSE_SOUND_VOLUME
    }
}

impl Mob for HorseEntity {
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
        Some(if self.is_baby() {
            Sound::EntityBabyHorseAmbient
        } else {
            Sound::EntityHorseAmbient
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

    // horse: open mount screen with 0 chest columns (AbstractHorse.openCustomInventoryScreen)
    fn open_custom_inventory(&self, player: &Arc<Player>) {
        let entity = self.get_entity();
        if (!entity.has_passengers() || entity.has_passenger(player.entity_id())) && self.is_tamed()
        {
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                0,
                &player.inventory,
                Arc::new(SimpleInventory::new(0)),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                0,
            )));
            player.open_mount_screen(handler, 0, entity.entity_id);
        }
    }

    // horse: AbstractHorse.finalizeSpawn, Horse.randomizeAttributes and the coat
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let color = self.get_random().random_range(0..7);
        let style = self.get_random().random_range(0..5);
        self.set_variant(color | (style << 8));
        self.randomize_attributes();
        self.finalize_spawn_super(group_data)
    }

    // horse: Horse.canMate
    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        if partner.get_entity().entity_id == self.get_entity().entity_id {
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
        nbt.put_int("Variant", self.get_variant());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.read_horse_nbt(nbt);
        if let Some(variant) = nbt.get_int("Variant") {
            self.set_variant(variant);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::horse::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_TYPE_VARIANT,
            VarInt(self.get_variant()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::horse::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    // horse: Horse.mobInteract and AbstractHorse.mobInteract with the body armor
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
            // AbstractHorse.equipBodyArmor
            if is_horse_armor(stack.get_item()) && !horse.is_wearing_armor() {
                horse.equip_armor(stack);
                stack.decrement_unless_creative(player.gamemode.load(), 1);
                return true;
            }
            false
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;
    use crate::entity::passive::abstract_horse::STAND_TICKS;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn horse_in(world: &Arc<World>) -> Arc<HorseEntity> {
        HorseEntity::new(Entity::new(
            world.clone(),
            pumpkin_util::math::vector3::Vector3::new(0.5, 64.0, 0.5),
            &EntityType::HORSE,
        ))
    }

    #[tokio::test]
    async fn a_horse_randomizes_its_attributes_only_when_it_spawns() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let horse = horse_in(&world);
        let living = &horse.mob_entity.living_entity;
        assert!((living.get_max_health() - 53.0).abs() < f32::EPSILON);
        horse.randomize_attributes();
        assert!((15.0..=30.0).contains(&living.get_max_health()));
        let speed = living.get_attribute_value(&Attributes::MOVEMENT_SPEED);
        assert!((MIN_MOVEMENT_SPEED..=MAX_MOVEMENT_SPEED).contains(&speed));
        world.level.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_angry_horse_rears_and_settles_after_twenty_ticks() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let horse = horse_in(&world);
        horse.make_mad();
        assert!(horse.is_horse_standing());
        for _ in 0..STAND_TICKS {
            horse.mob_tick(horse.as_ref());
        }
        assert!(!horse.is_horse_standing());
        world.level.shutdown().await;
        Ok(())
    }
}
