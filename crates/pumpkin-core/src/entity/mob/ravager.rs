use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::world::BlockFlags;

use crate::entity::ageable::AgeableMob;
use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{
        Mob, MobEntity,
        patrol::{LongDistancePatrolGoal, PatrolData, PatrollingMonster},
        raider::{
            self, ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider, RaiderCelebrationGoal,
            RaiderData, RaiderMoveThroughVillageGoal,
        },
    },
};
use crate::world::game_event::GameEventContext;

const RAVAGER_HUNTS: &[Hunt] = &[
    Hunt::new(3, TargetMatch::Type(&EntityType::PLAYER), true),
    Hunt::new(4, TargetMatch::VILLAGER_LIKE, true).only_mobs(is_adult),
    Hunt::new(4, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
];

fn is_adult(mob: &dyn Mob) -> bool {
    !mob.as_ageable().is_some_and(AgeableMob::is_baby)
}

const BASE_MOVEMENT_SPEED: f64 = 0.3;
const ATTACK_MOVEMENT_SPEED: f64 = 0.35;
const ATTACK_DURATION: i32 = 10;
const STUNNED_DURATION: i32 = 40;
const ROAR_DURATION: i32 = 20;
const ROAR_AT_TICK: i32 = 10;
const ROAR_RADIUS: f64 = 4.0;
const ROAR_DAMAGE: f32 = 6.0;
const MAX_HEAD_Y_ROT: f32 = 45.0;

pub struct RavagerEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    attack_tick: AtomicI32,
    stunned_tick: AtomicI32,
    roar_tick: AtomicI32,
}

impl RavagerEntity {
    #[must_use]
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let ravager = Self {
            mob_entity,
            raider_data: RaiderData::default(),
            attack_tick: AtomicI32::new(0),
            stunned_tick: AtomicI32::new(0),
            roar_tick: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(ravager);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(4, Box::new(LongDistancePatrolGoal::new(0.7, 0.595)));
            goal_selector.add_goal(1, Box::new(ObtainRaidLeaderBannerGoal));
            goal_selector.add_goal(3, Box::new(PathfindToRaidGoal::default()));
            goal_selector.add_goal(4, Box::new(RaiderMoveThroughVillageGoal::new(1.05)));
            goal_selector.add_goal(5, Box::new(RaiderCelebrationGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(4, Box::new(MeleeAttackGoal::new(1.0, true)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::water_avoiding(0.4)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(10, LookAtEntityGoal::any_mob(mob_weak, 8.0, 0.02));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(2, raider::revenge_goal());
            hunting::apply(target_selector, &mob_arc.mob_entity, RAVAGER_HUNTS);
        };

        mob_arc
    }

    fn is_busy(&self) -> bool {
        self.attack_tick.load(Ordering::Relaxed) > 0
            || self.stunned_tick.load(Ordering::Relaxed) > 0
            || self.roar_tick.load(Ordering::Relaxed) > 0
    }

    // ravager: knock entity away (Ravager.strongKnockback)
    fn strong_knockback(&self, entity: &Entity) {
        let pos = self.get_entity().pos.load();
        let other = entity.pos.load();
        let dx = other.x - pos.x;
        let dz = other.z - pos.z;
        let dd = (dx * dx + dz * dz).max(0.001);
        entity.add_velocity(Vector3::new(dx / dd * 4.0, 0.2, dz / dd * 4.0));
    }

    // ravager: roar damage + knockback in 4 blocks (Ravager.roar)
    fn roar(&self) {
        let entity = self.get_entity();
        if !entity.is_alive() {
            return;
        }
        let world = entity.world.load();
        let griefing = world.level_info.load().game_rules.mob_griefing;
        let area = entity.bounding_box.load().expand_all(ROAR_RADIUS);
        for other in world.get_entities_at_box(&area) {
            let other_entity = other.get_entity();
            let Some(living) = other.get_living_entity() else {
                continue;
            };
            if other_entity.entity_type == &EntityType::RAVAGER
                || !living.is_alive()
                || (!griefing && other_entity.entity_type == &EntityType::ARMOR_STAND)
            {
                continue;
            }
            if !other_entity
                .entity_type
                .has_tag(&tag::EntityType::MINECRAFT_ILLAGER)
            {
                other.damage_with_context(
                    other.as_ref(),
                    ROAR_DAMAGE,
                    DamageType::MOB_ATTACK,
                    None,
                    Some(self),
                    Some(self),
                );
            }
            if other.get_player().is_none() {
                self.strong_knockback(other_entity);
            }
        }
        world.emit_game_event_with_context(
            GameEvent::EntityAction.name(),
            entity.pos.load(),
            &GameEventContext {
                source_entity: Some(self),
            },
        );
        world.send_entity_status(entity, EntityStatus::RavagerRoared, ActorEventID::None);
    }

    // ravager: smash leaves on collision, else hop (Ravager.aiStep)
    fn break_leaves(&self) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        if !entity.horizontal_collision.load(Ordering::Relaxed)
            || !world.level_info.load().game_rules.mob_griefing
        {
            return;
        }
        let area = entity.bounding_box.load().expand_all(0.2);
        let mut destroyed = false;
        for pos in BlockPos::iterate(area.min_block_pos(), area.max_block_pos()) {
            if world.get_block(&pos).has_tag(&tag::Block::MINECRAFT_LEAVES) {
                destroyed |= world
                    .break_block(&pos, None, BlockFlags::NOTIFY_ALL)
                    .is_some();
            }
        }
        if !destroyed && entity.on_ground.load(Ordering::Relaxed) {
            self.mob_entity.living_entity.jump();
        }
    }
}

impl Mob for RavagerEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityRavagerStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn as_patrolling_monster(&self) -> Option<&dyn PatrollingMonster> {
        Some(self)
    }

    fn as_raider(&self) -> Option<&dyn Raider> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_raider_nbt(nbt);
        nbt.put_int("AttackTick", self.attack_tick.load(Ordering::Relaxed));
        nbt.put_int("StunTick", self.stunned_tick.load(Ordering::Relaxed));
        nbt.put_int("RoarTick", self.roar_tick.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_raider_nbt(nbt);
        self.attack_tick
            .store(nbt.get_int("AttackTick").unwrap_or(0), Ordering::Relaxed);
        self.stunned_tick
            .store(nbt.get_int("StunTick").unwrap_or(0), Ordering::Relaxed);
        self.roar_tick
            .store(nbt.get_int("RoarTick").unwrap_or(0), Ordering::Relaxed);
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityRavagerAmbient)
    }

    fn get_max_head_rotation(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    // ravager: frozen while attacking, stunned or roaring (Ravager.isImmobile)
    fn is_mob_immobile(&self) -> bool {
        self.mob_entity.living_entity.is_dead_or_dying() || self.is_busy()
    }

    // ravager: blind while stunned or roaring (Ravager.hasLineOfSight)
    fn has_line_of_sight(&self, target: &Entity) -> bool {
        if self.stunned_tick.load(Ordering::Relaxed) > 0
            || self.roar_tick.load(Ordering::Relaxed) > 0
        {
            return false;
        }
        self.mob_entity
            .sensing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_line_of_sight(&self.mob_entity.living_entity.entity, target)
    }

    // ravager: attack cooldown + sound on every attempt (Ravager.doHurtTarget)
    fn pre_attack(&self, _target: &dyn EntityBase) {
        self.attack_tick.store(ATTACK_DURATION, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.world.load().send_entity_status(
            entity,
            EntityStatus::StartAttacking,
            ActorEventID::None,
        );
        self.play_entity_sound(Sound::EntityRavagerAttack, 1.0, 1.0);
    }

    // ravager: shield block -> 50% stun 40 ticks, else strong knockback (Ravager.blockedByItem)
    fn blocked_by_item(&self, defender: &dyn EntityBase) {
        if self.roar_tick.load(Ordering::Relaxed) != 0 {
            return;
        }
        if rand::random::<f64>() < 0.5 {
            self.stunned_tick.store(STUNNED_DURATION, Ordering::Relaxed);
            self.play_entity_sound(Sound::EntityRavagerStunned, 1.0, 1.0);
            let entity = self.get_entity();
            entity.world.load().send_entity_status(
                entity,
                EntityStatus::RavagerStunned,
                ActorEventID::None,
            );
            defender.push(self);
        } else {
            self.strong_knockback(defender.get_entity());
        }
        defender.get_entity().send_velocity();
    }

    // ravager: speed, leaves, roar/attack/stun timers (Ravager.aiStep)
    fn mob_ai_step(&self) {
        let living = &self.mob_entity.living_entity;
        if !living.entity.is_alive() {
            return;
        }
        if self.is_mob_immobile() {
            living.set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.0);
        } else {
            let max_speed = if self.mob_entity.get_target().is_some() {
                ATTACK_MOVEMENT_SPEED
            } else {
                BASE_MOVEMENT_SPEED
            };
            let base = living.get_attribute_base(&Attributes::MOVEMENT_SPEED);
            living.set_attribute_base(&Attributes::MOVEMENT_SPEED, base + (max_speed - base) * 0.1);
        }

        self.break_leaves();

        if self.roar_tick.load(Ordering::Relaxed) > 0
            && self.roar_tick.fetch_sub(1, Ordering::Relaxed) - 1 == ROAR_AT_TICK
        {
            self.roar();
        }
        if self.attack_tick.load(Ordering::Relaxed) > 0 {
            self.attack_tick.fetch_sub(1, Ordering::Relaxed);
        }
        if self.stunned_tick.load(Ordering::Relaxed) > 0
            && self.stunned_tick.fetch_sub(1, Ordering::Relaxed) - 1 == 0
        {
            self.play_entity_sound(Sound::EntityRavagerRoar, 1.0, 1.0);
            self.roar_tick.store(ROAR_DURATION, Ordering::Relaxed);
        }
    }
}

impl PatrollingMonster for RavagerEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }

    fn can_be_leader(&self) -> bool {
        false
    }
}

impl Raider for RavagerEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityRavagerCelebrate
    }
}
