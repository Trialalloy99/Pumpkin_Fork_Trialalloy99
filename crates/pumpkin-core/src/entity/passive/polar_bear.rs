use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{
    sound::Sound,
    tag::{self, Taggable},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::behavior::neutral::apply_targets,
    ai::goal::{
        active_target::{ActiveTargetGoal, TargetCondition},
        escape_danger::EscapeDangerGoal,
        follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal,
        revenge::RevengeGoal,
        swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{
        Mob, MobEntity,
        neutral::{NeutralData, NeutralMob},
    },
    passive::animal::Animal,
    player::Player,
};

fn adult_with_cub_nearby(mob: &dyn Mob) -> bool {
    if mob.as_ageable().is_some_and(AgeableMob::is_baby) {
        return false;
    }
    let entity = mob.get_entity();
    entity
        .world
        .load()
        .get_entities_at_box(&entity.bounding_box.load().expand(8.0, 4.0, 8.0))
        .iter()
        .any(|other| {
            other.get_entity().entity_type == &EntityType::POLAR_BEAR
                && other
                    .get_mob()
                    .and_then(Mob::as_ageable)
                    .is_some_and(AgeableMob::is_baby)
        })
}

fn polar_bear_should_panic(mob: &dyn Mob) -> bool {
    mob.get_mob_entity()
        .living_entity
        .get_last_damage_type()
        .is_some_and(|damage_type| {
            damage_type.has_tag(if mob.as_ageable().is_some_and(AgeableMob::is_baby) {
                &tag::DamageType::MINECRAFT_PANIC_CAUSES
            } else {
                &tag::DamageType::MINECRAFT_PANIC_ENVIRONMENTAL_CAUSES
            })
        })
}

fn is_baby(mob: &dyn Mob) -> bool {
    mob.as_ageable().is_some_and(AgeableMob::is_baby)
}

const POLAR_BEAR_HUNTS: &[Hunt] =
    &[Hunt::new(4, TargetMatch::Type(&EntityType::FOX), true).when(TargetCondition::Adult)];

pub struct PolarBearEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub neutral_data: NeutralData,
    pub standing: AtomicBool,
    warning_sound_ticks: AtomicI32,
}

impl PolarBearEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let polar_bear = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            neutral_data: NeutralData::default(),
            standing: AtomicBool::new(false),
            warning_sound_ticks: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(polar_bear);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(MeleeAttackGoal::new(1.25, true)));
            goal_selector.add_goal(
                1,
                EscapeDangerGoal::new(2.0).gated_by(polar_bear_should_panic),
            );
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.25)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;

            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .raising_alarm_when(is_baby)
                        .alerting_only(|other| !is_baby(other)),
                ),
            );
            target_selector.add_goal(
                2,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::PLAYER, true)
                    .every(20)
                    .scale_range(0.5)
                    .start_gated_by(adult_with_cub_nearby),
            );
            apply_targets(target_selector, &mob_arc.mob_entity, 3, 5, false);
            hunting::apply(target_selector, &mob_arc.mob_entity, POLAR_BEAR_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn is_standing(&self) -> bool {
        self.standing.load(Ordering::Relaxed)
    }

    pub fn set_standing(&self, standing: bool) {
        self.standing.store(standing, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::polar_bear::DATA_STANDING_ID,
            standing,
        );
    }

    fn play_warning_sound(&self) {
        if self.warning_sound_ticks.load(Ordering::Relaxed) <= 0 {
            self.make_sound(Sound::EntityPolarBearWarning);
            self.warning_sound_ticks.store(40, Ordering::Relaxed);
        }
    }
}

impl CustomSound for PolarBearEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPolarBearDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPolarBearHurt)
    }
}

impl AgeableMob for PolarBearEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::POLAR_BEAR_BABY_SPAWN_CHANCE
    }
}

impl Animal for PolarBearEntity {
    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }
}

impl NeutralMob for PolarBearEntity {
    fn get_neutral_data(&self) -> &NeutralData {
        &self.neutral_data
    }
}

impl Mob for PolarBearEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_baby() {
            Sound::EntityPolarBearAmbientBaby
        } else {
            Sound::EntityPolarBearAmbient
        })
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.warning_sound_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |ticks| {
                (ticks > 0).then_some(ticks - 1)
            })
            .ok();
    }

    fn melee_attack_cooldown_tick(
        &self,
        target: &dyn EntityBase,
        cooldown: i32,
        in_attack_range: bool,
        max_cooldown: i32,
    ) -> i32 {
        if in_attack_range {
            return cooldown;
        }
        let target_width = f64::from(target.get_entity().width());
        let close_range = (target_width + 3.0).powi(2);
        let distance = self
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&target.get_entity().pos.load());
        if distance >= close_range {
            self.set_standing(false);
            return max_cooldown;
        }

        let cooldown = if cooldown <= 0 {
            self.set_standing(false);
            max_cooldown
        } else {
            cooldown
        };
        if cooldown <= 10 {
            self.set_standing(true);
            self.play_warning_sound();
        }
        cooldown
    }

    fn melee_attack_stopped(&self) {
        self.set_standing(false);
    }

    fn pre_attack(&self, _target: &dyn EntityBase) {
        self.set_standing(false);
    }
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityPolarBearStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::polar_bear::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::polar_bear::DATA_STANDING_ID,
            self.is_standing(),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        self.animal_interact(player, item_stack, Sound::EntityPolarBearAmbient)
    }
}
