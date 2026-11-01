use std::sync::{Arc, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::Sound;
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::behavior::{fleeing, hunting};
use crate::entity::mob::spider::{SPIDER_FLEES, SPIDER_HUNTS};
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        leap_at_target::LeapAtTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, revenge::RevengeGoal, spider_attack::SpiderAttackGoal,
        swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

pub struct CaveSpiderEntity {
    pub mob_entity: MobEntity,
    climbing: std::sync::atomic::AtomicBool,
}

impl CaveSpiderEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        // spider: wall climber navigation (Spider.createNavigation)
        *mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            crate::entity::ai::pathfinder::Navigator::wall_climber();
        let cave_spider = Self {
            mob_entity,
            climbing: std::sync::atomic::AtomicBool::new(false),
        };
        let mob_arc = Arc::new(cave_spider);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            fleeing::apply(goal_selector, SPIDER_FLEES);
            // spider: leap 3, attack 4 (Spider.registerGoals)
            goal_selector.add_goal(3, Box::new(LeapAtTargetGoal::new(0.4)));
            goal_selector.add_goal(4, SpiderAttackGoal::new(1.0, true));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::water_avoiding(0.8)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(6, Box::new(RandomLookAroundGoal::default()));

            target_selector.add_goal(1, Box::new(RevengeGoal::new(true)));
            hunting::apply(target_selector, &mob_arc.mob_entity, SPIDER_HUNTS);
        };

        mob_arc
    }
}

impl Mob for CaveSpiderEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntitySpiderStep, 0.15, 1.0);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<crate::world::World>,
        group_data: Option<super::spawn::SpawnGroupData>,
    ) -> Option<super::spawn::SpawnGroupData> {
        // cave spider: no jockey, no effects, not even Mob.finalizeSpawn (CaveSpider.finalizeSpawn)
        let _ = world;
        group_data
    }

    // cave spider: climbs like a spider (Spider.tick)
    fn post_tick(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        let climbing = entity
            .horizontal_collision
            .load(std::sync::atomic::Ordering::Relaxed);
        if self
            .climbing
            .swap(climbing, std::sync::atomic::Ordering::Relaxed)
            != climbing
        {
            entity.set_synced_data(
                pumpkin_data::tracked_data::spider::DATA_FLAGS_ID,
                i8::from(climbing),
            );
        }
    }

    fn climbable_override(&self) -> Option<bool> {
        Some(self.climbing.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        if let Some(living) = target.get_living_entity() {
            let world = self.mob_entity.living_entity.entity.world.load();
            let duration = match world.level_info.load().difficulty {
                pumpkin_util::Difficulty::Normal => 7 * 20,
                pumpkin_util::Difficulty::Hard => 15 * 20,
                _ => 0,
            };
            if duration > 0 {
                living.add_effect(Effect {
                    effect_type: &StatusEffect::POISON,
                    duration,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                });
            }
        }
    }
}
