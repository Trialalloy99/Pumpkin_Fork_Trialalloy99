use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::ActorEventID;
use pumpkin_util::GameMode;
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::goal::track_target::TrackTargetGoal;
use crate::entity::ai::goal::{Controls, Goal, to_goal_ticks};
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::ai::util::{default_random_pos, land_random_pos};
use crate::entity::passive::villager::VillagerEntity;
use crate::entity::{
    Entity, EntityBase,
    ai::behavior::neutral::apply_targets,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal, move_towards_target::MoveTowardsTargetGoal,
        offer_flower::OfferFlowerGoal, revenge::RevengeGoal,
    },
    mob::{
        Mob, MobEntity,
        neutral::{NeutralData, NeutralMob},
    },
    player::Player,
};
use crate::plugin::api::events::entity::entity_target::TargetReason;
use crate::world::World;
use crate::world::village::sections_to_village_in;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

const VILLAGER_SCAN_RADIUS: f64 = 32.0;
const GOLEM_LEASH_EYE_FACTOR: f64 = 0.875;
const GOLEM_LEASH_WIDTH_FACTOR: f64 = 0.4;

/// Represents an Iron Golem, a powerful neutral mob that protects villagers and players.
///
/// Wiki: <https://minecraft.wiki/w/Iron_Golem>
const IRON_GOLEM_HUNTS: &[Hunt] = &[Hunt::new(3, TargetMatch::Enemy, false)
    .every(5)
    .matching(|_hunter, target, _world| target.entity.entity_type != &EntityType::CREEPER)];

pub struct IronGolemEntity {
    pub mob_entity: MobEntity,
    pub neutral_data: NeutralData,
    pub player_created: AtomicBool,
    pub attack_animation_tick: AtomicI32,
    pub offer_flower_tick: AtomicI32,
    crackiness: AtomicU8,
}

impl IronGolemEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        // iron golem: getLeashOffset, (0, 0.875 * eyeHeight, width * 0.4)
        mob_entity
            .living_entity
            .entity
            .leash_offset_factors
            .store(Some((GOLEM_LEASH_EYE_FACTOR, GOLEM_LEASH_WIDTH_FACTOR)));
        let iron_golem = Self {
            mob_entity,
            neutral_data: NeutralData::default(),
            player_created: AtomicBool::new(false),
            attack_animation_tick: AtomicI32::new(0),
            offer_flower_tick: AtomicI32::new(0),
            crackiness: AtomicU8::new(0),
        };
        let mob_arc = Arc::new(iron_golem);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            goal_selector.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, true)));
            goal_selector.add_goal(2, Box::new(MoveTowardsTargetGoal::new(0.9, 32.0)));
            // iron golem: village goals (IronGolem.registerGoals)
            goal_selector.add_goal(2, Box::new(VillageStrollGoal::move_back(0.6)));
            goal_selector.add_goal(4, Box::new(VillageStrollGoal::golem_stroll(0.6)));
            goal_selector.add_goal(5, Box::new(OfferFlowerGoal::new()));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));

            target_selector.add_goal(1, Box::new(DefendVillageTargetGoal::new()));
            target_selector.add_goal(2, Box::new(RevengeGoal::new(true)));
            apply_targets(target_selector, &mob_arc.mob_entity, 3, 4, false);
            hunting::apply(target_selector, &mob_arc.mob_entity, IRON_GOLEM_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn is_player_created(&self) -> bool {
        self.player_created.load(Ordering::Relaxed)
    }

    pub fn set_player_created(&self, value: bool) {
        self.player_created.store(value, Ordering::Relaxed);
        let entity = self.get_entity();
        let flag: u8 = u8::from(value);
        entity.set_synced_data(pumpkin_data::tracked_data::iron_golem::FLAGS_ID, flag);
    }

    pub fn offer_flower(&self, offer: bool) {
        let entity = self.get_entity();
        let world = entity.world.load();
        if offer {
            self.offer_flower_tick.store(400, Ordering::Relaxed);
            world.send_entity_status(
                entity,
                EntityStatus::OfferFlower,
                ActorEventID::StartOfferFlower,
            );
        } else {
            self.offer_flower_tick.store(0, Ordering::Relaxed);
            world.send_entity_status(
                entity,
                EntityStatus::StopOfferFlower,
                ActorEventID::StopOfferFlower,
            );
        }
    }

    #[must_use]
    pub fn get_offer_flower_tick(&self) -> i32 {
        self.offer_flower_tick.load(Ordering::Relaxed)
    }

    /// Trigger the attack animation for the iron golem.
    /// This sends `EntityStatus::StartAttacking` to clients and sets the `attack_animation_tick` counter.
    pub fn trigger_attack_animation(&self) {
        self.attack_animation_tick.store(10, Ordering::Relaxed);
        let entity = self.get_entity();
        let world = entity.world.load();
        world.send_entity_status(
            entity,
            EntityStatus::StartAttacking,
            ActorEventID::StartAttacking,
        );
    }
}

impl IronGolemEntity {
    // iron golem: 0 none, 1 low, 2 medium, 3 high (Crackiness.GOLEM)
    fn crackiness_level(&self) -> u8 {
        let living = &self.mob_entity.living_entity;
        let fraction = living.health.load() / living.get_max_health();
        if fraction < 0.25 {
            3
        } else if fraction < 0.5 {
            2
        } else {
            u8::from(fraction < 0.75)
        }
    }
}

impl NeutralMob for IronGolemEntity {
    fn get_neutral_data(&self) -> &NeutralData {
        &self.neutral_data
    }
}

impl Mob for IronGolemEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityIronGolemStep, 1.0, 1.0);
    }

    fn can_attack(&self, target: &dyn EntityBase) -> bool {
        let target_type = target.get_entity().entity_type;
        if self.is_player_created() && target_type == &EntityType::PLAYER {
            return false;
        }
        // Vanilla's `super.canAttack` goes through `Mob` crepper and ghast
        target_type != &EntityType::CREEPER
            && target_type != &EntityType::GHAST
            && self.get_mob_entity().living_entity.can_attack(target)
    }

    // iron golem: half to 1.5x damage (IronGolem.doHurtTarget)
    fn mob_attack_damage(&self, base: f32) -> f32 {
        let whole = base as i32;
        if whole > 0 {
            base / 2.0 + self.get_random().random_range(0..whole) as f32
        } else {
            base
        }
    }

    fn pre_attack(&self, _target: &dyn EntityBase) {
        self.play_entity_sound(Sound::EntityIronGolemAttack, 1.0, 1.0);
    }

    // iron golem: toss target up (IronGolem.doHurtTarget)
    fn on_attack(&self, target: &dyn EntityBase) {
        let resistance = target.get_living_entity().map_or(0.0, |living| {
            living.get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE)
        });
        let scale = (1.0 - resistance).max(0.0);
        let entity = target.get_entity();
        let velocity = entity.velocity.load();
        entity.set_velocity(velocity.add_raw(0.0, f64::from(0.4f32) * scale, 0.0));
    }

    fn pre_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.crackiness
            .store(self.crackiness_level(), Ordering::Relaxed);
        self.pre_damage(damage_type, source)
    }

    // iron golem: crack sound (IronGolem.hurtServer)
    fn on_damage_with_context(
        &self,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        if self.crackiness_level() != self.crackiness.load(Ordering::Relaxed) {
            self.play_entity_sound(Sound::EntityIronGolemDamage, 1.0, 1.0);
        }
        self.on_damage(damage_type, source);
        true
    }

    // iron golem: shove enemies into a target (IronGolem.doPush)
    fn on_push_entity(&self, other: &Arc<dyn EntityBase>) {
        let other_type = other.get_entity().entity_type;
        if TargetMatch::Enemy.matches(other_type)
            && other_type != &EntityType::CREEPER
            && self.get_random().random_range(0..20) == 0
        {
            self.set_mob_target(Some(other.clone()), TargetReason::Unknown);
        }
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        false
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        120
    }

    // iron golem: solid floor, two free blocks above (IronGolem.checkSpawnObstruction)
    fn check_spawn_obstruction(&self, world: &World) -> bool {
        let pos = self.get_entity().block_pos.load();
        if !world.get_block_state(&pos.down()).is_solid() {
            return false;
        }
        for i in 1..3 {
            let state = world.get_block_state(&pos.up_height(i));
            if !crate::world::natural_spawner::is_valid_empty_spawn_block(
                state,
                &EntityType::IRON_GOLEM,
            ) {
                return false;
            }
        }
        let state = world.get_block_state(&pos);
        (state.is_liquid() && !state.is_full_cube()
            || crate::world::natural_spawner::is_valid_empty_spawn_block(
                state,
                &EntityType::IRON_GOLEM,
            ))
            && world
                .get_entities_at_box(&self.get_entity().bounding_box.load())
                .iter()
                .all(|entity| entity.get_entity().entity_id == self.get_entity().entity_id)
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("PlayerCreated", self.is_player_created());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(created) = nbt.get_bool("PlayerCreated") {
            self.set_player_created(created);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_iron_golem(&self) -> Option<&crate::entity::passive::iron_golem::IronGolemEntity> {
        Some(self)
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let attack_tick = self.attack_animation_tick.load(Ordering::Relaxed);
        if attack_tick > 0 {
            self.attack_animation_tick.fetch_sub(1, Ordering::Relaxed);
        }

        let flower_tick = self.offer_flower_tick.load(Ordering::Relaxed);
        if flower_tick > 0 {
            self.offer_flower_tick.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let flag: u8 = u8::from(self.is_player_created());
        entity.set_synced_data(pumpkin_data::tracked_data::iron_golem::FLAGS_ID, flag);
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.item.id == Item::IRON_INGOT.id {
            let living = &self.mob_entity.living_entity;
            let current_health = living.health.load();
            let max_health = living.get_max_health();
            if current_health < max_health {
                // iron golem: repair (IronGolem.mobInteract)
                living.heal(25.0);
                let mut random = self.get_random();
                let pitch = (random.random::<f32>() - random.random::<f32>()).mul_add(0.2, 1.0);
                self.play_entity_sound(Sound::EntityIronGolemRepair, 1.0, pitch);
                if player.gamemode.load() != GameMode::Creative {
                    item_stack.item_count = item_stack.item_count.saturating_sub(1);
                }
                return true;
            }
        }
        false
    }
}

// iron golem: stroll variants (MoveBackToVillageGoal, GolemRandomStrollInVillageGoal)
#[derive(Clone, Copy, PartialEq, Eq)]
enum VillageStrollKind {
    MoveBack,
    GolemStroll,
}

struct VillageStrollGoal {
    kind: VillageStrollKind,
    speed: f64,
    interval: i32,
    target: Option<Vector3<f64>>,
}

impl VillageStrollGoal {
    const fn move_back(speed: f64) -> Self {
        Self {
            kind: VillageStrollKind::MoveBack,
            speed,
            interval: 10,
            target: None,
        }
    }

    const fn golem_stroll(speed: f64) -> Self {
        Self {
            kind: VillageStrollKind::GolemStroll,
            speed,
            interval: 240,
            target: None,
        }
    }

    // golem: section closest to a village (MoveBackToVillageGoal.getPosition)
    fn move_back_position(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let pos = entity.block_pos.load();
        let villages = world.occupied_village_sections(&pos, 2);
        let here = [pos.0.x >> 4, pos.0.y >> 4, pos.0.z >> 4];
        let current = sections_to_village_in(&villages, here);
        let mut best = here;
        let mut best_distance = current;
        // golem: BehaviorUtils.findSectionClosestToVillage, cube radius 2
        for dx in -2..=2 {
            for dy in -2..=2 {
                for dz in -2..=2 {
                    let section = [here[0] + dx, here[1] + dy, here[2] + dz];
                    let distance = sections_to_village_in(&villages, section);
                    if distance < best_distance {
                        best = section;
                        best_distance = distance;
                    }
                }
            }
        }
        if best == here {
            return None;
        }
        let towards = Vector3::new(
            f64::from(best[0] * 16 + 8) + 0.5,
            f64::from(best[1] * 16 + 8),
            f64::from(best[2] * 16 + 8) + 0.5,
        );
        default_random_pos::get_pos_towards(mob, 10, 7, towards, std::f64::consts::FRAC_PI_2)
    }

    // golem: random occupied village poi nearby (GolemRandomStrollInVillageGoal.getPositionTowardsPoi)
    fn poi_position(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let pos = entity.block_pos.load();
        let villages = world.occupied_village_sections(&pos, 2);
        if villages.is_empty() {
            return None;
        }
        let here = [pos.0.x >> 4, pos.0.y >> 4, pos.0.z >> 4];
        let mut village_sections = Vec::new();
        for dx in -2..=2 {
            for dy in -2..=2 {
                for dz in -2..=2 {
                    let section = [here[0] + dx, here[1] + dy, here[2] + dz];
                    if sections_to_village_in(&villages, section) == 0 {
                        village_sections.push(section);
                    }
                }
            }
        }
        if village_sections.is_empty() {
            return None;
        }
        let mut random = mob.get_random();
        let section = village_sections[random.random_range(0..village_sections.len())];
        let center = BlockPos::new(
            section[0] * 16 + 8,
            section[1] * 16 + 8,
            section[2] * 16 + 8,
        );
        let pois = world.occupied_pois_in_range(center, 8);
        if pois.is_empty() {
            return None;
        }
        let poi = pois[random.random_range(0..pois.len())];
        land_random_pos::get_pos_towards(
            mob,
            10,
            7,
            Vector3::new(
                f64::from(poi.0.x) + 0.5,
                f64::from(poi.0.y),
                f64::from(poi.0.z) + 0.5,
            ),
        )
    }

    // golem: GolemRandomStrollInVillageGoal.getPositionTowardsVillagerWhoWantsGolem
    fn villager_position(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let game_time = world.get_world_age();
        let area = entity.bounding_box.load().expand(
            VILLAGER_SCAN_RADIUS,
            VILLAGER_SCAN_RADIUS,
            VILLAGER_SCAN_RADIUS,
        );
        let villagers: Vec<Arc<dyn EntityBase>> = world
            .get_entities_at_box(&area)
            .into_iter()
            .filter(|villager| {
                villager.get_entity().entity_type == &EntityType::VILLAGER
                    && villager.get_mob().is_some_and(|villager_mob| {
                        villager_mob
                            .get_mob_entity()
                            .try_with_brain(villager_mob, |brain| {
                                VillagerEntity::wants_to_spawn_golem_in(brain, game_time)
                            })
                            .unwrap_or(false)
                    })
            })
            .collect();
        if villagers.is_empty() {
            return None;
        }
        let villager = &villagers[mob.get_random().random_range(0..villagers.len())];
        land_random_pos::get_pos_towards(mob, 10, 7, villager.get_entity().pos.load())
    }

    fn golem_stroll_position(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let mut random = mob.get_random();
        if random.random::<f32>() < 0.3 {
            return land_random_pos::get_pos(mob, 10, 7);
        }
        let target = if random.random::<f32>() < 0.7 {
            Self::villager_position(mob).or_else(|| Self::poi_position(mob))
        } else {
            Self::poi_position(mob).or_else(|| Self::villager_position(mob))
        };
        target.or_else(|| land_random_pos::get_pos(mob, 10, 7))
    }
}

impl Goal for VillageStrollGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if mob.get_entity().has_passengers() {
            return false;
        }
        if mob
            .get_random()
            .random_range(0..to_goal_ticks(self.interval))
            != 0
        {
            return false;
        }
        self.target = match self.kind {
            VillageStrollKind::MoveBack => {
                let entity = mob.get_entity();
                if entity.world.load().is_village(&entity.block_pos.load()) {
                    return false;
                }
                Self::move_back_position(mob)
            }
            VillageStrollKind::GolemStroll => Self::golem_stroll_position(mob),
        };
        self.target.is_some()
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        !mob.is_navigator_idle() && !mob.get_entity().has_passengers()
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(target) = self.target {
            let pos = mob.get_entity().pos.load();
            mob.get_mob_entity()
                .steered()
                .navigator()
                .set_progress(NavigatorGoal::new(pos, target, self.speed));
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.target = None;
        mob.get_mob_entity().steered().navigator().stop();
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// iron golem: attack players villagers hate (DefendVillageTargetGoal)
struct DefendVillageTargetGoal {
    track_target_goal: TrackTargetGoal,
    predicate: TargetPredicate,
    potential_target: Option<Arc<dyn EntityBase>>,
}

impl DefendVillageTargetGoal {
    fn new() -> Self {
        Self {
            track_target_goal: TrackTargetGoal::new(false, true),
            predicate: TargetPredicate::create_attackable().set_base_max_distance(64.0),
            potential_target: None,
        }
    }
}

impl Goal for DefendVillageTargetGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let bb = entity.bounding_box.load();
        let area = BoundingBox::new(
            bb.min.sub_raw(10.0, 8.0, 10.0),
            bb.max.add_raw(10.0, 8.0, 10.0),
        );
        let players: Vec<Arc<Player>> = world
            .get_players_at_box(&area)
            .into_iter()
            .filter(|player| self.predicate.test(&world, Some(mob), player.as_ref()))
            .collect();
        if !players.is_empty() {
            for villager in world.get_entities_at_box(&area) {
                if villager.get_entity().entity_type != &EntityType::VILLAGER
                    || !self.predicate.test(&world, Some(mob), villager.as_ref())
                {
                    continue;
                }
                let Some(villager) = villager.cast_any().downcast_ref::<VillagerEntity>() else {
                    continue;
                };
                for player in &players {
                    if villager.get_player_reputation(&player.get_entity().entity_uuid) <= -100 {
                        self.potential_target = Some(player.clone() as Arc<dyn EntityBase>);
                    }
                }
            }
        }
        // golem: potentialTarget is never cleared (DefendVillageTargetGoal.canUse)
        self.potential_target.as_ref().is_some_and(|target| {
            target
                .get_player()
                .is_none_or(|player| !player.is_spectator() && !player.is_creative())
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.track_target_goal.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.track_target_goal.start_targeting(
            mob,
            self.potential_target.clone(),
            TargetReason::Unknown,
        );
        self.track_target_goal.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.track_target_goal.stop(mob);
    }

    fn controls(&self) -> Controls {
        Controls::TARGET
    }
}
