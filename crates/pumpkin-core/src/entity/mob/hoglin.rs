use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::{Block, BlockState};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        active_target::ActiveTargetGoal, avoid_entity::AvoidEntityGoal, breed::BreedGoal,
        follow_parent::FollowParentGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal,
        wander_around::WanderAroundGoal,
    },
    ai::pathfinder::NavigatorGoal,
    ai::util::default_random_pos,
    custom_sound::CustomSound,
    mob::{Mob, MobEntity, conversion::convert_single, spawn::SpawnGroupData},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

pub struct HoglinEntity {
    pub mob_entity: MobEntity,
    pub immune_to_zombification: AtomicBool,
    pub time_in_overworld: AtomicI32,
    pub cannot_be_hunted: AtomicBool,
    pub ageable_data: AgeableData,
    attack_animation_remaining_ticks: AtomicI32,
    pacified_ticks: AtomicI32,
    nearest_repellent: AtomicCell<Option<BlockPos>>,
    avoid_target: Mutex<Option<Arc<dyn EntityBase>>>,
    avoid_ticks: AtomicI32,
    was_fighting: AtomicBool,
    was_avoiding: AtomicBool,
    attack_target_ticks: AtomicI32,
    temporary_attack_target_id: AtomicI32,
}

fn is_adult(mob: &dyn Mob) -> bool {
    !mob.is_baby_mob()
}

fn is_baby(mob: &dyn Mob) -> bool {
    mob.is_baby_mob()
}

fn is_idle_adult(mob: &dyn Mob) -> bool {
    !mob.is_baby_mob() && mob.get_mob_entity().get_target().is_none()
}

fn may_attack_player(mob: &dyn Mob) -> bool {
    mob.get_mob_entity().love_ticks.load(Ordering::Relaxed) <= 0 && !mob.is_pacified()
}

struct HoglinRetreatGoal {
    hoglin: Weak<HoglinEntity>,
    recalculate_ticks: i32,
}

impl Goal for HoglinRetreatGoal {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.hoglin.upgrade().is_some_and(|hoglin| {
            hoglin.avoid_ticks.load(Ordering::Relaxed) > 0
                || hoglin.nearest_repellent.load().is_some()
        })
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.can_start(mob)
    }

    fn tick(&mut self, _mob: &dyn Mob) {
        self.recalculate_ticks -= 1;
        if self.recalculate_ticks > 0 {
            return;
        }
        self.recalculate_ticks = 10;
        let Some(hoglin) = self.hoglin.upgrade() else {
            return;
        };
        let (away_from, speed, horizontal, vertical) = {
            let target = hoglin
                .avoid_target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(target) = target {
                (Some(target.get_entity().pos.load()), 1.3, 16, 7)
            } else {
                (
                    hoglin.nearest_repellent.load().map(|pos| pos.to_f64()),
                    1.0,
                    8,
                    4,
                )
            }
        };
        let Some(away_from) = away_from else {
            return;
        };
        if let Some(away) =
            default_random_pos::get_pos_away(hoglin.as_ref(), horizontal, vertical, away_from)
        {
            let entity = hoglin.get_entity();
            hoglin
                .mob_entity
                .steered()
                .navigator()
                .set_progress(NavigatorGoal::new(entity.pos.load(), away, speed));
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

impl HoglinEntity {
    pub const CONVERSION_TIME: i32 = 300;
    pub const XP_REWARD: u32 = 5;
    pub const BABY_XP_REWARD: u32 = 3;
    pub const BABY_DIMENSIONS: EntityDimensions = EntityDimensions {
        width: 0.75,
        height: 0.85,
        eye_height: 0.625,
    };

    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        {
            let mut attributes = mob_entity
                .living_entity
                .attributes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(health) = attributes.get_mut(&Attributes::MAX_HEALTH.id) {
                health.base_value = 40.0;
                health.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(speed) = attributes.get_mut(&Attributes::MOVEMENT_SPEED.id) {
                speed.base_value = 0.3;
                speed.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(knockback_res) = attributes.get_mut(&Attributes::KNOCKBACK_RESISTANCE.id) {
                knockback_res.base_value = 0.6;
                knockback_res.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(attack_kb) = attributes.get_mut(&Attributes::ATTACK_KNOCKBACK.id) {
                attack_kb.base_value = 1.0;
                attack_kb.dirty.store(true, Ordering::Relaxed);
            }
            if let Some(damage) = attributes.get_mut(&Attributes::ATTACK_DAMAGE.id) {
                damage.base_value = 6.0;
                damage.dirty.store(true, Ordering::Relaxed);
            }
        }
        mob_entity.living_entity.health.store(40.0);

        let hoglin = Self {
            mob_entity,
            immune_to_zombification: AtomicBool::new(false),
            time_in_overworld: AtomicI32::new(0),
            cannot_be_hunted: AtomicBool::new(false),
            ageable_data: AgeableData::default(),
            attack_animation_remaining_ticks: AtomicI32::new(0),
            pacified_ticks: AtomicI32::new(0),
            nearest_repellent: AtomicCell::new(None),
            avoid_target: Mutex::new(None),
            avoid_ticks: AtomicI32::new(0),
            was_fighting: AtomicBool::new(false),
            was_avoiding: AtomicBool::new(false),
            attack_target_ticks: AtomicI32::new(0),
            temporary_attack_target_id: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(hoglin);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(
                0,
                Box::new(HoglinRetreatGoal {
                    hoglin: Arc::downgrade(&mob_arc),
                    recalculate_ticks: 0,
                }),
            );
            goal_selector.add_goal(1, BreedGoal::new(0.6));
            goal_selector.add_goal(
                2,
                Box::new(
                    AvoidEntityGoal::new(&EntityType::PIGLIN, 8.0, 0.4, 0.4).when(is_idle_adult),
                ),
            );
            goal_selector.add_goal(
                3,
                Box::new(
                    MeleeAttackGoal::new(1.0, true)
                        .with_interval(40)
                        .gated_by(is_adult),
                ),
            );
            goal_selector.add_goal(
                3,
                Box::new(
                    MeleeAttackGoal::new(1.0, true)
                        .with_interval(15)
                        .gated_by(is_baby),
                ),
            );
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(0.6)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(0.4)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(
                1,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::PLAYER, true)
                    .gated_by(may_attack_player),
            );
        };

        mob_arc
    }

    pub fn is_immune_to_zombification(&self) -> bool {
        self.immune_to_zombification.load(Ordering::Relaxed)
    }

    pub fn set_immune_to_zombification(&self, immune: bool) {
        self.immune_to_zombification
            .store(immune, Ordering::Relaxed);
        self.mob_entity.living_entity.entity.set_synced_data(
            pumpkin_data::tracked_data::hoglin::DATA_IMMUNE_TO_ZOMBIFICATION,
            immune,
        );
    }

    pub fn is_converting(&self, world: &World) -> bool {
        !self.is_immune_to_zombification()
            && !self.mob_entity.is_no_ai()
            && world.dimension.piglins_zombify
    }

    fn begin_avoidance(&self, target: Arc<dyn EntityBase>) {
        self.set_mob_target(
            None,
            crate::plugin::api::events::entity::entity_target::TargetReason::Fleeing,
        );
        *self
            .avoid_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(target);
        self.avoid_ticks
            .store(rand::random_range(100..=400), Ordering::Relaxed);
    }

    fn visible_adult_hoglins(&self) -> Vec<Arc<dyn EntityBase>> {
        let entity = self.get_entity();
        entity
            .world
            .load()
            .get_entities_at_box(&entity.bounding_box.load().expand(16.0, 8.0, 16.0))
            .into_iter()
            .filter(|nearby| {
                nearby.get_entity().entity_type == &EntityType::HOGLIN
                    && nearby.get_entity().entity_id != entity.entity_id
                    && nearby.get_mob().is_some_and(|mob| !mob.is_baby_mob())
                    && self.has_line_of_sight(nearby.get_entity())
            })
            .collect()
    }

    fn broadcast_attack_target(&self, target: Arc<dyn EntityBase>) {
        for nearby in self.visible_adult_hoglins() {
            let Some(hoglin) = nearby.get_mob() else {
                continue;
            };
            if hoglin.is_pacified() {
                continue;
            }
            let nearby_pos = nearby.get_entity().pos.load();
            let new_distance = nearby_pos.squared_distance_to_vec(&target.get_entity().pos.load());
            let closer = hoglin
                .get_mob_entity()
                .get_target()
                .as_ref()
                .is_none_or(|current| {
                    new_distance
                        < nearby_pos.squared_distance_to_vec(&current.get_entity().pos.load())
                });
            if closer {
                hoglin.set_temporary_attack_target(
                    target.clone(),
                    200,
                    crate::plugin::api::events::entity::entity_target::TargetReason::TargetAttackedNearbyEntity,
                );
            }
        }
    }

    fn broadcast_retreat(&self, target: Arc<dyn EntityBase>) {
        for nearby in self.visible_adult_hoglins() {
            if let Some(hoglin) = nearby.get_mob() {
                hoglin.begin_avoiding_entity(target.clone());
            }
        }
    }

    fn scan_repellent(&self) {
        let entity = self.get_entity();
        let center = entity.block_pos.load();
        let world = entity.world.load();
        let mut nearest = None;
        let mut nearest_distance = i32::MAX;
        for dy in -4..=4 {
            for dx in -8..=8 {
                for dz in -8..=8 {
                    let distance = dx * dx + dy * dy + dz * dz;
                    if distance >= nearest_distance {
                        continue;
                    }
                    let pos = BlockPos::new(center.0.x + dx, center.0.y + dy, center.0.z + dz);
                    if world
                        .get_block(&pos)
                        .has_tag(&tag::Block::MINECRAFT_HOGLIN_REPELLENTS)
                    {
                        nearest = Some(pos);
                        nearest_distance = distance;
                    }
                }
            }
        }
        self.nearest_repellent.store(nearest);
        if nearest.is_none() {
            return;
        }
        self.pacified_ticks.store(200, Ordering::Relaxed);
        self.set_mob_target(
            None,
            crate::plugin::api::events::entity::entity_target::TargetReason::ForgotTarget,
        );
    }

    fn visible_adult_counts(&self) -> (usize, usize) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let area = entity.bounding_box.load().expand(16.0, 8.0, 16.0);
        let mut piglins = 0;
        let mut hoglins = 1;
        for nearby in world.get_entities_at_box(&area) {
            let Some(mob) = nearby.get_mob() else {
                continue;
            };
            if mob.is_baby_mob() || !self.has_line_of_sight(nearby.get_entity()) {
                continue;
            }
            if nearby.get_entity().entity_type == &EntityType::PIGLIN
                || nearby.get_entity().entity_type == &EntityType::PIGLIN_BRUTE
            {
                piglins += 1;
            } else if nearby.get_entity().entity_type == &EntityType::HOGLIN
                && nearby.get_entity().entity_id != entity.entity_id
            {
                hoglins += 1;
            }
        }
        (piglins, hoglins)
    }

    fn convert_to_zoglin(&self) {
        self.make_sound(Sound::EntityHoglinConvertedToZombified);
        let _ = convert_single(self, &EntityType::ZOGLIN, true, false, |zoglin| {
            if let Some(living) = zoglin.get_living_entity() {
                living.add_effect(Effect {
                    effect_type: &StatusEffect::NAUSEA,
                    duration: 200,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                });
            }
        });
    }
}

impl AgeableMob for HoglinEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn age_boundary_reached(&self) {
        let baby = self.is_baby();
        let living = &self.mob_entity.living_entity;
        living.entity.set_default_dimensions(if baby {
            Self::BABY_DIMENSIONS
        } else {
            Entity::type_dimensions(living.entity.entity_type)
        });
        living.set_attribute_base(&Attributes::ATTACK_DAMAGE, if baby { 0.5 } else { 6.0 });
    }
}

impl Animal for HoglinEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_HOGLIN_FOOD)
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let offspring = crate::entity::r#type::from_type(
            &EntityType::HOGLIN,
            entity.pos.load(),
            &entity.world.load(),
            uuid::Uuid::new_v4(),
        );
        if let Some(mob) = offspring.get_mob() {
            mob.get_mob_entity()
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        offspring
    }
}

impl CustomSound for HoglinEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityHoglinDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityHoglinHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl Mob for HoglinEntity {
    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn is_baby_mob(&self) -> bool {
        self.is_baby()
    }

    fn is_pacified(&self) -> bool {
        self.pacified_ticks.load(Ordering::Relaxed) > 0
    }

    fn can_fall_in_love(&self) -> bool {
        !self.is_pacified()
    }

    fn can_be_hunted(&self) -> bool {
        !self.is_baby() && !self.cannot_be_hunted.load(Ordering::Relaxed)
    }

    fn set_temporary_attack_target(
        &self,
        target: Arc<dyn EntityBase>,
        ticks: i32,
        reason: crate::plugin::api::events::entity::entity_target::TargetReason,
    ) {
        self.temporary_attack_target_id
            .store(target.get_entity().entity_id, Ordering::Relaxed);
        self.attack_target_ticks.store(ticks, Ordering::Relaxed);
        self.set_mob_target(Some(target), reason);
    }

    fn begin_avoiding_entity(&self, target: Arc<dyn EntityBase>) {
        self.begin_avoidance(target);
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(
            if self.avoid_ticks.load(Ordering::Relaxed) > 0
                || self.is_converting(&self.get_entity().world.load())
            {
                Sound::EntityHoglinRetreat
            } else if self.mob_entity.get_target().is_some() {
                Sound::EntityHoglinAngry
            } else if self.nearest_repellent.load().is_some() {
                Sound::EntityHoglinRetreat
            } else {
                Sound::EntityHoglinAmbient
            },
        )
    }

    fn outgoing_attack_damage(&self, base_damage: f32) -> f32 {
        if self.is_baby() || base_damage < 1.0 {
            base_damage
        } else {
            base_damage / 2.0 + rand::random_range(0..base_damage as i32) as f32
        }
    }

    fn pre_attack(&self, _target: &dyn EntityBase) {
        self.attack_animation_remaining_ticks
            .store(10, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.world.load().send_entity_status(
            entity,
            EntityStatus::StartAttacking,
            pumpkin_protocol::bedrock::server::ActorEventID::StartAttacking,
        );
        self.make_sound(Sound::EntityHoglinAttack);
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityHoglinStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn spawn_as_baby(&self) -> bool {
        self.set_baby(true);
        true
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if rand::random::<f32>() < 0.2 {
            self.set_baby(true);
        }
        self.finalize_spawn_super(group_data)
    }

    fn get_base_experience_reward(&self) -> u32 {
        if self.is_baby() {
            Self::BABY_XP_REWARD
        } else {
            Self::XP_REWARD
        }
    }

    fn should_drop_loot(&self) -> bool {
        self.mob_entity.living_entity.should_drop_loot()
    }

    fn should_drop_experience(&self) -> bool {
        true
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        self.write_animal_nbt(nbt);
        if self.is_immune_to_zombification() {
            nbt.put_bool("IsImmuneToZombification", true);
        }
        let time = self.time_in_overworld.load(Ordering::Relaxed);
        if time > 0 {
            nbt.put_int("TimeInOverworld", time);
        }
        if self.cannot_be_hunted.load(Ordering::Relaxed) {
            nbt.put_bool("CannotBeHunted", true);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        self.read_animal_nbt(nbt);
        if let Some(immune) = nbt.get_bool("IsImmuneToZombification") {
            self.set_immune_to_zombification(immune);
        }
        if let Some(time) = nbt.get_int("TimeInOverworld") {
            self.time_in_overworld.store(time, Ordering::Relaxed);
        }
        if let Some(cannot_hunt) = nbt.get_bool("CannotBeHunted") {
            self.cannot_be_hunted.store(cannot_hunt, Ordering::Relaxed);
        }
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        if !entity.is_alive() {
            return;
        }

        self.attack_animation_remaining_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |ticks| {
                (ticks > 0).then_some(ticks - 1)
            })
            .ok();
        self.pacified_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |ticks| {
                (ticks > 0).then_some(ticks - 1)
            })
            .ok();
        let avoid_ticks = self.avoid_ticks.load(Ordering::Relaxed);
        if avoid_ticks > 0 {
            self.avoid_ticks.fetch_sub(1, Ordering::Relaxed);
        } else {
            self.avoid_target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        let attack_ticks = self.attack_target_ticks.load(Ordering::Relaxed);
        if attack_ticks > 0
            && self.attack_target_ticks.fetch_sub(1, Ordering::Relaxed) == 1
            && self.mob_entity.get_target().as_ref().is_some_and(|target| {
                target.get_entity().entity_id
                    == self.temporary_attack_target_id.load(Ordering::Relaxed)
            })
        {
            self.set_mob_target(
                None,
                crate::plugin::api::events::entity::entity_target::TargetReason::ForgotTarget,
            );
        }
        if entity.tick_count.load(Ordering::Relaxed) % 20 == 0 {
            self.scan_repellent();
            if !self.is_baby() && self.avoid_ticks.load(Ordering::Relaxed) > 0 {
                let (piglins, hoglins) = self.visible_adult_counts();
                if piglins <= hoglins {
                    self.avoid_ticks.store(0, Ordering::Relaxed);
                }
            }
        }

        let world = entity.world.load();
        let fighting = self.mob_entity.get_target().is_some();
        let avoiding = self.avoid_ticks.load(Ordering::Relaxed) > 0;
        let was_fighting = self.was_fighting.swap(fighting, Ordering::Relaxed);
        let was_avoiding = self.was_avoiding.swap(avoiding, Ordering::Relaxed);
        let activity_changed = fighting != was_fighting || avoiding != was_avoiding;
        if activity_changed {
            self.make_sound(if fighting {
                Sound::EntityHoglinAngry
            } else if avoiding {
                Sound::EntityHoglinRetreat
            } else {
                Sound::EntityHoglinAmbient
            });
        }
        if self.is_converting(&world) {
            let time = self.time_in_overworld.fetch_add(1, Ordering::Relaxed) + 1;
            if time > Self::CONVERSION_TIME {
                self.convert_to_zoglin();
            }
        } else {
            self.time_in_overworld.store(0, Ordering::Relaxed);
        }
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        if !self.is_baby() {
            if target.get_entity().entity_type == &EntityType::PIGLIN {
                let (piglins, hoglins) = self.visible_adult_counts();
                if piglins > hoglins {
                    let world = self.get_entity().world.load();
                    if let Some(target) = crate::entity::mob::neutral::find_by_uuid(
                        &world,
                        target.get_entity().entity_uuid,
                    ) {
                        self.begin_avoidance(target.clone());
                        self.broadcast_retreat(target);
                    }
                } else {
                    let world = self.get_entity().world.load();
                    if let Some(target) = crate::entity::mob::neutral::find_by_uuid(
                        &world,
                        target.get_entity().entity_uuid,
                    ) {
                        self.broadcast_attack_target(target);
                    }
                }
            } else {
                let world = self.get_entity().world.load();
                if let Some(target) = crate::entity::mob::neutral::find_by_uuid(
                    &world,
                    target.get_entity().entity_uuid,
                ) {
                    self.broadcast_attack_target(target);
                }
            }
            throw_target(&self.mob_entity.living_entity, target);
        }
    }

    fn blocked_by_item(&self, defender: &dyn EntityBase) {
        if !self.is_baby() {
            throw_target(&self.mob_entity.living_entity, defender);
        }
    }

    fn on_damage_with_context(
        &self,
        _damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let Some(attacker) = cause.or(source) else {
            return true;
        };
        self.pacified_ticks.store(0, Ordering::Relaxed);
        self.mob_entity.set_love_ticks(0, None);
        let world = self.get_entity().world.load();
        let Some(attacker) =
            crate::entity::mob::neutral::find_by_uuid(&world, attacker.get_entity().entity_uuid)
        else {
            return true;
        };
        if self.is_baby() {
            self.begin_avoidance(attacker);
            return true;
        }
        if attacker.get_entity().entity_type == &EntityType::PIGLIN
            && self.avoid_ticks.load(Ordering::Relaxed) > 0
        {
            return true;
        }
        if attacker.get_entity().entity_type == &EntityType::HOGLIN
            || !self.can_attack(attacker.as_ref())
        {
            return true;
        }
        let entity_pos = self.get_entity().pos.load();
        let new_distance = entity_pos.squared_distance_to_vec(&attacker.get_entity().pos.load());
        let closer = self.mob_entity.get_target().as_ref().is_none_or(|current| {
            new_distance.sqrt()
                <= entity_pos
                    .squared_distance_to_vec(&current.get_entity().pos.load())
                    .sqrt()
                    + 4.0
        });
        if closer {
            self.set_temporary_attack_target(
                attacker.clone(),
                200,
                crate::plugin::api::events::entity::entity_target::TargetReason::TargetAttackedEntity,
            );
            self.broadcast_attack_target(attacker);
        }
        true
    }

    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        if self
            .nearest_repellent
            .load()
            .is_some_and(|repellent| repellent.squared_distance(pos) < 64)
        {
            -1.0
        } else if self.get_entity().world.load().get_block(&pos.down()) == &Block::CRIMSON_NYLIUM {
            10.0
        } else {
            0.0
        }
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        true
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let succeeded = self.animal_interact(player, item_stack, Sound::EntityHoglinAmbient);
        if succeeded {
            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        succeeded
    }
}

/// Vanilla `HoglinBase.throwTarget`, shared with zoglins.
pub fn throw_target(attacker: &crate::entity::living::LivingEntity, target: &dyn EntityBase) {
    let attacker_entity = &attacker.entity;
    let Some(target_living) = target.get_living_entity() else {
        return;
    };
    let effective_power = attacker.get_attribute_value(&Attributes::ATTACK_KNOCKBACK)
        - target_living.get_attribute_value(&Attributes::KNOCKBACK_RESISTANCE);
    if effective_power <= 0.0 {
        return;
    }
    let my_pos = attacker_entity.pos.load();
    let target_pos = target.get_entity().pos.load();
    let dx = target_pos.x - my_pos.x;
    let dz = target_pos.z - my_pos.z;
    let dist = dx.hypot(dz).max(0.001);
    let angle = f64::from(rand::random_range(-10..=10));
    let scale = effective_power * f64::from(rand::random::<f32>().mul_add(0.5, 0.2));
    let base_x = dx / dist * scale;
    let base_z = dz / dist * scale;
    let push_x = base_x.mul_add(angle.cos(), base_z * angle.sin());
    let push_z = (-base_x).mul_add(angle.sin(), base_z * angle.cos());
    let push_y = effective_power * f64::from(rand::random::<f32>()) * 0.5;
    let vel = target.get_entity().velocity.load();
    target.get_entity().velocity.store(Vector3::new(
        vel.x + push_x,
        vel.y + push_y,
        vel.z + push_z,
    ));
}
