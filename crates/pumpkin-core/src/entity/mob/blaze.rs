use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_util::math::vector3::Vector3;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::goal::move_towards_restriction::MoveTowardsRestrictionGoal;
use crate::entity::ai::goal::revenge::RevengeGoal;
use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::ai::util::RandomExt;
use crate::entity::custom_sound::CustomSound;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

const BLAZE_HUNTS: &[Hunt] = &[Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true)];

pub struct BlazeEntity {
    pub entity: Arc<MobEntity>,
    pub is_charged: AtomicBool,
    allowed_height_offset: AtomicCell<f32>,
    next_height_offset_change_tick: AtomicI32,
}

impl BlazeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = Arc::new(MobEntity::new(entity));
        let blaze = Self {
            entity,
            is_charged: AtomicBool::new(false),
            allowed_height_offset: AtomicCell::new(0.5),
            next_height_offset_change_tick: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(blaze);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        {
            let goal_selector = &mob_arc.entity.goals_selector;
            let target_selector = &mob_arc.entity.target_selector;

            goal_selector.add_goal(
                4,
                Box::new(
                    crate::entity::ai::goal::blaze_attack::BlazeShootFireballGoal::new(
                        Arc::downgrade(&mob_arc),
                    ),
                ),
            );

            goal_selector.add_goal(5, Box::new(MoveTowardsRestrictionGoal::new(1.0)));
            goal_selector.add_goal(
                7,
                Box::new(WanderAroundGoal::water_avoiding_with_probability(1.0, 0.0)),
            );
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));

            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).alerting_others()));
            hunting::apply(target_selector, &mob_arc.entity, BLAZE_HUNTS);
        };

        {
            let mut navigator = mob_arc
                .entity
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            navigator.set_pathfinding_malus(PathType::Water, -1.0);
            navigator.set_pathfinding_malus(PathType::Lava, 8.0);
            navigator.set_pathfinding_malus(PathType::DangerFire, 0.0);
            navigator.set_pathfinding_malus(PathType::DamageFire, 0.0);
        };

        mob_arc
    }

    pub fn is_charged(&self) -> bool {
        self.is_charged.load(Ordering::Relaxed)
    }

    pub fn set_charged(&self, charged: bool) {
        self.is_charged.store(charged, Ordering::Relaxed);
        let flags = i8::from(charged);
        self.entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::blaze::DATA_FLAGS_ID, flags);
    }
}

impl Mob for BlazeEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBlazeAmbient)
    }

    fn mob_tick(&self, caller: &dyn EntityBase) {
        let base_entity = &self.entity.living_entity.entity;
        if !base_entity.is_alive() {
            return;
        }

        let on_ground = base_entity.on_ground.load(Ordering::Relaxed);
        let vel = base_entity.velocity.load();
        if !on_ground && vel.y < 0.0 {
            base_entity
                .velocity
                .store(Vector3::new(vel.x, vel.y * 0.6, vel.z));
        }

        if base_entity.touching_water.load(Ordering::Relaxed) {
            caller.damage(caller, 1.0, DamageType::DROWN);
        }
    }

    // blaze: hover toward targets above it (Blaze.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        if self
            .next_height_offset_change_tick
            .fetch_sub(1, Ordering::Relaxed)
            <= 0
        {
            self.next_height_offset_change_tick
                .store(100, Ordering::Relaxed);
            self.allowed_height_offset
                .store(rand::rng().triangle(0.5, 6.891) as f32);
        }
        let Some(target) = self.entity.get_target() else {
            return;
        };
        let entity = &self.entity.living_entity.entity;
        if target.get_entity().get_eye_pos().y
            > entity.get_eye_pos().y + f64::from(self.allowed_height_offset.load())
            && self.can_attack(target.as_ref())
        {
            let velocity = entity.velocity.load();
            entity
                .velocity
                .store(velocity.add_raw(0.0, (0.3 - velocity.y) * 0.3, 0.0));
        }
    }
}

impl CustomSound for BlazeEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBlazeDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityBlazeHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}
