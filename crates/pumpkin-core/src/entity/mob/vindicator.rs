use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;
use pumpkin_data::sound::Sound;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::Difficulty;
use rand::RngExt;

use crate::entity::ai::behavior::fleeing::{self, Flee};
use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        Controls, Goal, break_door::BreakDoorGoal, look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal, open_door::OpenDoorGoal, swim::SwimGoal, to_goal_ticks,
        wander_around::WanderAroundGoal,
    },
    mob::{
        Mob, MobEntity,
        patrol::{LongDistancePatrolGoal, PatrolData, PatrollingMonster},
        raider::{
            self, HoldGroundAttackGoal, ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider,
            RaiderCelebrationGoal, RaiderData, RaiderMoveThroughVillageGoal,
        },
    },
};
use crate::world::World;

const VINDICATOR_FLEES: &[Flee] = &[raider::flee_creaking(1, 1.0, 1.2)];

const VINDICATOR_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, true),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), true),
    Hunt::new(4, TargetMatch::Any, true)
        .every(0)
        .gated_by(is_johnny),
];

const JOHNNY_NAME: &str = "Johnny";
const DOOR_BREAK_TIME: i32 = 6;
const DOOR_BREAK_CHANCE: i32 = 10;

// vindicator: johnny targets every living thing (VindicatorJohnnyAttackGoal)
fn is_johnny(mob: &dyn Mob) -> bool {
    mob.cast_any()
        .downcast_ref::<VindicatorEntity>()
        .is_some_and(|vindicator| vindicator.johnny.load(Ordering::Relaxed))
}

pub struct VindicatorEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    johnny: AtomicBool,
}

impl VindicatorEntity {
    #[must_use]
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let vindicator = Self {
            mob_entity,
            raider_data: RaiderData::default(),
            johnny: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(vindicator);
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
            fleeing::apply(goal_selector, VINDICATOR_FLEES);
            goal_selector.add_goal(2, Box::new(VindicatorBreakDoorGoal::default()));
            goal_selector.add_goal(3, Box::new(RaiderOpenDoorGoal::default()));
            goal_selector.add_goal(4, Box::new(HoldGroundAttackGoal::new(10.0)));
            goal_selector.add_goal(5, Box::new(MeleeAttackGoal::new(1.0, false)));
            goal_selector.add_goal(8, Box::new(WanderAroundGoal::new(0.6)));
            goal_selector.add_goal(
                9,
                Box::new(LookAtEntityGoal::new(
                    mob_weak.clone(),
                    &EntityType::PLAYER,
                    3.0,
                    1.0,
                    false,
                )),
            );
            goal_selector.add_goal(10, LookAtEntityGoal::any_mob(mob_weak, 8.0, 0.02));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, raider::revenge_goal());
            hunting::apply(target_selector, &mob_arc.mob_entity, VINDICATOR_HUNTS);
        };

        mob_arc
    }
}

impl Mob for VindicatorEntity {
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
        if self.johnny.load(Ordering::Relaxed) {
            nbt.put_bool(JOHNNY_NAME, true);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_raider_nbt(nbt);
        self.johnny.store(
            nbt.get_bool(JOHNNY_NAME).unwrap_or(false),
            Ordering::Relaxed,
        );
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityVindicatorAmbient)
    }

    // vindicator: doors only while raided, johnny via name (Vindicator.customServerAiStep, setCustomName)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        if !self.johnny.load(Ordering::Relaxed)
            && entity
                .custom_name
                .load()
                .as_ref()
                .as_ref()
                .is_some_and(|name| name.clone().get_text() == JOHNNY_NAME)
        {
            self.johnny.store(true, Ordering::Relaxed);
        }
        if self.mob_entity.is_no_ai() {
            return;
        }
        let raided = entity
            .world
            .load()
            .raids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_raid_at(&entity.block_pos.load())
            .is_some();
        self.mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_open_doors(raided);
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.finalize_spawn_super(group_data);
        self.mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_can_open_doors(true);
        group_data
    }
}

// vindicator: break doors in raids on normal+ (Vindicator.VindicatorBreakDoorGoal)
struct VindicatorBreakDoorGoal {
    inner: BreakDoorGoal,
}

impl Default for VindicatorBreakDoorGoal {
    fn default() -> Self {
        Self {
            inner: BreakDoorGoal::with_door_break_time(
                DOOR_BREAK_TIME,
                Arc::new(|difficulty| {
                    difficulty == Difficulty::Normal || difficulty == Difficulty::Hard
                }),
            ),
        }
    }
}

fn mob_has_active_raid(mob: &dyn Mob) -> bool {
    mob.as_raider().is_some_and(Raider::has_active_raid)
}

impl Goal for VindicatorBreakDoorGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        mob_has_active_raid(mob)
            && mob
                .get_random()
                .random_range(0..to_goal_ticks(DOOR_BREAK_CHANCE))
                == 0
            && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        mob_has_active_raid(mob) && self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
        mob.get_mob_entity()
            .living_entity
            .no_action_time
            .store(0, Ordering::Relaxed);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
    }

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// illager: open doors only in raids (AbstractIllager.RaiderOpenDoorGoal)
struct RaiderOpenDoorGoal {
    inner: OpenDoorGoal,
}

impl Default for RaiderOpenDoorGoal {
    fn default() -> Self {
        Self {
            inner: OpenDoorGoal::new(false),
        }
    }
}

impl Goal for RaiderOpenDoorGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start(mob) && mob_has_active_raid(mob)
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

    fn should_run_every_tick(&self) -> bool {
        self.inner.should_run_every_tick()
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

impl PatrollingMonster for VindicatorEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }
}

impl Raider for VindicatorEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityVindicatorCelebrate
    }
}
