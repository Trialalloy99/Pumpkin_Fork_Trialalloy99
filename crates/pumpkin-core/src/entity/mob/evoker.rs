use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};
use uuid::Uuid;

use pumpkin_data::BlockDirection;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::entity::ai::behavior::fleeing::{self, Flee};
use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{look_at_entity::LookAtEntityGoal, swim::SwimGoal, wander_around::WanderAroundGoal},
    mob::{
        Mob, MobEntity,
        patrol::{PatrolData, PatrollingMonster},
        raider::{
            self, ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider, RaiderCelebrationGoal,
            RaiderData, RaiderMoveThroughVillageGoal,
        },
        spawn::finalize_spawn,
        spellcaster::{
            Spell, SpellcasterCastingSpellGoal, SpellcasterData, SpellcasterIllager,
            SpellcasterUseSpellGoal,
        },
        vex::VexEntity,
    },
    passive::sheep::SheepEntity,
    projectile::evoker_fangs::EvokerFangsEntity,
    r#type::from_type,
};
use crate::world::World;
use crate::world::game_event::GameEventContext;

const VEX_COUNT_RANGE: f64 = 16.0;
const WOLOLO_TARGET_COLOR: u8 = 11;
const WOLOLO_RESULT_COLOR: u8 = 14;
const FANGS_CLOSE_RANGE_SQR: f64 = 9.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum IllagerSpell {
    None = 0,
    SummonVex = 1,
    Fangs = 2,
    Wololo = 3,
    Disappear = 4,
    Blindness = 5,
}

impl IllagerSpell {
    #[must_use]
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::SummonVex,
            2 => Self::Fangs,
            3 => Self::Wololo,
            4 => Self::Disappear,
            5 => Self::Blindness,
            _ => Self::None,
        }
    }
}

const EVOKER_FLEES: &[Flee] = &[
    Flee::new(2, TargetMatch::Type(&EntityType::PLAYER), 8.0, 0.6, 1.0),
    raider::flee_creaking(3, 0.6, 1.0),
];

const EVOKER_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true).forget_unseen_after(300),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false).forget_unseen_after(300),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), false),
];

pub struct EvokerEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    spellcaster: SpellcasterData,
    wololo_target_id: AtomicI32,
}

const NO_WOLOLO_TARGET: i32 = -1;

impl EvokerEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let evoker = Self {
            mob_entity,
            raider_data: RaiderData::default(),
            spellcaster: SpellcasterData::default(),
            wololo_target_id: AtomicI32::new(NO_WOLOLO_TARGET),
        };
        let mob_arc = Arc::new(evoker);
        let mob_weak: Weak<Self> = Arc::downgrade(&mob_arc);
        let mob_interface_weak: Weak<dyn Mob> = {
            let mob_dyn: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_dyn)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                1,
                Box::new(SpellcasterCastingSpellGoal::new(mob_weak.clone())),
            );
            goal_selector.add_goal(2, Box::new(ObtainRaidLeaderBannerGoal));
            fleeing::apply(goal_selector, EVOKER_FLEES);
            goal_selector.add_goal(3, Box::new(RaiderMoveThroughVillageGoal::new(1.05)));
            goal_selector.add_goal(3, Box::new(PathfindToRaidGoal::default()));
            goal_selector.add_goal(
                4,
                Box::new(SpellcasterUseSpellGoal::new(
                    mob_weak.clone(),
                    SummonVexSpell {
                        evoker: mob_weak.clone(),
                    },
                )),
            );
            goal_selector.add_goal(
                5,
                Box::new(SpellcasterUseSpellGoal::new(
                    mob_weak.clone(),
                    FangsSpell {
                        evoker: mob_weak.clone(),
                    },
                )),
            );
            goal_selector.add_goal(
                6,
                Box::new(SpellcasterUseSpellGoal::new(
                    mob_weak.clone(),
                    WololoSpell { evoker: mob_weak },
                )),
            );
            goal_selector.add_goal(7, Box::new(RaiderCelebrationGoal));
            goal_selector.add_goal(8, Box::new(WanderAroundGoal::new(0.6)));
            goal_selector.add_goal(
                9,
                Box::new(LookAtEntityGoal::new(
                    mob_interface_weak.clone(),
                    &EntityType::PLAYER,
                    3.0,
                    1.0,
                    false,
                )),
            );
            goal_selector.add_goal(10, LookAtEntityGoal::any_mob(mob_interface_weak, 8.0, 0.02));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(1, raider::revenge_goal());
            hunting::apply(target_selector, &mob_arc.mob_entity, EVOKER_HUNTS);
        };

        mob_arc
    }

    fn wololo_target(&self) -> Option<Arc<dyn EntityBase>> {
        let id = self.wololo_target_id.load(Ordering::Relaxed);
        if id == NO_WOLOLO_TARGET {
            return None;
        }
        self.mob_entity
            .living_entity
            .entity
            .world
            .load()
            .get_entity_by_id(id)
    }
}

impl SpellcasterIllager for EvokerEntity {
    fn spellcaster_data(&self) -> &SpellcasterData {
        &self.spellcaster
    }

    fn casting_sound(&self) -> Sound {
        Sound::EntityEvokerCastSpell
    }

    fn casting_look_target(&self) -> Option<Arc<dyn EntityBase>> {
        self.wololo_target()
    }
}

impl Mob for EvokerEntity {
    fn as_patrolling_monster(&self) -> Option<&dyn PatrollingMonster> {
        Some(self)
    }

    fn as_raider(&self) -> Option<&dyn Raider> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_raider_nbt(nbt);
        self.spellcaster.write_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_raider_nbt(nbt);
        self.spellcaster.read_nbt(nbt);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.spellcaster.tick();
    }

    // evoker: vexes of an allied owner are allies (Evoker.considersEntityAsAlly)
    fn mob_considers_entity_as_ally(
        &self,
        other: &dyn EntityBase,
        _brain: Option<&crate::entity::ai::brain::BrainState>,
    ) -> Option<bool> {
        let entity = &self.mob_entity.living_entity.entity;
        let illager_ally = |other: &dyn EntityBase| {
            self.is_on_same_team(other)
                || (other
                    .get_entity()
                    .entity_type
                    .has_tag(&tag::EntityType::MINECRAFT_ILLAGER_FRIENDS)
                    && self.get_team_name().is_none()
                    && other.get_team_name().is_none())
        };
        if illager_ally(other) {
            return Some(true);
        }
        if let Some(vex) = other.cast_any().downcast_ref::<VexEntity>()
            && let Some(owner) = vex.get_owner()
        {
            return Some(
                owner.get_entity().entity_id == entity.entity_id || illager_ally(owner.as_ref()),
            );
        }
        Some(false)
    }
}

impl PatrollingMonster for EvokerEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }
}

impl Raider for EvokerEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityEvokerCelebrate
    }
}

// evoker: fangs line or ring (Evoker.EvokerAttackSpellGoal)
struct FangsSpell {
    evoker: Weak<EvokerEntity>,
}

impl FangsSpell {
    // evoker: fang placement on the ground (EvokerAttackSpellGoal.createSpellEntity)
    fn create_spell_entity(
        evoker: &EvokerEntity,
        world: &Arc<World>,
        (x, z): (f64, f64),
        (min_y, max_y): (f64, f64),
        angle: f32,
        delay_ticks: u32,
    ) {
        let mut pos = BlockPos::floored(x, max_y, z);
        let mut top_offset = 0.0;
        let lowest = min_y.floor() as i32 - 1;
        let mut success = false;
        loop {
            let below = pos.down();
            if world
                .get_block_state(&below)
                .is_side_solid(BlockDirection::Up)
            {
                let state = world.get_block_state(&pos);
                if !state.is_air() {
                    for shape in state.get_block_collision_shapes_at(&pos) {
                        top_offset = f64::max(top_offset, shape.max.y);
                    }
                }
                success = true;
                break;
            }
            pos = pos.down();
            if pos.0.y < lowest {
                break;
            }
        }
        if !success {
            return;
        }
        let evoker_ent = &evoker.mob_entity.living_entity.entity;
        let fang_pos = Vector3::new(x, f64::from(pos.0.y) + top_offset, z);
        let entity = Entity::from_uuid(
            Uuid::new_v4(),
            world.clone(),
            fang_pos,
            &EntityType::EVOKER_FANGS,
        );
        world.spawn_entity(Arc::new(EvokerFangsEntity::new(
            entity,
            delay_ticks,
            angle,
            Some(evoker_ent),
        )));
        world.emit_game_event_with_context(
            GameEvent::EntityPlace.name(),
            fang_pos,
            &GameEventContext {
                source_entity: Some(evoker),
            },
        );
    }
}

impl Spell for FangsSpell {
    fn casting_time(&self) -> i32 {
        40
    }

    fn casting_interval(&self) -> i32 {
        100
    }

    fn prepare_sound(&self) -> Option<Sound> {
        Some(Sound::EntityEvokerPrepareAttack)
    }

    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Fangs
    }

    fn perform(&mut self, mob: &dyn Mob) {
        let Some(evoker) = self.evoker.upgrade() else {
            return;
        };
        let Some(target) = mob.get_mob_entity().get_target() else {
            return;
        };
        let evoker_ent = &evoker.mob_entity.living_entity.entity;
        let world = evoker_ent.world.load_full();
        let evoker_pos = evoker_ent.pos.load();
        let target_pos = target.get_entity().pos.load();
        let min_y = target_pos.y.min(evoker_pos.y);
        let max_y = target_pos.y.max(evoker_pos.y) + 1.0;
        let angle_towards_target =
            (target_pos.z - evoker_pos.z).atan2(target_pos.x - evoker_pos.x) as f32;
        let ys = (min_y, max_y);

        if evoker_pos.squared_distance_to_vec(&target_pos) < FANGS_CLOSE_RANGE_SQR {
            for i in 0..5 {
                let angle = angle_towards_target + i as f32 * std::f32::consts::PI * 0.4;
                let at = (
                    evoker_pos.x + f64::from(angle.cos() * 1.5),
                    evoker_pos.z + f64::from(angle.sin() * 1.5),
                );
                Self::create_spell_entity(&evoker, &world, at, ys, angle, 0);
            }
            for i in 0..8 {
                let angle = angle_towards_target
                    + i as f32 * std::f32::consts::PI * 2.0 / 8.0
                    + (std::f64::consts::PI * 2.0 / 5.0) as f32;
                let at = (
                    evoker_pos.x + f64::from(angle.cos() * 2.5),
                    evoker_pos.z + f64::from(angle.sin() * 2.5),
                );
                Self::create_spell_entity(&evoker, &world, at, ys, angle, 3);
            }
        } else {
            for i in 0..16u32 {
                let reach = 1.25 * f64::from(i + 1);
                let at = (
                    evoker_pos.x + f64::from(angle_towards_target.cos()) * reach,
                    evoker_pos.z + f64::from(angle_towards_target.sin()) * reach,
                );
                Self::create_spell_entity(&evoker, &world, at, ys, angle_towards_target, i);
            }
        }
    }
}

// evoker: vex summon (Evoker.EvokerSummonSpellGoal)
struct SummonVexSpell {
    evoker: Weak<EvokerEntity>,
}

impl Spell for SummonVexSpell {
    fn casting_time(&self) -> i32 {
        100
    }

    fn casting_interval(&self) -> i32 {
        340
    }

    fn prepare_sound(&self) -> Option<Sound> {
        Some(Sound::EntityEvokerPrepareSummon)
    }

    fn spell(&self) -> IllagerSpell {
        IllagerSpell::SummonVex
    }

    fn can_use(&mut self, mob: &dyn Mob) -> bool {
        // cheap roll first, then the vex count (random.nextInt(8) + 1 > vexes)
        let roll = rand::random_range(0..8) + 1;
        let entity = mob.get_entity();
        let pos = entity.pos.load();
        let bb =
            entity
                .bounding_box
                .load()
                .expand(VEX_COUNT_RANGE, VEX_COUNT_RANGE, VEX_COUNT_RANGE);
        let vexes = entity
            .world
            .load()
            .get_entities_at_box(&bb)
            .iter()
            .filter(|other| {
                let other_entity = other.get_entity();
                *other_entity.entity_type == EntityType::VEX
                    && other_entity.is_alive()
                    && other_entity.pos.load().squared_distance_to_vec(&pos)
                        <= VEX_COUNT_RANGE * VEX_COUNT_RANGE
            })
            .take(roll as usize)
            .count();
        roll as usize > vexes
    }

    fn perform(&mut self, mob: &dyn Mob) {
        let Some(evoker) = self.evoker.upgrade() else {
            return;
        };
        let evoker_ent = &evoker.mob_entity.living_entity.entity;
        let world = evoker_ent.world.load_full();
        let evoker_block_pos = mob.get_entity().block_pos.load();
        let evoker_team = evoker.get_team_name();
        for _ in 0..3 {
            let pos = evoker_block_pos.add(
                rand::random_range(0..5) - 2,
                1,
                rand::random_range(0..5) - 2,
            );
            let vex = from_type(
                &EntityType::VEX,
                pos.to_centered_f64() - Vector3::new(0.0, 0.5, 0.0),
                &world,
                Uuid::new_v4(),
            );
            finalize_spawn(
                &vex,
                &world,
                None,
                crate::entity::mob::spawn::SpawnReason::MobSummoned,
            );
            if let Some(vex_entity) = vex.cast_any().downcast_ref::<VexEntity>() {
                vex_entity.set_owner(evoker_ent);
                vex_entity.set_bound_origin(Some(pos));
                vex_entity.set_limited_life(20 * (30 + rand::random_range(0..90)));
            }
            if let Some(team) = &evoker_team {
                vex.get_entity().join_team(team);
            }
            world.spawn_entity(vex);
            world.emit_game_event_with_context(
                GameEvent::EntityPlace.name(),
                pos.to_centered_f64(),
                &GameEventContext {
                    source_entity: Some(&*evoker),
                },
            );
        }
    }
}

// evoker: turn blue sheep red (Evoker.EvokerWololoSpellGoal)
struct WololoSpell {
    evoker: Weak<EvokerEntity>,
}

impl Spell for WololoSpell {
    fn casting_time(&self) -> i32 {
        60
    }

    fn casting_interval(&self) -> i32 {
        140
    }

    fn cast_warmup(&self) -> i32 {
        40
    }

    fn requires_target(&self) -> bool {
        false
    }

    fn prepare_sound(&self) -> Option<Sound> {
        Some(Sound::EntityEvokerPrepareWololo)
    }

    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Wololo
    }

    fn can_use(&mut self, mob: &dyn Mob) -> bool {
        if mob.get_mob_entity().get_target().is_some() {
            return false;
        }
        let Some(evoker) = self.evoker.upgrade() else {
            return false;
        };
        let entity = mob.get_entity();
        let world = entity.world.load();
        if !world.level_info.load().game_rules.mob_griefing {
            return false;
        }
        let pos = entity.pos.load();
        let bb = entity.bounding_box.load().expand(16.0, 4.0, 16.0);
        let sheep: Vec<i32> = world
            .get_entities_at_box(&bb)
            .iter()
            .filter(|other| {
                other
                    .cast_any()
                    .downcast_ref::<SheepEntity>()
                    .is_some_and(|sheep| {
                        sheep.get_color() == WOLOLO_TARGET_COLOR
                            && other.get_entity().pos.load().squared_distance_to_vec(&pos) <= 256.0
                    })
            })
            .map(|other| other.get_entity().entity_id)
            .collect();
        if sheep.is_empty() {
            return false;
        }
        evoker
            .wololo_target_id
            .store(sheep[rand::random_range(0..sheep.len())], Ordering::Relaxed);
        true
    }

    fn can_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.evoker.upgrade().is_some_and(|evoker| {
            evoker.wololo_target_id.load(Ordering::Relaxed) != NO_WOLOLO_TARGET
        })
    }

    fn on_stop(&mut self, _mob: &dyn Mob) {
        if let Some(evoker) = self.evoker.upgrade() {
            evoker
                .wololo_target_id
                .store(NO_WOLOLO_TARGET, Ordering::Relaxed);
        }
    }

    fn perform(&mut self, _mob: &dyn Mob) {
        let Some(evoker) = self.evoker.upgrade() else {
            return;
        };
        if let Some(target) = evoker.wololo_target()
            && target.get_entity().is_alive()
            && let Some(sheep) = target.cast_any().downcast_ref::<SheepEntity>()
        {
            sheep.set_color(WOLOLO_RESULT_COLOR);
        }
    }
}
