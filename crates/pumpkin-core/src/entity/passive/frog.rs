use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::{optional_int::OptionalInt, var_int::VarInt};
use pumpkin_util::math::position::BlockPos;
use uuid::Uuid;

use crate::entity::passive::frog_goals::{
    self, CroakGoal, FrogStrollGoal, LongJumpGoal, LongJumpMidJumpGoal, ShootTongueGoal,
    TryFindLandGoal, TryFindLandNearLiquidGoal, TryLaySpawnGoal,
};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::behavior::hunting::{self, Hunt},
    ai::control::smooth_swimming_move_control::SmoothSwimmingMoveControl,
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, gated::GatedGoal,
        look_at_entity::LookAtEntityGoal, random_swimming::RandomSwimmingGoal, tempt::TemptGoal,
    },
    ai::pathfinder::{Navigator, node::PathType},
    ai::target_match::TargetMatch,
    living::LivingEntity,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

use pumpkin_data::frog_variant::FrogVariant;

pub const FROG_FOOD: &[&Item] = &[&Item::SLIME_BALL];

// frog: constants (Frog, FrogAi)
const TONGUE_TARGET_RANGE_SQ: f64 = 100.0;
const UNREACHABLE_TONGUE_TARGETS_TICKS: i64 = 100;
const MAX_UNREACHABLE_TONGUE_TARGETS: usize = 5;
const FROG_FALL_DAMAGE_REDUCTION: i32 = 5;
const SPEED_WHEN_TEMPTED: f64 = 1.25;
const SPEED_WHEN_PANICKING: f64 = 2.0;

// frog: FrogAttackablesSensor, eatable, close and not recently unreachable
fn attackable_target(hunter: &dyn Mob, target: &LivingEntity, world: &World) -> bool {
    let Some(frog) = hunter.cast_any().downcast_ref::<FrogEntity>() else {
        return false;
    };
    !hunter.get_mob_entity().is_in_love()
        && hunter
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.entity.pos.load())
            < TONGUE_TARGET_RANGE_SQ
        && frog.is_reachable_tongue_target(&target.entity.entity_uuid, world.get_world_age())
        && world
            .get_entity_by_id(target.entity.entity_id)
            .is_some_and(|entity| FrogEntity::can_eat(entity.as_ref()))
}

const FROG_HUNTS: &[Hunt] = &[Hunt::new(
    1,
    TargetMatch::Tag(&tag::EntityType::MINECRAFT_FROG_FOOD),
    true,
)
.matching(attackable_target)];

/// Represents a Frog, an amphibious mob that can eat small slimes and magma cubes.
///
/// Wiki: <https://minecraft.wiki/w/Frog>
pub struct FrogEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub tongue_target_id: AtomicI32,
    pub pregnant: AtomicBool,
    pub mid_jump: AtomicBool,
    pub long_jump_cooldown: AtomicI32,
    unreachable_tongue_targets: Mutex<Vec<(Uuid, i64)>>,
}

impl FrogEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        *navigator = Navigator::amphibious(true);
        // frog: pathfinding malus (Frog constructor)
        navigator.set_pathfinding_malus(PathType::Water, 4.0);
        navigator.set_pathfinding_malus(PathType::Trapdoor, -1.0);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) =
            Box::new(SmoothSwimmingMoveControl::new(85, 10, 0.02, 0.1, true));
        let frog = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(FrogVariant::Temperate.id() as i32),
            tongue_target_id: AtomicI32::new(-1),
            pregnant: AtomicBool::new(false),
            mid_jump: AtomicBool::new(false),
            long_jump_cooldown: AtomicI32::new(frog_goals::time_between_long_jumps()),
            unreachable_tongue_targets: Mutex::new(Vec::new()),
        };
        let mob_arc = Arc::new(frog);
        let mob_weak: std::sync::Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            use frog_goals::{
                idle_only, idle_or_swim, lay_spawn_only, long_jump_activity, swim_only,
                tongue_activity,
            };
            let goals = &mob_arc.mob_entity.goals_selector;

            // frog: CORE activity
            goals.add_goal(0, EscapeDangerGoal::new(SPEED_WHEN_PANICKING));
            // frog: TONGUE activity
            goals.add_goal(
                0,
                Box::new(GatedGoal::new(ShootTongueGoal::new(), tongue_activity)),
            );
            // frog: LONG_JUMP activity
            goals.add_goal(1, Box::new(LongJumpMidJumpGoal));
            goals.add_goal(
                2,
                Box::new(GatedGoal::new(LongJumpGoal::new(), long_jump_activity)),
            );
            // frog: LAY_SPAWN activity
            goals.add_goal(
                3,
                Box::new(GatedGoal::new(
                    TryFindLandNearLiquidGoal::new(8, 1.0),
                    lay_spawn_only,
                )),
            );
            goals.add_goal(4, Box::new(GatedGoal::new(TryLaySpawnGoal, lay_spawn_only)));
            // frog: IDLE and SWIM activities
            goals.add_goal(
                5,
                Box::new(GatedGoal::new(
                    *LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
                    idle_or_swim,
                )),
            );
            goals.add_goal(5, Box::new(GatedGoal::new(*BreedGoal::new(1.0), idle_only)));
            goals.add_goal(
                6,
                Box::new(GatedGoal::new(
                    TemptGoal::new(SPEED_WHEN_TEMPTED, FROG_FOOD, false),
                    idle_or_swim,
                )),
            );
            goals.add_goal(
                8,
                Box::new(GatedGoal::new(TryFindLandGoal::new(8, 1.5), swim_only)),
            );
            goals.add_goal(
                9,
                Box::new(GatedGoal::new(
                    RandomSwimmingGoal::new(0.75, 120),
                    swim_only,
                )),
            );
            goals.add_goal(
                9,
                Box::new(GatedGoal::new(FrogStrollGoal::new(1.0), idle_or_swim)),
            );
            goals.add_goal(10, Box::new(GatedGoal::new(CroakGoal::new(), idle_only)));
            goals.add_goal(
                10,
                Box::new(GatedGoal::new(CroakGoal::new(), lay_spawn_only)),
            );
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;
            hunting::apply(target_selector, &mob_arc.mob_entity, FROG_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn is_pregnant(&self) -> bool {
        self.pregnant.load(Ordering::Relaxed)
    }

    pub fn set_pregnant(&self, pregnant: bool) {
        self.pregnant.store(pregnant, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_mid_jump(&self) -> bool {
        self.mid_jump.load(Ordering::Relaxed)
    }

    pub fn set_mid_jump(&self, mid_jump: bool) {
        self.mid_jump.store(mid_jump, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_tempted(&self) -> bool {
        self.mob_entity.goals_selector.is_running::<TemptGoal>()
    }

    #[must_use]
    pub fn is_panicking(&self) -> bool {
        self.mob_entity
            .goals_selector
            .is_running::<EscapeDangerGoal>()
    }

    // frog: Frog.canEat, small slimes and magma cubes only
    #[must_use]
    pub fn can_eat(target: &dyn EntityBase) -> bool {
        if !target
            .get_entity()
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_FROG_FOOD)
        {
            return false;
        }
        // frog: AbstractCubeMob size must be 1
        if let Some(slime) = target
            .cast_any()
            .downcast_ref::<crate::entity::mob::slime::SlimeEntity>()
        {
            return slime.get_size() == 1;
        }
        if let Some(magma) = target
            .cast_any()
            .downcast_ref::<crate::entity::mob::magma_cube::MagmaCubeEntity>()
        {
            return magma.slime.get_size() == 1;
        }
        true
    }

    pub fn set_tongue_target(&self, target: Option<&dyn EntityBase>) {
        let id = target.map(|target| target.get_entity().entity_id);
        self.tongue_target_id
            .store(id.unwrap_or(-1), Ordering::Relaxed);
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::frog::DATA_TONGUE_TARGET_ID,
            OptionalInt(id),
        );
    }

    #[must_use]
    pub fn tongue_target(&self) -> Option<Arc<dyn EntityBase>> {
        let id = self.tongue_target_id.load(Ordering::Relaxed);
        (id >= 0)
            .then(|| self.get_entity().world.load().get_entity_by_id(id))
            .flatten()
    }

    // frog: UNREACHABLE_TONGUE_TARGETS memory, 5 entries for 100 ticks
    pub fn add_unreachable_tongue_target(&self, uuid: Uuid, now: i64) {
        let mut targets = self
            .unreachable_tongue_targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        targets.retain(|(_, expiry)| *expiry > now);
        if !targets.iter().any(|(known, _)| *known == uuid) {
            if targets.len() >= MAX_UNREACHABLE_TONGUE_TARGETS {
                targets.remove(0);
            }
            targets.push((uuid, now + UNREACHABLE_TONGUE_TARGETS_TICKS));
        }
    }

    #[must_use]
    pub fn is_reachable_tongue_target(&self, uuid: &Uuid, now: i64) -> bool {
        !self
            .unreachable_tongue_targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(known, expiry)| known == uuid && *expiry > now)
    }

    #[must_use]
    pub fn get_variant(&self) -> FrogVariant {
        FrogVariant::from_id(self.variant.load(Ordering::Relaxed) as u32)
    }

    pub fn set_variant(&self, variant: FrogVariant) {
        self.set_variant_id(variant.id() as i32);
    }

    pub fn set_variant_id(&self, raw_id: i32) {
        self.variant.store(raw_id, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::frog::VARIANT, VarInt(raw_id));
    }
}

impl AgeableMob for FrogEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn can_be_a_baby(&self) -> bool {
        false
    }
}

impl Animal for FrogEntity {
    // frog: eat sound (Frog.playEatingSound)
    fn eating_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        Some(pumpkin_data::sound::Sound::EntityFrogEat)
    }

    // frog: Frog.getBreedOffspring, none (the mother lays frogspawn)
    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        crate::entity::r#type::from_type(
            entity.entity_type,
            entity.pos.load(),
            &entity.world.load(),
            Uuid::new_v4(),
        )
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_FROG_FOOD)
            || FROG_FOOD.iter().any(|i| i.id == item_stack.item.id)
    }
}

impl Mob for FrogEntity {
    // frog: biome coat, long jump cooldown (Frog.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let biome = world.get_biome(&self.get_entity().block_pos.load());
        let variant = if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_COLD_VARIANT_FROGS) {
            FrogVariant::Cold
        } else if biome.has_tag(&tag::WorldgenBiome::MINECRAFT_SPAWNS_WARM_VARIANT_FROGS) {
            FrogVariant::Warm
        } else {
            FrogVariant::Temperate
        };
        self.set_variant(variant);
        self.long_jump_cooldown
            .store(frog_goals::time_between_long_jumps(), Ordering::Relaxed);
        self.finalize_spawn_super(group_data)
    }

    // frog: pregnant instead of a baby (Frog.spawnChildFromBreeding)
    fn custom_breed(&self, mate: &dyn EntityBase) -> bool {
        let Some(mate_mob) = mate.get_mob() else {
            return false;
        };
        crate::entity::ai::goal::breed::BreedGoal::award_breeding(self, mate, None);
        self.set_pregnant(true);
        self.mob_entity
            .breeding_cooldown
            .store(6000, Ordering::Relaxed);
        mate.set_breeding_cooldown(6000);
        self.mob_entity.reset_love_ticks();
        mate_mob.get_mob_entity().reset_love_ticks();
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if world.level_info.load().game_rules.mob_drops {
            crate::entity::experience_orb::ExperienceOrbEntity::spawn(
                &world,
                entity.pos.load(),
                rand::random_range(1..=7),
            );
        }
        true
    }

    // frog: createAttributes, attack damage 10.0 (Frog.createAttributes)
    fn mob_attack_damage(&self, _base: f32) -> f32 {
        10.0
    }

    // frog: Frog.calculateFallDamage
    fn fall_damage_reduction(&self) -> i32 {
        FROG_FALL_DAMAGE_REDUCTION
    }

    fn mob_is_pushed_by_fluids(&self) -> bool {
        false
    }

    // frog: CountDownCooldownTicks(LONG_JUMP_COOLDOWN_TICKS) and the tongue pose reset
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let cooldown = self.long_jump_cooldown.load(Ordering::Relaxed);
        if cooldown > 0 {
            self.long_jump_cooldown
                .store(cooldown - 1, Ordering::Relaxed);
        }
        if self.tongue_target_id.load(Ordering::Relaxed) >= 0
            && self.mob_entity.get_target().is_none()
        {
            self.set_tongue_target(None);
        }
    }

    // frog: slow swim without gravity pull (Frog.travelInWater)
    fn travel_in_water(&self, caller: &dyn EntityBase, _gravity: f64, _falling: bool) {
        let living = &self.mob_entity.living_entity;
        let entity = &living.entity;
        entity.update_velocity_from_input(
            living.movement_input.load(),
            f64::from(living.get_speed()),
        );
        living.make_move(caller);
        entity.velocity.store(entity.velocity.load() * 0.9);
    }

    // frog: Frog.getAmbientSound
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityFrogAmbient)
    }

    // frog: head rotation limit (Frog.getMaxHeadYRot), look control keeps pitch while using the tongue
    fn look_resets_pitch(&self) -> bool {
        self.tongue_target_id.load(Ordering::Relaxed) < 0
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityFrogStep, 0.15, 1.0);
    }

    fn mob_get_component(
        &self,
        component: pumpkin_data::data_component::DataComponent,
    ) -> Option<Box<dyn pumpkin_data::data_component_impl::DataComponentImpl>> {
        use pumpkin_data::{data_component::DataComponent, data_component_impl::FrogVariantImpl};
        match component {
            DataComponent::FrogVariant => Some(Box::new(FrogVariantImpl {
                value: format!("minecraft:{}", self.get_variant().to_name()).into(),
            })),
            _ => None,
        }
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let variant_id = self.variant.load(Ordering::Relaxed) as u32;
        let variant_str =
            self.resolve_dynamic_entry_id("minecraft:frog_variant", variant_id, |_id| {
                self.get_variant().asset_id().to_string()
            });
        nbt.put_string("variant", variant_str);
        nbt.put_bool("IsPregnant", self.is_pregnant());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_pregnant(nbt.get_bool("IsPregnant").unwrap_or(false));
        if let Some(variant_str) = nbt.get_string("variant")
            && let Some(id) =
                self.resolve_dynamic_raw_id("minecraft:frog_variant", variant_str, |n| {
                    FrogVariant::from_name(n).map(|v| v.id())
                })
        {
            self.set_variant_id(id as i32);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_set_variant_name(&self, name: &str) {
        if let Some(id) = self.resolve_dynamic_raw_id("minecraft:frog_variant", name, |n| {
            FrogVariant::from_name(n).map(|v| v.id())
        }) {
            self.set_variant_id(id as i32);
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::frog::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::frog::VARIANT,
            VarInt(self.variant.load(Ordering::Relaxed)),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityFrogAmbient)
    }
}
