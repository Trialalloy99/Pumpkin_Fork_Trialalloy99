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
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::mount_screen_handler::MountScreenHandler;
use pumpkin_inventory::{Inventory, inventory::SimpleInventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::passive::tamable::is_untamed;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        llama_follow_caravan::LlamaFollowCaravanGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, ranged_attack::RangedAttackGoal, revenge::RevengeGoal,
        run_around_like_crazy::RunAroundLikeCrazyGoal, swim::SwimGoal, tempt::TemptGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, RangedAttackMob, spawn::SpawnGroupData},
    passive::{
        abstract_horse::{FLAG_BRED, FLAG_TAME, generate_max_health, tame_horse, throw_rider_off},
        animal::{Animal, get_carpet_color_from_item},
        trader_llama::TraderLlamaEntity,
    },
    player::Player,
    projectile::llama_spit::LlamaSpitEntity,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::HAY_BLOCK];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum LlamaVariant {
    #[default]
    Creamy = 0,
    White = 1,
    Brown = 2,
    Gray = 3,
}

impl LlamaVariant {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::White,
            2 => Self::Brown,
            3 => Self::Gray,
            _ => Self::Creamy,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub fn random_variant() -> Self {
        let mut rng = rand::rng();
        match rng.random_range(0..4) {
            1 => Self::White,
            2 => Self::Brown,
            3 => Self::Gray,
            _ => Self::Creamy,
        }
    }
}

/// Vanilla `Wolf.avoidLlama`: the stronger the llama, the likelier a wolf flees.
pub fn scares_wolves(threat: &dyn EntityBase) -> bool {
    let any = threat.cast_any();
    let strength = any
        .downcast_ref::<LlamaEntity>()
        .map(LlamaEntity::get_strength)
        .or_else(|| {
            any.downcast_ref::<TraderLlamaEntity>()
                .map(TraderLlamaEntity::get_strength)
        });
    strength.is_some_and(|strength| strength >= rand::rng().random_range(0..5))
}

/// Vanilla `LlamaAttackWolfGoal`: untamed wolves within a quarter of the follow range.
pub const LLAMA_HUNTS: &[Hunt] = &[Hunt::new(2, TargetMatch::Type(&EntityType::WOLF), false)
    .every(16)
    .only_mobs(is_untamed)
    .range_scale(0.25)];

pub struct LlamaEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub strength: AtomicI32,
    pub carpet_color: AtomicCell<Option<u8>>,
    pub flags: AtomicU8,
    pub has_chest: AtomicBool,
    pub temper: AtomicI32,
    pub owner: AtomicCell<Option<Uuid>>,
    pub chest_inventory: Arc<SimpleInventory>,
    pub saddle_inventory: Arc<SimpleInventory>,
    pub armor_inventory: Arc<SimpleInventory>,
}

impl LlamaEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mut rng = rand::rng();
        let variant = LlamaVariant::random_variant();

        // llama: vanilla attributes (Llama.createAttributes, Llama.setRandomStrength)
        let max_health = generate_max_health(&mut rng);
        mob_entity.living_entity.set_max_health(max_health);
        mob_entity.living_entity.health.store(max_health);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.175);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::JUMP_STRENGTH, 0.5);

        let max_str = if rng.random_range(0.0..1.0) < 0.04 {
            5
        } else {
            3
        };
        let strength = rng.random_range(1..=max_str);

        let llama = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(variant.id()),
            strength: AtomicI32::new(strength),
            carpet_color: AtomicCell::new(None),
            flags: AtomicU8::new(0),
            has_chest: AtomicBool::new(false),
            temper: AtomicI32::new(0),
            owner: AtomicCell::new(None),
            chest_inventory: Arc::new(SimpleInventory::new(15)),
            saddle_inventory: Arc::new(SimpleInventory::new(1)),
            armor_inventory: Arc::new(SimpleInventory::new(1)),
        };
        let mob_arc = Arc::new(llama);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let ranged_weak: Weak<dyn RangedAttackMob> = {
            let ranged_arc: Arc<dyn RangedAttackMob> = mob_arc.clone();
            Arc::downgrade(&ranged_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            // llama: vanilla goal priorities (Llama.registerGoals)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.2));
            goal_selector.add_goal(1, Box::new(RunAroundLikeCrazyGoal::new(1.2)));
            goal_selector.add_goal(
                2,
                Box::new(RangedAttackGoal::new(ranged_weak, 1.25, 40, 20.0)),
            );
            goal_selector.add_goal(3, Box::new(LlamaFollowCaravanGoal::new(2.1)));
            goal_selector.add_goal(4, BreedGoal::new(1.0));
            goal_selector.add_goal(5, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(6, Box::new(FollowParentGoal::new(1.0)));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::new(0.7)));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(9, Box::new(RandomLookAroundGoal::default()));

            target_selector.add_goal(1, Box::new(RevengeGoal::new(true)));
            hunting::apply(target_selector, &mob_arc.mob_entity, LLAMA_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn get_variant(&self) -> LlamaVariant {
        LlamaVariant::from_id(self.variant.load(Ordering::Relaxed))
    }

    pub fn set_variant(&self, variant: LlamaVariant) {
        self.variant.store(variant.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_VARIANT_ID,
            VarInt(variant.id()),
        );
    }

    #[must_use]
    pub fn get_strength(&self) -> i32 {
        self.strength.load(Ordering::Relaxed)
    }

    pub fn set_strength(&self, strength: i32) {
        let clamped = strength.clamp(1, 5);
        self.strength.store(clamped, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_STRENGTH_ID,
            VarInt(clamped),
        );
    }

    #[must_use]
    pub fn has_chest(&self) -> bool {
        self.has_chest.load(Ordering::Relaxed)
    }

    pub fn set_has_chest(&self, has_chest: bool) {
        self.has_chest.store(has_chest, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::llama::DATA_ID_CHEST, has_chest);
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
            pumpkin_data::tracked_data::llama::DATA_ID_FLAGS,
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

    pub fn spit(&self, target: &Arc<dyn EntityBase>) {
        let entity = self.get_entity();
        let world = entity.world.load_full();

        let spit_entity = Entity::new(world.clone(), entity.pos.load(), &EntityType::LLAMA_SPIT);
        let spit = LlamaSpitEntity::new_shot(spit_entity, entity);

        let mob_pos = entity.pos.load();
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();
        let target_height = f64::from(target_entity.entity_dimension.load().height);

        let dx = target_pos.x - mob_pos.x;
        let dy = (target_pos.y + target_height / 3.0) - spit.get_entity().pos.load().y;
        let dz = target_pos.z - mob_pos.z;
        let horizontal_distance = dx.hypot(dz);
        let yo = horizontal_distance * 0.2;

        spit.thrown.set_velocity(dx, dy + yo, dz, 1.5, 10.0);

        if !entity.silent.load(Ordering::Relaxed) {
            world.play_sound(Sound::EntityLlamaSpit, SoundCategory::Neutral, &mob_pos);
        }

        let spit_arc: Arc<dyn EntityBase> = Arc::new(spit);
        world.spawn_entity(spit_arc);
    }
}

impl AgeableMob for LlamaEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::ABSTRACT_HORSE_BABY_SPAWN_CHANCE
    }
}

impl Animal for LlamaEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_LLAMA_FOOD)
            || item_stack.item == &Item::WHEAT
            || item_stack.item == &Item::HAY_BLOCK
    }

    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby_base = from_type(
            &EntityType::LLAMA,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        if let Some(baby) = baby_base.cast_any().downcast_ref::<Self>() {
            let mut rng = rand::rng();
            // llama: 50% inherit variant from either parent (Llama.makeNewLlama)
            let other_llama = partner.cast_any().downcast_ref::<Self>();
            #[allow(clippy::option_if_let_else)]
            let chosen_variant = if let Some(p2) = other_llama {
                if rng.random_bool(0.5) {
                    self.get_variant()
                } else {
                    p2.get_variant()
                }
            } else {
                self.get_variant()
            };
            baby.set_variant(chosen_variant);

            // llama: strength genetics (Llama.makeNewLlama)
            let strength = if rng.random_range(0.0..1.0) < 0.04 {
                rng.random_range(1..=5)
            } else {
                let p1 = self.get_strength();
                let p2 = other_llama.map_or(p1, Self::get_strength);
                let max = p1.max(p2);
                rng.random_range(1..=max)
            };
            baby.set_strength(strength);
            baby.get_entity().age.store(-24000, Ordering::Relaxed);
        }
        baby_base
    }
}

impl CustomSound for LlamaEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaHurt)
    }
}

impl Mob for LlamaEntity {
    fn is_tamed(&self) -> bool {
        self.is_tame()
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityLlamaAmbient)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityLlamaStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn get_temper(&self) -> i32 {
        self.temper.load(Ordering::Relaxed)
    }

    fn max_temper(&self) -> i32 {
        100
    }

    fn modify_temper(&self, delta: i32) {
        let cur = self.temper.load(Ordering::Relaxed);
        self.temper
            .store((cur + delta).clamp(0, 100), Ordering::Relaxed);
    }

    fn tame_with_name(&self, player: &Player) -> bool {
        tame_horse(self, player, &self.flags, &self.owner)
    }

    fn make_mad(&self) {
        throw_rider_off(self, &self.flags, Sound::EntityLlamaAngry);
    }

    fn open_custom_inventory(&self, player: &Arc<Player>) {
        if !self.is_baby() && self.is_tamed() {
            let columns = if self.has_chest() {
                self.get_strength().clamp(1, 5) as usize
            } else {
                0
            };
            let handler = Arc::new(std::sync::Mutex::new(MountScreenHandler::new(
                columns as u8,
                &player.inventory,
                self.chest_inventory.clone(),
                self.saddle_inventory.clone(),
                self.armor_inventory.clone(),
                columns,
            )));
            player.open_mount_screen(handler, columns, self.get_entity().entity_id);
        }
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_super(group_data)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_int("Variant", self.get_variant().id());
        nbt.put_int("Strength", self.get_strength());
        nbt.put_bool("ChestedHorse", self.has_chest());
        nbt.put_bool("Tame", self.is_tame());
        nbt.put_int("Temper", self.temper.load(Ordering::Relaxed));
        nbt.put_bool("Bred", self.has_flag(FLAG_BRED));
        if let Some(owner) = self.owner.load() {
            nbt.put_uuid("Owner", owner);
        }
        if let Some(color) = self.carpet_color.load() {
            nbt.put_byte("DecorColor", color as i8);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(variant) = nbt.get_int("Variant") {
            self.set_variant(LlamaVariant::from_id(variant));
        }
        if let Some(strength) = nbt.get_int("Strength") {
            self.set_strength(strength);
        }
        if let Some(chested) = nbt.get_bool("ChestedHorse") {
            self.set_has_chest(chested);
        }
        if let Some(tame) = nbt.get_bool("Tame") {
            self.set_tame(tame);
        }
        if let Some(temper) = nbt.get_int("Temper") {
            self.temper.store(temper, Ordering::Relaxed);
        }
        if let Some(bred) = nbt.get_bool("Bred") {
            self.set_flag(FLAG_BRED, bred);
        }
        if let Some(owner) = nbt.get_uuid("Owner") {
            self.owner.store(Some(owner));
        }
        if let Some(color) = nbt.get_byte("DecorColor") {
            self.carpet_color.store(Some(color as u8));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::llama::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_VARIANT_ID,
            VarInt(self.get_variant().id()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_STRENGTH_ID,
            VarInt(self.get_strength()),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_ID_CHEST,
            self.has_chest(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::llama::DATA_ID_FLAGS,
            self.flags.load(Ordering::Relaxed) as i8,
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();

        // Shift-right-click to open mount screen
        if !self.is_baby() && self.is_tame() && player.living_entity.entity.is_sneaking() {
            self.open_custom_inventory(player);
            return true;
        }

        // Chest equipping
        if self.is_tame() && item == &Item::CHEST && !self.has_chest() && !self.is_baby() {
            self.set_has_chest(true);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityDonkeyChest,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            return true;
        }

        // Carpet equipping
        if self.is_tame()
            && !self.is_baby()
            && let Some(color) = get_carpet_color_from_item(item)
        {
            self.carpet_color.store(Some(color));
            self.armor_inventory.set_stack(0, ItemStack::new(1, item));
            self.mob_entity
                .living_entity
                .send_equipment_changes(&[(EquipmentSlot::BODY, ItemStack::new(1, item))]);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            world.play_sound(
                Sound::EntityLlamaSwag,
                SoundCategory::Neutral,
                &entity.pos.load(),
            );
            return true;
        }

        // Feeding
        if self.is_food(item_stack) {
            let (age_up, temper_inc, heal, can_love) = if item == &Item::WHEAT {
                (10, 3, 2.0f32, false)
            } else if item == &Item::HAY_BLOCK {
                (90, 6, 10.0f32, true)
            } else {
                (10, 3, 2.0f32, false)
            };

            let mut fed = false;
            let living = &self.mob_entity.living_entity;
            let cur_health = living.health.load();
            let max_health = living.get_max_health();
            if cur_health < max_health && heal > 0.0 {
                living.health.store((cur_health + heal).min(max_health));
                fed = true;
            }

            if self.is_baby() && age_up > 0 {
                self.age_up(age_up, true);
                fed = true;
            }

            if !self.is_tame() && temper_inc > 0 {
                self.modify_temper(temper_inc);
                fed = true;
            }

            if can_love && self.is_tame() && self.get_entity().age.load(Ordering::Relaxed) == 0 {
                self.mob_entity
                    .set_love_ticks(600, Some(player.gameprofile.id));
                fed = true;
            }

            if fed {
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                let entity = self.get_entity();
                let world = entity.world.load();
                world.play_sound(
                    Sound::EntityLlamaEat,
                    SoundCategory::Neutral,
                    &entity.pos.load(),
                );
                return true;
            }
        }

        // Riding
        if !self.is_baby() {
            let world = player.world();
            let ent = &self.mob_entity.living_entity.entity;
            if let Some(vehicle) = world.get_entity_by_id(ent.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                ent.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
                return true;
            }
        }

        false
    }
}

impl RangedAttackMob for LlamaEntity {
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, _power: f32) {
        self.spit(target);
    }
}
