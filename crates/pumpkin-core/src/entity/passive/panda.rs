use std::sync::{
    Arc, Weak,
    atomic::{AtomicI32, AtomicU8, Ordering::Relaxed},
};

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
        Controls, Goal, breed::BreedGoal, escape_danger::EscapeDangerGoal,
        follow_parent::FollowParentGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, revenge::RevengeGoal,
        swim::SwimGoal, tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
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
}

impl PandaEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
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
        };
        let mob_arc = Arc::new(panda);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(2, EscapeDangerGoal::new(2.0));
            // panda: breeding requires nearby bamboo blocks (Panda.PandaBreedGoal)
            goal_selector.add_goal(2, Box::new(PandaBreedGoal::new(1.0)));
            // panda: aggressive panda attack goal (Panda.PandaAttackGoal)
            goal_selector.add_goal(3, Box::new(MeleeAttackGoal::new(1.2, true)));
            goal_selector.add_goal(4, Box::new(TemptGoal::new(1.0, TEMPT_ITEMS, false)));
            goal_selector.add_goal(5, Box::new(FollowParentGoal::new(1.25)));
            // panda: sit, lie on back, sneeze, roll goals (Panda)
            goal_selector.add_goal(7, Box::new(PandaSitGoal));
            goal_selector.add_goal(8, Box::new(PandaLieOnBackGoal));
            goal_selector.add_goal(8, Box::new(PandaSneezeGoal));
            goal_selector.add_goal(
                9,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(10, Box::new(RandomLookAroundGoal::default()));
            goal_selector.add_goal(11, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(12, Box::new(PandaRollGoal));
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;
            // panda: aggressive retaliation when hurt (Panda.PandaHurtByTargetGoal)
            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).alerting_others()));
        };

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
            || item_stack.item == &Item::BAMBOO
            || item_stack.item == &Item::CAKE
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

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        if !entity.is_alive() || self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }

        // Unhappy counter decay
        let unhappy = self.unhappy_counter.load(Relaxed);
        if unhappy > 0 {
            self.unhappy_counter.store(unhappy - 1, Relaxed);
            if unhappy == 29 || unhappy == 14 {
                self.play_entity_sound(Sound::EntityPandaCantBreed, 1.0, 1.0);
            }
        }

        // Sneeze sequence (Panda.tick)
        if self.is_sneezing() {
            let sc = self.sneeze_counter.fetch_add(1, Relaxed) + 1;
            entity.set_synced_data(
                pumpkin_data::tracked_data::panda::SNEEZE_COUNTER,
                VarInt(sc),
            );
            if sc == 1 {
                self.play_entity_sound(Sound::EntityPandaPreSneeze, 1.0, 1.0);
            } else if sc > 20 {
                self.set_sneezing(false);
                self.after_sneeze();
            }
        }

        // Roll handling
        if self.is_rolling() {
            let rc = self.roll_counter.fetch_add(1, Relaxed) + 1;
            if rc > 32 {
                self.set_rolling(false);
                self.roll_counter.store(0, Relaxed);
            }
        }
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

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityPandaAmbient)
    }
}

// panda: breeding requires at least 8 bamboo blocks within 5 blocks radius (Panda.PandaBreedGoal)
struct PandaBreedGoal {
    inner: Box<BreedGoal>,
}

impl PandaBreedGoal {
    fn new(speed: f64) -> Self {
        Self {
            inner: BreedGoal::new(speed),
        }
    }

    fn has_nearby_bamboo(panda: &PandaEntity) -> bool {
        let entity = panda.get_entity();
        let pos = entity.block_pos.load();
        let world = entity.world.load();
        let mut count = 0;
        for dy in -2..=2 {
            for dx in -5..=5 {
                for dz in -5..=5 {
                    let check_pos = BlockPos::new(pos.0.x + dx, pos.0.y + dy, pos.0.z + dz);
                    let block = world.get_block(&check_pos);
                    if block.id == Block::BAMBOO.id {
                        count += 1;
                        if count >= 8 {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

impl Goal for PandaBreedGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() else {
            return false;
        };
        if panda.unhappy_counter.load(Relaxed) > 0 {
            return false;
        }
        if !self.inner.can_start(mob) {
            return false;
        }
        if !Self::has_nearby_bamboo(panda) {
            panda.set_unhappy_counter(32);
            return false;
        }
        true
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

// panda: baby sneeze goal (Panda.PandaSneezeGoal)
struct PandaSneezeGoal;

impl Goal for PandaSneezeGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() else {
            return false;
        };
        let entity = panda.get_entity();
        let is_baby = entity.age.load(Relaxed) < 0;
        if !is_baby || panda.is_sneezing() {
            return false;
        }
        let chance = if panda.is_weak() { 100 } else { 500 };
        panda.get_random().random_range(0..chance) == 0
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() {
            panda.set_sneezing(true);
        }
    }
}

// panda: sitting goal (Panda.PandaSitGoal)
struct PandaSitGoal;

impl Goal for PandaSitGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() else {
            return false;
        };
        if panda.is_sitting() || panda.is_rolling() || panda.is_on_back() {
            return false;
        }
        let chance = if panda.is_lazy() { 100 } else { 400 };
        panda.get_random().random_range(0..chance) == 0
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() {
            panda.set_sitting(true);
        }
    }
}

// panda: lying on back goal (Panda.PandaLieOnBackGoal)
struct PandaLieOnBackGoal;

impl Goal for PandaLieOnBackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() else {
            return false;
        };
        if !panda.is_lazy() || panda.is_on_back() || panda.is_rolling() {
            return false;
        }
        panda.get_random().random_range(0..400) == 0
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() {
            panda.set_on_back(true);
        }
    }
}

// panda: playful roll goal (Panda.PandaRollGoal)
struct PandaRollGoal;

impl Goal for PandaRollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() else {
            return false;
        };
        if !panda.is_playful() || panda.is_rolling() {
            return false;
        }
        panda.get_random().random_range(0..200) == 0
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(panda) = mob.cast_any().downcast_ref::<PandaEntity>() {
            panda.set_rolling(true);
            panda.roll_counter.store(0, Relaxed);
        }
    }
}
