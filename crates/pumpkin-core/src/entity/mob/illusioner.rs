use std::sync::{Arc, Weak};

use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::Sound;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::Difficulty;

use crate::entity::ai::behavior::fleeing::{self, Flee};
use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        bow_attack::BowAttackGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{
        Mob, MobEntity,
        equipment::RegionalDifficulty,
        evoker::IllagerSpell,
        patrol::{LongDistancePatrolGoal, PatrolData, PatrollingMonster},
        raider::{
            self, ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, Raider, RaiderCelebrationGoal,
            RaiderData, RaiderMoveThroughVillageGoal,
        },
        spawn::SpawnGroupData,
        spellcaster::{
            Spell, SpellcasterCastingSpellGoal, SpellcasterData, SpellcasterIllager,
            SpellcasterUseSpellGoal,
        },
    },
};
use crate::world::World;

const ILLUSIONER_FLEES: &[Flee] = &[raider::flee_creaking(3, 1.0, 1.2)];

const ILLUSIONER_HUNTS: &[Hunt] = &[
    Hunt::new(2, TargetMatch::Type(&EntityType::PLAYER), true).forget_unseen_after(300),
    Hunt::new(3, TargetMatch::VILLAGER_LIKE, false).forget_unseen_after(300),
    Hunt::new(3, TargetMatch::Type(&EntityType::IRON_GOLEM), false).forget_unseen_after(300),
];

const BLINDNESS_DURATION: i32 = 400;
const MIRROR_DURATION: i32 = 1200;

pub struct IllusionerEntity {
    pub mob_entity: MobEntity,
    pub raider_data: RaiderData,
    spellcaster: SpellcasterData,
}

impl IllusionerEntity {
    #[must_use]
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let illusioner = Self {
            mob_entity,
            raider_data: RaiderData::default(),
            spellcaster: SpellcasterData::default(),
        };
        let mob_arc = Arc::new(illusioner);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let self_weak = Arc::downgrade(&mob_arc);

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(4, Box::new(LongDistancePatrolGoal::new(0.7, 0.595)));
            goal_selector.add_goal(1, Box::new(ObtainRaidLeaderBannerGoal));
            goal_selector.add_goal(3, Box::new(PathfindToRaidGoal::default()));
            goal_selector.add_goal(4, Box::new(RaiderMoveThroughVillageGoal::new(1.05)));
            goal_selector.add_goal(5, Box::new(RaiderCelebrationGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                1,
                Box::new(SpellcasterCastingSpellGoal::new(self_weak.clone())),
            );
            fleeing::apply(goal_selector, ILLUSIONER_FLEES);
            goal_selector.add_goal(
                4,
                Box::new(SpellcasterUseSpellGoal::new(self_weak.clone(), MirrorSpell)),
            );
            goal_selector.add_goal(
                5,
                Box::new(SpellcasterUseSpellGoal::new(
                    self_weak,
                    BlindnessSpell::default(),
                )),
            );
            goal_selector.add_goal(6, Box::new(BowAttackGoal::new(0.5, 20, 15.0)));
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
            hunting::apply(target_selector, &mob_arc.mob_entity, ILLUSIONER_HUNTS);
        };

        mob_arc
    }
}

impl SpellcasterIllager for IllusionerEntity {
    fn spellcaster_data(&self) -> &SpellcasterData {
        &self.spellcaster
    }

    fn casting_sound(&self) -> Sound {
        Sound::EntityIllusionerCastSpell
    }
}

impl Mob for IllusionerEntity {
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
        self.spellcaster.write_nbt(nbt);
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_raider_nbt(nbt);
        self.spellcaster.read_nbt(nbt);
    }

    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        self.spellcaster.tick();
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityIllusionerAmbient)
    }

    // illusioner: bow in main hand (Illusioner.finalizeSpawn)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.mob_entity
            .set_item_slot(&EquipmentSlot::MAIN_HAND, ItemStack::new(1, &Item::BOW));
        self.finalize_spawn_super(group_data)
    }
}

// illusioner: invisibility spell (IllusionerMirrorSpellGoal)
struct MirrorSpell;

impl Spell for MirrorSpell {
    fn casting_time(&self) -> i32 {
        20
    }

    fn casting_interval(&self) -> i32 {
        340
    }

    fn prepare_sound(&self) -> Option<Sound> {
        Some(Sound::EntityIllusionerPrepareMirror)
    }

    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Disappear
    }

    fn can_use(&mut self, mob: &dyn Mob) -> bool {
        !mob.get_mob_entity()
            .living_entity
            .has_effect(&StatusEffect::INVISIBILITY)
    }

    fn perform(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().living_entity.add_effect(Effect {
            effect_type: &StatusEffect::INVISIBILITY,
            duration: MIRROR_DURATION,
            amplifier: 0,
            ambient: false,
            show_particles: true,
            show_icon: true,
            blend: false,
        });
    }
}

// illusioner: blind a new target on hard-ish difficulty (IllusionerBlindnessSpellGoal)
#[derive(Default)]
struct BlindnessSpell {
    last_target_id: i32,
}

impl Spell for BlindnessSpell {
    fn casting_time(&self) -> i32 {
        20
    }

    fn casting_interval(&self) -> i32 {
        180
    }

    fn prepare_sound(&self) -> Option<Sound> {
        Some(Sound::EntityIllusionerPrepareBlindness)
    }

    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Blindness
    }

    fn can_use(&mut self, mob: &dyn Mob) -> bool {
        let Some(target) = mob.get_mob_entity().get_target() else {
            return false;
        };
        if target.get_entity().entity_id == self.last_target_id {
            return false;
        }
        let entity = mob.get_entity();
        RegionalDifficulty::at(&entity.world.load_full(), entity.pos.load()).effective_difficulty
            > Difficulty::Normal as i32 as f32
    }

    fn on_start(&mut self, mob: &dyn Mob) {
        if let Some(target) = mob.get_mob_entity().get_target() {
            self.last_target_id = target.get_entity().entity_id;
        }
    }

    fn perform(&mut self, mob: &dyn Mob) {
        if let Some(living) = mob
            .get_mob_entity()
            .get_target()
            .as_ref()
            .and_then(|target| target.get_living_entity())
        {
            living.add_effect(Effect {
                effect_type: &StatusEffect::BLINDNESS,
                duration: BLINDNESS_DURATION,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
        }
    }
}

impl PatrollingMonster for IllusionerEntity {
    fn get_patrol_data(&self) -> &PatrolData {
        &self.raider_data.patrol_data
    }
}

impl Raider for IllusionerEntity {
    fn get_raider_data(&self) -> &RaiderData {
        &self.raider_data
    }

    fn get_celebrate_sound(&self) -> Sound {
        Sound::EntityIllusionerAmbient
    }
}
