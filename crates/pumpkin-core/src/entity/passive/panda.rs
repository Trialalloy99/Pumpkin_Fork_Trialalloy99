use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering::Relaxed},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;

use pumpkin_data::{
    Block, BlockState,
    entity::EntityType,
    item::Item,
    item_stack::ItemStack,
    particle::Particle,
    sound::{Sound, SoundCategory},
    tag::{self, Taggable},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        Controls, Goal,
        avoid_entity::{AvoidEntityGoal, not_spectator},
        breed::BreedGoal,
        escape_danger::EscapeDangerGoal,
        follow_parent::FollowParentGoal,
        gated::GatedGoal,
        look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal,
        revenge::RevengeGoal,
        swim::SwimGoal,
        tempt::TemptGoal,
        to_goal_ticks,
        wander_around::WanderAroundGoal,
    },
    ai::pathfinder::NavigatorGoal,
    ai::target_match::TargetMatch,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};

const TEMPT_ITEMS: &[&Item] = &[&Item::BAMBOO];

pub const FLAG_SNEEZE: u8 = 2;
pub const FLAG_ROLL: u8 = 4;
pub const FLAG_SIT: u8 = 8;
pub const FLAG_ON_BACK: u8 = 16;

// panda: gene definition and dominant/recessive rules (Panda.Gene)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PandaGene {
    #[default]
    Normal = 0,
    Lazy = 1,
    Worried = 2,
    Playful = 3,
    Brown = 4,
    Weak = 5,
    Aggressive = 6,
}

impl PandaGene {
    #[must_use]
    pub const fn from_id(id: u8) -> Self {
        match id {
            1 => Self::Lazy,
            2 => Self::Worried,
            3 => Self::Playful,
            4 => Self::Brown,
            5 => Self::Weak,
            6 => Self::Aggressive,
            _ => Self::Normal,
        }
    }

    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn is_recessive(self) -> bool {
        matches!(self, Self::Brown | Self::Weak)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Lazy => "lazy",
            Self::Worried => "worried",
            Self::Playful => "playful",
            Self::Brown => "brown",
            Self::Weak => "weak",
            Self::Aggressive => "aggressive",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "lazy" => Self::Lazy,
            "worried" => Self::Worried,
            "playful" => Self::Playful,
            "brown" => Self::Brown,
            "weak" => Self::Weak,
            "aggressive" => Self::Aggressive,
            _ => Self::Normal,
        }
    }

    // panda: gene expression from main and hidden alleles (Panda.Gene.getVariantFromGenes)
    #[must_use]
    pub fn get_variant(main_gene: Self, hidden_gene: Self) -> Self {
        if main_gene.is_recessive() {
            if main_gene == hidden_gene {
                main_gene
            } else {
                Self::Normal
            }
        } else {
            main_gene
        }
    }

    // panda: random gene roll on natural spawn (Panda.Gene.getRandom)
    #[must_use]
    pub fn random_gene() -> Self {
        let mut rng = rand::rng();
        let roll = rng.random_range(0..16);
        match roll {
            0 => Self::Lazy,
            1 => Self::Worried,
            2 => Self::Playful,
            4 => Self::Aggressive,
            5..=8 => Self::Weak,
            9..=10 => Self::Brown,
            _ => Self::Normal,
        }
    }
}

// panda: panda mob structure and state (Panda)
pub struct PandaEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub main_gene: AtomicU8,
    pub hidden_gene: AtomicU8,
    pub flags: AtomicU8,
    pub eat_counter: AtomicI32,
    pub sneeze_counter: AtomicI32,
    pub unhappy_counter: AtomicI32,
    pub roll_counter: AtomicI32,
    // panda: Panda.gotBamboo / didBite / rollDelta
    pub got_bamboo: AtomicBool,
    pub did_bite: AtomicBool,
    roll_delta: AtomicCell<Vector3<f64>>,
    // panda: PandaLookAtPlayerGoal.setTarget from the breed goal
    look_player: Mutex<Option<Weak<Player>>>,
}

impl PandaEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Box::new(PandaMoveControl::default());
        mob_entity.set_can_pick_up_loot(true);
        let gene = PandaGene::random_gene();
        let hidden = PandaGene::random_gene();
        let panda = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            main_gene: AtomicU8::new(gene.id()),
            hidden_gene: AtomicU8::new(hidden.id()),
            flags: AtomicU8::new(0),
            eat_counter: AtomicI32::new(0),
            sneeze_counter: AtomicI32::new(0),
            unhappy_counter: AtomicI32::new(0),
            roll_counter: AtomicI32::new(0),
            got_bamboo: AtomicBool::new(false),
            did_bite: AtomicBool::new(false),
            roll_delta: AtomicCell::new(Vector3::new(0.0, 0.0, 0.0)),
            look_player: Mutex::new(None),
        };
        let mob_arc = Arc::new(panda);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            // panda: Panda.PandaPanicGoal, stops while sitting
            goal_selector.add_goal(2, EscapeDangerGoal::new(2.0).gated_by(not_sitting));
            goal_selector.add_goal(2, Box::new(PandaBreedGoal::new(1.0)));
            // panda: Panda.PandaAttackGoal
            goal_selector.add_goal(
                3,
                Box::new(MeleeAttackGoal::new(1.2, true).gated_by(can_perform_action)),
            );
            goal_selector.add_goal(4, Box::new(TemptGoal::new(1.0, TEMPT_ITEMS, false)));
            // panda: Panda.PandaAvoidGoal, worried pandas avoid players and monsters
            goal_selector.add_goal(
                6,
                Box::new(
                    AvoidEntityGoal::new(&EntityType::PLAYER, 8.0, 2.0, 2.0)
                        .when(worried_and_can_act)
                        .matching(not_spectator),
                ),
            );
            goal_selector.add_goal(
                6,
                Box::new(
                    AvoidEntityGoal::new(TargetMatch::Monster, 4.0, 2.0, 2.0)
                        .when(worried_and_can_act)
                        .matching(not_spectator),
                ),
            );
            goal_selector.add_goal(7, Box::new(PandaSitGoal::default()));
            goal_selector.add_goal(8, Box::new(PandaLieOnBackGoal::default()));
            goal_selector.add_goal(8, Box::new(PandaSneezeGoal));
            // panda: Panda.PandaLookAtPlayerGoal
            goal_selector.add_goal(
                9,
                Box::new(GatedGoal::new(
                    PandaLookAtPlayerGoal {
                        inner: LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
                    },
                    can_perform_action,
                )),
            );
            goal_selector.add_goal(10, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(12, Box::new(PandaRollGoal));
            goal_selector.add_goal(13, Box::new(FollowParentGoal::new(1.25)));
            goal_selector.add_goal(14, Box::new(WanderAroundGoal::new(1.0)));
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;
            // panda: Panda.PandaHurtByTargetGoal, alerts aggressive pandas, drops after bamboo or a bite
            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .alerting_only(is_aggressive_panda)
                        .continuing_while(still_angry),
                ),
            );
        };
        mob_arc.set_attributes();

        mob_arc
    }

    #[must_use]
    pub fn get_main_gene(&self) -> PandaGene {
        PandaGene::from_id(self.main_gene.load(Relaxed))
    }

    pub fn set_main_gene(&self, gene: PandaGene) {
        self.main_gene.store(gene.id(), Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::MAIN_GENE_ID,
            gene.id() as i8,
        );
    }

    #[must_use]
    pub fn get_hidden_gene(&self) -> PandaGene {
        PandaGene::from_id(self.hidden_gene.load(Relaxed))
    }

    pub fn set_hidden_gene(&self, gene: PandaGene) {
        self.hidden_gene.store(gene.id(), Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::HIDDEN_GENE_ID,
            gene.id() as i8,
        );
    }

    #[must_use]
    pub fn get_variant(&self) -> PandaGene {
        PandaGene::get_variant(self.get_main_gene(), self.get_hidden_gene())
    }

    #[must_use]
    pub fn is_lazy(&self) -> bool {
        self.get_variant() == PandaGene::Lazy
    }

    #[must_use]
    pub fn is_worried(&self) -> bool {
        self.get_variant() == PandaGene::Worried
    }

    #[must_use]
    pub fn is_playful(&self) -> bool {
        self.get_variant() == PandaGene::Playful
    }

    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.get_variant() == PandaGene::Weak
    }

    #[must_use]
    pub fn is_aggressive(&self) -> bool {
        self.get_variant() == PandaGene::Aggressive
    }

    #[must_use]
    pub fn is_brown(&self) -> bool {
        self.get_variant() == PandaGene::Brown
    }

    #[must_use]
    pub fn has_flag(&self, flag: u8) -> bool {
        (self.flags.load(Relaxed) & flag) != 0
    }

    pub fn set_flag(&self, flag: u8, val: bool) {
        let current = self.flags.load(Relaxed);
        let new_flags = if val { current | flag } else { current & !flag };
        self.flags.store(new_flags, Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::DATA_ID_FLAGS,
            new_flags as i8,
        );
    }

    #[must_use]
    pub fn is_sneezing(&self) -> bool {
        self.has_flag(FLAG_SNEEZE)
    }

    pub fn set_sneezing(&self, val: bool) {
        self.set_flag(FLAG_SNEEZE, val);
        if !val {
            self.sneeze_counter.store(0, Relaxed);
            self.get_entity()
                .set_synced_data(pumpkin_data::tracked_data::panda::SNEEZE_COUNTER, VarInt(0));
        }
    }

    #[must_use]
    pub fn is_sitting(&self) -> bool {
        self.has_flag(FLAG_SIT)
    }

    pub fn set_sitting(&self, val: bool) {
        self.set_flag(FLAG_SIT, val);
    }

    #[must_use]
    pub fn is_on_back(&self) -> bool {
        self.has_flag(FLAG_ON_BACK)
    }

    pub fn set_on_back(&self, val: bool) {
        self.set_flag(FLAG_ON_BACK, val);
    }

    #[must_use]
    pub fn is_rolling(&self) -> bool {
        self.has_flag(FLAG_ROLL)
    }

    pub fn set_rolling(&self, val: bool) {
        self.set_flag(FLAG_ROLL, val);
    }

    pub fn set_unhappy_counter(&self, counter: i32) {
        self.unhappy_counter.store(counter, Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::panda::UNHAPPY_COUNTER,
            VarInt(counter),
        );
    }

    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.get_entity().age.load(Relaxed) < 0
    }

    // panda: Panda.isScared, worried pandas fear thunder
    #[must_use]
    pub fn is_scared(&self) -> bool {
        self.is_worried() && self.get_entity().world.load().is_thundering()
    }

    #[must_use]
    pub fn is_eating(&self) -> bool {
        self.eat_counter.load(Relaxed) > 0
    }

    pub fn eat(&self, value: bool) {
        self.set_eat_counter(i32::from(value));
    }

    fn set_eat_counter(&self, value: i32) {
        self.eat_counter.store(value, Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::panda::EAT_COUNTER,
            VarInt(value),
        );
    }

    // panda: Panda.canPerformAction
    #[must_use]
    pub fn can_perform_action(&self) -> bool {
        !self.is_on_back()
            && !self.is_scared()
            && !self.is_eating()
            && !self.is_rolling()
            && !self.is_sitting()
    }

    #[must_use]
    pub fn held_item(&self) -> ItemStack {
        self.mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&EquipmentSlot::MAIN_HAND)
    }

    pub fn set_held_item(&self, stack: ItemStack) {
        self.mob_entity
            .set_item_slot(&EquipmentSlot::MAIN_HAND, stack);
        self.mob_entity
            .set_drop_chance(&EquipmentSlot::MAIN_HAND, 2.0);
    }

    fn drop_stack(&self, stack: ItemStack) {
        let entity = self.get_entity();
        let world = entity.world.load();
        world.spawn_entity(Arc::new(crate::entity::item::ItemEntity::new(
            Entity::new(world.clone(), entity.pos.load(), &EntityType::ITEM),
            stack,
        )));
    }

    // panda: Panda.tryToSit
    fn try_to_sit(&self) {
        if !self.get_entity().touching_water.load(Relaxed) {
            self.mob_entity.steered().navigator().stop();
            self.set_sitting(true);
        }
    }

    // panda: Panda.setAttributes, weak pandas have 10 health, lazy ones walk slower
    pub fn set_attributes(&self) {
        let living = &self.mob_entity.living_entity;
        if self.is_weak() {
            living.set_attribute_base(&Attributes::MAX_HEALTH, 10.0);
        }
        if self.is_lazy() {
            living.set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.07);
        }
    }

    // panda: Panda.handleEating, sitting pandas eat what they hold
    fn handle_eating(&self) {
        let held = self.held_item();
        let mut rng = self.get_random();
        if !self.is_eating() && self.is_sitting() && !self.is_scared() && !held.is_empty() {
            if rng.random_range(0..80) == 1 {
                self.eat(true);
            }
        } else if held.is_empty() || !self.is_sitting() {
            self.eat(false);
        }
        if !self.is_eating() {
            return;
        }
        let counter = self.eat_counter.load(Relaxed);
        if counter % 5 == 0 {
            let volume = 0.5 + 0.5 * rng.random_range(0..2) as f32;
            let pitch = (rng.random::<f32>() - rng.random::<f32>()) * 0.2 + 1.0;
            self.play_entity_sound(Sound::EntityPandaEat, volume, pitch);
        }
        if counter > 80 && rng.random_range(0..20) == 1 {
            if counter > 100
                && held
                    .item
                    .has_tag(&tag::Item::MINECRAFT_PANDA_EATS_FROM_GROUND)
            {
                self.set_held_item(ItemStack::EMPTY.clone());
                self.set_sitting(false);
            }
            self.eat(false);
            return;
        }
        self.set_eat_counter(counter + 1);
    }

    // panda: Panda.handleRoll, three hops over 32 ticks
    fn handle_roll(&self) {
        let rc = self.roll_counter.fetch_add(1, Relaxed) + 1;
        if rc > 32 {
            self.set_rolling(false);
            return;
        }
        let entity = self.get_entity();
        let movement = entity.velocity.load();
        if rc == 1 {
            let angle = entity.yaw.load().to_radians();
            let multiplier = if self.is_baby() { 0.1 } else { 0.2 };
            let delta = Vector3::new(
                movement.x + f64::from(-angle.sin() * multiplier),
                0.0,
                movement.z + f64::from(angle.cos() * multiplier),
            );
            self.roll_delta.store(delta);
            entity.velocity.store(Vector3::new(delta.x, 0.27, delta.z));
        } else if rc != 7 && rc != 15 && rc != 23 {
            let delta = self.roll_delta.load();
            entity
                .velocity
                .store(Vector3::new(delta.x, movement.y, delta.z));
        } else {
            let y = if entity.on_ground.load(Relaxed) {
                0.27
            } else {
                movement.y
            };
            entity.velocity.store(Vector3::new(0.0, y, 0.0));
        }
    }

    // panda: inherit genes from parents (Panda.setGeneFromParents)
    pub fn set_genes_from_parents(&self, parent1: &Self, parent2: &Self) {
        let mut rng = self.get_random();
        let g1 = if rng.random_bool(0.5) {
            parent1.get_main_gene()
        } else {
            parent1.get_hidden_gene()
        };
        let g2 = if rng.random_bool(0.5) {
            parent2.get_main_gene()
        } else {
            parent2.get_hidden_gene()
        };
        let (mut main, mut hidden) = if rng.random_bool(0.5) {
            (g1, g2)
        } else {
            (g2, g1)
        };
        if rng.random_range(0..32) == 0 {
            main = PandaGene::random_gene();
        }
        if rng.random_range(0..32) == 0 {
            hidden = PandaGene::random_gene();
        }
        self.set_main_gene(main);
        self.set_hidden_gene(hidden);
    }

    // panda: sneeze completion and slimeball drop chance (Panda.afterSneeze)
    fn after_sneeze(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        world.spawn_particle(
            pos + Vector3::new(0.0, f64::from(entity.height()) * 0.5, 0.0),
            Vector3::new(0.5, 0.5, 0.5),
            1.0,
            10,
            Particle::Sneeze,
        );
        self.play_entity_sound(Sound::EntityPandaSneeze, 1.0, 1.0);
        // panda: startled adult pandas within 10 blocks jump (Panda.afterSneeze)
        let area = entity.bounding_box.load().expand(10.0, 10.0, 10.0);
        for other in world.get_entities_at_box(&area) {
            if let Some(other) = other.cast_any().downcast_ref::<Self>()
                && !other.is_baby()
                && other.get_entity().on_ground.load(Relaxed)
                && !other.get_entity().touching_water.load(Relaxed)
                && other.can_perform_action()
            {
                other.mob_entity.living_entity.jump();
            }
        }

        // 1/700 chance to drop a slimeball (Panda.afterSneeze, BuiltInLootTables.PANDA_SNEEZE)
        if self.get_random().random_range(0..700) == 0 {
            let item_entity = Arc::new(crate::entity::item::ItemEntity::new(
                Entity::new(
                    world.clone(),
                    pos + Vector3::new(0.0, 0.5, 0.0),
                    &EntityType::ITEM,
                ),
                ItemStack::new(1, &Item::SLIME_BALL),
            ));
            world.spawn_entity(item_entity);
        }
    }
}

impl AgeableMob for PandaEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::PANDA_BABY_SPAWN_CHANCE
    }
}

impl Animal for PandaEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_PANDA_FOOD)
    }

    // panda: Panda.getBreedOffspring, genes from both parents
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let baby = Self::new(Entity::new(
            entity.world.load_full(),
            entity.pos.load(),
            &EntityType::PANDA,
        ));
        if let Some(partner) = partner.cast_any().downcast_ref::<Self>() {
            baby.set_genes_from_parents(self, partner);
        }
        baby.set_attributes();
        baby
    }
}

impl CustomSound for PandaEntity {
    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPandaHurt)
    }

    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPandaDeath)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Neutral
    }
}

impl Mob for PandaEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityPandaStep, 0.15, 1.0);
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
        if self.is_aggressive() {
            Some(Sound::EntityPandaAggressiveAmbient)
        } else if self.is_worried() {
            Some(Sound::EntityPandaWorriedAmbient)
        } else {
            Some(Sound::EntityPandaAmbient)
        }
    }

    // panda: Panda.tick after LivingEntity.tick
    fn post_tick(&self) {
        let entity = self.get_entity();
        if !entity.is_alive() {
            return;
        }
        if self.is_worried() {
            if entity.world.load().is_thundering() && !entity.touching_water.load(Relaxed) {
                self.set_sitting(true);
                self.eat(false);
            } else if !self.is_eating() {
                self.set_sitting(false);
            }
        }

        let target = self.mob_entity.get_target();
        if target.is_none() {
            self.got_bamboo.store(false, Relaxed);
            self.did_bite.store(false, Relaxed);
        }

        let unhappy = self.unhappy_counter.load(Relaxed);
        if unhappy > 0 {
            if let Some(target) = &target {
                self.mob_entity
                    .look_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .look_at_entity_with_range(target, 90.0, 90.0);
            }
            if unhappy == 29 || unhappy == 14 {
                self.play_entity_sound(Sound::EntityPandaCantBreed, 1.0, 1.0);
            }
            self.set_unhappy_counter(unhappy - 1);
        }

        if self.is_sneezing() {
            let sc = self.sneeze_counter.fetch_add(1, Relaxed) + 1;
            entity.set_synced_data(
                pumpkin_data::tracked_data::panda::SNEEZE_COUNTER,
                VarInt(sc),
            );
            if sc > 20 {
                self.set_sneezing(false);
                self.after_sneeze();
            } else if sc == 1 {
                self.play_entity_sound(Sound::EntityPandaPreSneeze, 1.0, 1.0);
            }
        }

        if self.is_rolling() {
            self.handle_roll();
        } else {
            self.roll_counter.store(0, Relaxed);
        }

        if self.is_sitting() {
            entity.pitch.store(0.0);
        }
        self.handle_eating();
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_string("MainGene", self.get_main_gene().as_str().to_string());
        nbt.put_string("HiddenGene", self.get_hidden_gene().as_str().to_string());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(main) = nbt.get_string("MainGene") {
            self.set_main_gene(PandaGene::from_name(main));
        }
        if let Some(hidden) = nbt.get_string("HiddenGene") {
            self.set_hidden_gene(PandaGene::from_name(hidden));
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::panda::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::MAIN_GENE_ID,
            self.main_gene.load(Relaxed) as i8,
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::HIDDEN_GENE_ID,
            self.hidden_gene.load(Relaxed) as i8,
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::DATA_ID_FLAGS,
            self.flags.load(Relaxed) as i8,
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::UNHAPPY_COUNTER,
            VarInt(self.unhappy_counter.load(Relaxed)),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::SNEEZE_COUNTER,
            VarInt(self.sneeze_counter.load(Relaxed)),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::panda::EAT_COUNTER,
            VarInt(self.eat_counter.load(Relaxed)),
        );
    }

    // panda: Panda.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if self.is_scared() {
            return false;
        }
        if self.is_on_back() {
            self.set_on_back(false);
            return true;
        }
        if !self.is_food(item_stack) {
            return self.is_baby()
                && item_stack.item == &Item::GOLDEN_DANDELION
                && self.animal_interact(player, item_stack, Sound::EntityPandaAmbient);
        }
        if self.mob_entity.get_target().is_some() {
            self.got_bamboo.store(true, Relaxed);
        }
        let age = self.get_entity().age.load(Relaxed);
        let gamemode = player.gamemode.load();
        if age < 0 {
            item_stack.decrement_unless_creative(gamemode, 1);
            self.age_up(((-age / 20) as f32 * 0.1) as i32, true);
            return true;
        }
        if age == 0 && self.can_fall_in_love() && !self.mob_entity.is_in_love() {
            return self.animal_interact(player, item_stack, Sound::EntityPandaAmbient);
        }
        let entity = self.get_entity();
        if self.is_sitting() || entity.touching_water.load(Relaxed) {
            return false;
        }
        self.try_to_sit();
        self.eat(true);
        let current = self.held_item();
        if !current.is_empty() && !player.is_creative() {
            self.drop_stack(current);
        }
        self.set_held_item(ItemStack::new(1, item_stack.item));
        item_stack.decrement_unless_creative(gamemode, 1);
        true
    }

    // panda: Panda.pickUpItem, only food from the ground into an empty hand
    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        self.held_item().is_empty()
            && stack
                .item
                .has_tag(&tag::Item::MINECRAFT_PANDA_EATS_FROM_GROUND)
    }

    fn mob_pick_up_item(&self, item: &crate::entity::item::ItemEntity, stack: &ItemStack) -> bool {
        if self.held_item().is_empty()
            && stack
                .item
                .has_tag(&tag::Item::MINECRAFT_PANDA_EATS_FROM_GROUND)
        {
            self.set_held_item(stack.clone());
            item.get_entity().remove();
        }
        true
    }

    // panda: Panda.hurtServer stands up
    fn on_damage(
        &self,
        _damage_type: pumpkin_data::damage::DamageType,
        _source: Option<&dyn EntityBase>,
    ) {
        self.set_sitting(false);
    }

    // panda: Panda.doHurtTarget, a non-aggressive panda lets go after one bite
    fn pre_attack(&self, _target: &dyn EntityBase) {
        if !self.is_aggressive() {
            self.did_bite.store(true, Relaxed);
        }
        self.play_entity_sound(Sound::EntityPandaBite, 1.0, 1.0);
    }
}

fn as_panda(mob: &dyn Mob) -> Option<&PandaEntity> {
    mob.cast_any().downcast_ref::<PandaEntity>()
}

fn not_sitting(mob: &dyn Mob) -> bool {
    !as_panda(mob).is_some_and(PandaEntity::is_sitting)
}

fn is_aggressive_panda(mob: &dyn Mob) -> bool {
    as_panda(mob).is_some_and(PandaEntity::is_aggressive)
}

fn still_angry(mob: &dyn Mob) -> bool {
    as_panda(mob).is_some_and(|p| !p.got_bamboo.load(Relaxed) && !p.did_bite.load(Relaxed))
}

fn can_perform_action(mob: &dyn Mob) -> bool {
    as_panda(mob).is_some_and(PandaEntity::can_perform_action)
}

fn worried_and_can_act(mob: &dyn Mob) -> bool {
    as_panda(mob).is_some_and(|p| p.is_worried() && p.can_perform_action())
}

// panda: Panda.canPickUpAndEat
fn can_pick_up_and_eat(item: &crate::entity::item::ItemEntity) -> bool {
    item.get_entity().is_alive()
        && item.get_pickup_delay() == 0
        && item
            .get_item_stack()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .item
            .has_tag(&tag::Item::MINECRAFT_PANDA_EATS_FROM_GROUND)
}

fn nearest_edible_item(panda: &PandaEntity, range: f64) -> Option<Arc<dyn EntityBase>> {
    let entity = panda.get_entity();
    let area = entity.bounding_box.load().expand(range, range, range);
    entity
        .world
        .load()
        .get_entities_at_box(&area)
        .into_iter()
        .find(|candidate| {
            candidate
                .cast_any()
                .downcast_ref::<crate::entity::item::ItemEntity>()
                .is_some_and(can_pick_up_and_eat)
        })
}

// panda: Panda.PandaMoveControl, frozen while sitting, eating, rolling or on its back
#[derive(Default)]
struct PandaMoveControl {
    inner: crate::entity::ai::control::move_control::MoveControl,
}

impl crate::entity::ai::control::Control for PandaMoveControl {}

impl crate::entity::ai::control::MoveControlTrait for PandaMoveControl {
    fn tick(&mut self, mob: &dyn Mob) {
        if can_perform_action(mob) {
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

// panda: Panda.PandaBreedGoal, needs bamboo within 8 blocks, otherwise shakes its head
struct PandaBreedGoal {
    inner: Box<BreedGoal>,
    unhappy_cooldown: i32,
}

impl PandaBreedGoal {
    fn new(speed: f64) -> Self {
        Self {
            inner: BreedGoal::new(speed),
            unhappy_cooldown: 0,
        }
    }

    // panda: PandaBreedGoal.canFindBamboo, spiral search, 3 layers up
    fn can_find_bamboo(panda: &PandaEntity) -> bool {
        let entity = panda.get_entity();
        let pos = entity.block_pos.load();
        let world = entity.world.load();
        for y_off in 0..3 {
            for r in 0..8 {
                let mut x = 0;
                while x <= r {
                    let mut z = if x < r && x > -r { r } else { 0 };
                    while z <= r {
                        let check = BlockPos::new(pos.0.x + x, pos.0.y + y_off, pos.0.z + z);
                        if world.get_block(&check) == &Block::BAMBOO {
                            return true;
                        }
                        z = if z > 0 { -z } else { 1 - z };
                    }
                    x = if x > 0 { -x } else { 1 - x };
                }
            }
        }
        false
    }
}

impl Goal for PandaBreedGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        if !self.inner.can_start(mob) || panda.unhappy_counter.load(Relaxed) != 0 {
            return false;
        }
        if Self::can_find_bamboo(panda) {
            return true;
        }
        let tick_count = panda.get_entity().tick_count.load(Relaxed);
        if self.unhappy_cooldown <= tick_count {
            panda.set_unhappy_counter(32);
            self.unhappy_cooldown = tick_count + 600;
            let entity = panda.get_entity();
            let player = entity
                .world
                .load()
                .get_closest_player(entity.pos.load(), 8.0);
            *panda
                .look_player
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                player.map(|p| Arc::downgrade(&p));
        }
        false
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
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

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// panda: Panda.PandaLookAtPlayerGoal, the breed goal can force the target
struct PandaLookAtPlayerGoal {
    inner: Box<LookAtEntityGoal>,
}

impl PandaLookAtPlayerGoal {
    fn forced(mob: &dyn Mob) -> Option<Arc<Player>> {
        let panda = as_panda(mob)?;
        let mut slot = panda
            .look_player
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let player = slot.as_ref().and_then(Weak::upgrade);
        if player.is_none() {
            *slot = None;
        }
        player
    }
}

impl Goal for PandaLookAtPlayerGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        Self::forced(mob).is_some() || self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        Self::forced(mob).is_some() || self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob) {
            *panda
                .look_player
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        if let Some(player) = Self::forced(mob) {
            let target: Arc<dyn EntityBase> = player;
            mob.get_mob_entity()
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at_entity(mob, &target);
        } else {
            self.inner.tick(mob);
        }
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

// panda: Panda.PandaSneezeGoal, babies sneeze, weak ones more often
struct PandaSneezeGoal;

impl Goal for PandaSneezeGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        if !panda.is_baby() || !panda.can_perform_action() {
            return false;
        }
        let mut rng = panda.get_random();
        (panda.is_weak() && rng.random_range(0..to_goal_ticks(500)) == 1)
            || rng.random_range(0..to_goal_ticks(6000)) == 1
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob) {
            panda.set_sneezing(true);
        }
    }
}

// panda: Panda.PandaSitGoal, walks to food on the ground, sits and eats it
#[derive(Default)]
struct PandaSitGoal {
    cooldown: i32,
}

impl Goal for PandaSitGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        let entity = panda.get_entity();
        if self.cooldown > entity.tick_count.load(Relaxed)
            || panda.is_baby()
            || entity.touching_water.load(Relaxed)
            || !panda.can_perform_action()
            || panda.unhappy_counter.load(Relaxed) > 0
        {
            return false;
        }
        !panda.held_item().is_empty() || nearest_edible_item(panda, 6.0).is_some()
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        let mut rng = panda.get_random();
        !panda.get_entity().touching_water.load(Relaxed)
            && (panda.is_lazy() || rng.random_range(0..to_goal_ticks(600)) != 1)
            && rng.random_range(0..to_goal_ticks(2000)) != 1
    }

    fn tick(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob)
            && !panda.is_sitting()
            && !panda.held_item().is_empty()
        {
            panda.try_to_sit();
        }
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(panda) = as_panda(mob) else {
            return;
        };
        if panda.held_item().is_empty() {
            if let Some(item) = nearest_edible_item(panda, 8.0) {
                let mob_pos = panda.get_entity().pos.load();
                let item_pos = item.get_entity().pos.load();
                panda
                    .mob_entity
                    .steered()
                    .navigator()
                    .set_progress(NavigatorGoal::new(mob_pos, item_pos, 1.2));
            }
        } else {
            panda.try_to_sit();
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let Some(panda) = as_panda(mob) else {
            return;
        };
        let held = panda.held_item();
        if !held.is_empty() {
            panda.drop_stack(held);
            panda.set_held_item(ItemStack::EMPTY.clone());
            let mut rng = panda.get_random();
            let wait_seconds = if panda.is_lazy() {
                rng.random_range(0..50) + 10
            } else {
                rng.random_range(0..150) + 10
            };
            self.cooldown = panda.get_entity().tick_count.load(Relaxed) + wait_seconds * 20;
        }
        panda.set_sitting(false);
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// panda: Panda.PandaLieOnBackGoal, lazy pandas roll onto their back
#[derive(Default)]
struct PandaLieOnBackGoal {
    cooldown: i32,
}

impl Goal for PandaLieOnBackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        self.cooldown < panda.get_entity().tick_count.load(Relaxed)
            && panda.is_lazy()
            && panda.can_perform_action()
            && panda.get_random().random_range(0..to_goal_ticks(400)) == 1
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        let mut rng = panda.get_random();
        !panda.get_entity().touching_water.load(Relaxed)
            && (panda.is_lazy() || rng.random_range(0..to_goal_ticks(600)) != 1)
            && rng.random_range(0..to_goal_ticks(2000)) != 1
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob) {
            panda.set_on_back(true);
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob) {
            panda.set_on_back(false);
            self.cooldown = panda.get_entity().tick_count.load(Relaxed) + 200;
        }
    }
}

// panda: Panda.PandaRollGoal, babies and playful pandas roll, also off ledges
struct PandaRollGoal;

impl Goal for PandaRollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = as_panda(mob) else {
            return false;
        };
        let entity = panda.get_entity();
        if !(panda.is_baby() || panda.is_playful())
            || !entity.on_ground.load(Relaxed)
            || !panda.can_perform_action()
        {
            return false;
        }
        let angle = entity.yaw.load().to_radians();
        let x_dir = -angle.sin();
        let z_dir = angle.cos();
        let x_step = if x_dir.abs() > 0.5 {
            x_dir.signum() as i32
        } else {
            0
        };
        let z_step = if z_dir.abs() > 0.5 {
            z_dir.signum() as i32
        } else {
            0
        };
        let pos = entity.block_pos.load();
        let below = BlockPos::new(pos.0.x + x_step, pos.0.y - 1, pos.0.z + z_step);
        if entity.world.load().get_block_state(&below).is_air() {
            return true;
        }
        let mut rng = panda.get_random();
        (panda.is_playful() && rng.random_range(0..to_goal_ticks(60)) == 1)
            || rng.random_range(0..to_goal_ticks(500)) == 1
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = as_panda(mob) {
            panda.set_rolling(true);
        }
    }

    fn can_stop(&self) -> bool {
        false
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK | Controls::JUMP
    }
}
