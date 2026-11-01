use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::position::BlockPos;

use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        active_target::ActiveTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, revenge::RevengeGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    living::LivingEntity,
    mob::{Mob, MobEntity, MobMetadata, hoglin::throw_target},
};
use crate::world::World;

pub struct ZoglinEntity {
    pub mob_entity: MobEntity,
    pub is_baby: AtomicBool,
    attack_animation_remaining_ticks: AtomicI32,
    was_fighting: AtomicBool,
    retaliation_ticks: AtomicI32,
    retaliation_target_id: AtomicI32,
}

fn is_adult(mob: &dyn Mob) -> bool {
    !mob.is_baby_mob()
}

fn is_baby(mob: &dyn Mob) -> bool {
    mob.is_baby_mob()
}

impl ZoglinEntity {
    pub const XP_REWARD: u32 = 5;
    const BABY_ATTACK_DAMAGE: f64 = 0.5;

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

        let zoglin = Self {
            mob_entity,
            is_baby: AtomicBool::new(false),
            attack_animation_remaining_ticks: AtomicI32::new(0),
            was_fighting: AtomicBool::new(false),
            retaliation_ticks: AtomicI32::new(0),
            retaliation_target_id: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(zoglin);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(
                4,
                Box::new(
                    MeleeAttackGoal::new(1.0, true)
                        .with_interval(40)
                        .gated_by(is_adult),
                ),
            );
            goal_selector.add_goal(
                4,
                Box::new(
                    MeleeAttackGoal::new(1.0, true)
                        .with_interval(15)
                        .gated_by(is_baby),
                ),
            );
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(0.4)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).forget_unseen_after(200)));
            target_selector.add_goal(
                2,
                Box::new(ActiveTargetGoal::new(
                    &mob_arc.mob_entity,
                    crate::entity::ai::target_match::TargetMatch::Any,
                    10,
                    true,
                    false,
                    Some(|target: &LivingEntity, _world: &World| {
                        target.entity.entity_type != &EntityType::ZOGLIN
                            && target.entity.entity_type != &EntityType::CREEPER
                    }),
                )),
            );
        };

        mob_arc
    }

    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.is_baby.load(Ordering::Relaxed)
    }

    /// Vanilla `Zoglin.setBaby`; growing up keeps the lowered attack damage, as in vanilla.
    pub fn set_baby(&self, baby: bool) {
        self.mob_entity
            .set_baby_flag(&self.is_baby, tracked_data::zoglin::DATA_BABY_ID, baby);
        let living = &self.mob_entity.living_entity;
        living.entity.set_default_dimensions(if baby {
            Self::BABY_DIMENSIONS
        } else {
            Entity::type_dimensions(living.entity.entity_type)
        });
        if baby {
            living.set_attribute_base(&Attributes::ATTACK_DAMAGE, Self::BABY_ATTACK_DAMAGE);
        }
    }
}

impl CustomSound for ZoglinEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZoglinDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityZoglinHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl Mob for ZoglinEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.mob_entity.get_target().is_some() {
            Sound::EntityZoglinAngry
        } else {
            Sound::EntityZoglinAmbient
        })
    }

    fn is_baby_mob(&self) -> bool {
        self.is_baby()
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
        self.make_sound(Sound::EntityZoglinAttack);
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.attack_animation_remaining_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |ticks| {
                (ticks > 0).then_some(ticks - 1)
            })
            .ok();
        let fighting = self.mob_entity.get_target().is_some();
        if fighting && !self.was_fighting.swap(fighting, Ordering::Relaxed) {
            self.make_sound(Sound::EntityZoglinAngry);
        } else if !fighting {
            self.was_fighting.store(false, Ordering::Relaxed);
        }
        let retaliation_ticks = self.retaliation_ticks.load(Ordering::Relaxed);
        if retaliation_ticks > 0
            && self.retaliation_ticks.fetch_sub(1, Ordering::Relaxed) == 1
            && self.mob_entity.get_target().as_ref().is_some_and(|target| {
                target.get_entity().entity_id == self.retaliation_target_id.load(Ordering::Relaxed)
            })
        {
            self.set_mob_target(
                None,
                crate::plugin::api::events::entity::entity_target::TargetReason::ForgotTarget,
            );
        }
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityZoglinStep, 0.15, 1.0);
    }

    fn mob_metadata(&self) -> Option<MobMetadata> {
        self.is_baby().then(|| MobMetadata::new().baby(true))
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<crate::entity::mob::spawn::SpawnGroupData>,
    ) -> Option<crate::entity::mob::spawn::SpawnGroupData> {
        if rand::random::<f32>() < 0.2 {
            self.set_baby(true);
        }
        self.mob_entity.finalize_spawn_base();
        group_data
    }

    fn spawn_as_baby(&self) -> bool {
        self.set_baby(true);
        true
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("IsBaby", self.is_baby());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_baby(nbt.get_bool("IsBaby").unwrap_or(false));
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        // Vanilla HoglinBase.hurtAndThrowTarget: babies don't throw.
        if self.is_baby() {
            return;
        }
        throw_target(&self.mob_entity.living_entity, target);
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
        if !self.can_attack(attacker) {
            return true;
        }
        let entity_pos = self.get_entity().pos.load();
        let attacker_distance = entity_pos
            .squared_distance_to_vec(&attacker.get_entity().pos.load())
            .sqrt();
        let close_enough = self.mob_entity.get_target().as_ref().is_none_or(|current| {
            attacker_distance
                <= entity_pos
                    .squared_distance_to_vec(&current.get_entity().pos.load())
                    .sqrt()
                    + 4.0
        });
        if close_enough {
            let world = self.get_entity().world.load();
            if let Some(attacker) =
                crate::entity::mob::neutral::find_by_uuid(&world, attacker.get_entity().entity_uuid)
            {
                self.retaliation_target_id
                    .store(attacker.get_entity().entity_id, Ordering::Relaxed);
                self.retaliation_ticks.store(200, Ordering::Relaxed);
                self.set_mob_target(
                    Some(attacker),
                    crate::plugin::api::events::entity::entity_target::TargetReason::TargetAttackedEntity,
                );
            }
        }
        true
    }
}
