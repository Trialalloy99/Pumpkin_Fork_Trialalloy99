use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag;
use pumpkin_data::tracked_data;
use pumpkin_data::{Block, BlockState};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;

use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        active_target::ActiveTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal,
        move_towards_restriction::MoveTowardsRestrictionGoal, open_door::OpenDoorGoal,
        revenge::RevengeGoal, swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    mob::{
        Mob, MobEntity, conversion::convert_single, equipment::RegionalDifficulty,
        neutral::find_by_uuid, spawn::SpawnGroupData,
    },
};
use crate::plugin::api::events::entity::entity_target::TargetReason;
use crate::world::World;

pub struct PiglinBruteEntity {
    pub mob_entity: MobEntity,
    pub immune_to_zombification: AtomicBool,
    pub time_in_overworld: AtomicI32,
    was_fighting: AtomicBool,
}

fn is_abstract_piglin(entity_type: &'static EntityType) -> bool {
    entity_type == &EntityType::PIGLIN || entity_type == &EntityType::PIGLIN_BRUTE
}

impl PiglinBruteEntity {
    pub const CONVERSION_TIME: i32 = 300;
    pub const XP_REWARD: u32 = 20;

    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let piglin = Self {
            mob_entity,
            immune_to_zombification: AtomicBool::new(false),
            time_in_overworld: AtomicI32::new(0),
            was_fighting: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(piglin);
        mob_arc.mob_entity.set_can_pick_up_loot(true);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(OpenDoorGoal::new(true)));
            goal_selector.add_goal(2, Box::new(MeleeAttackGoal::new(1.0, true)));
            goal_selector.add_goal(5, Box::new(MoveTowardsRestrictionGoal::new(0.6)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(0.6)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            let target_selector = &mob_arc.mob_entity.target_selector;
            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .ignoring(is_abstract_piglin)
                        .alerting_others()
                        .forget_unseen_after(600),
                ),
            );
            target_selector.add_goal(
                2,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::PLAYER, true),
            );
            target_selector.add_goal(
                3,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, TargetMatch::WITHER_LIKE, true),
            );
        };

        mob_arc
    }

    #[must_use]
    pub fn is_immune_to_zombification(&self) -> bool {
        self.immune_to_zombification.load(Ordering::Relaxed)
    }

    pub fn set_immune_to_zombification(&self, immune: bool) {
        self.immune_to_zombification
            .store(immune, Ordering::Relaxed);
        self.mob_entity.living_entity.entity.set_synced_data(
            tracked_data::piglin_brute::DATA_IMMUNE_TO_ZOMBIFICATION,
            immune,
        );
    }

    #[must_use]
    pub fn is_converting(&self, world: &World) -> bool {
        !self.is_immune_to_zombification()
            && !self.mob_entity.is_no_ai()
            && world.dimension.piglins_zombify
    }

    #[must_use]
    pub fn check_piglin_brute_spawn_rules(world: &World, pos: &BlockPos) -> bool {
        let below = BlockPos::new(pos.0.x, pos.0.y - 1, pos.0.z);
        let state = world.get_block_state(&below);
        state.id != Block::NETHER_WART_BLOCK.default_state.id
    }

    fn convert_to_zombified(&self) {
        self.make_sound(Sound::EntityPiglinBruteConvertedToZombified);
        let _ = convert_single(
            self,
            &EntityType::ZOMBIFIED_PIGLIN,
            true,
            true,
            |zombified| {
                if let Some(living) = zombified.get_living_entity() {
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
            },
        );
    }
}

impl CustomSound for PiglinBruteEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPiglinBruteDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPiglinBruteHurt)
    }

    fn sound_category(&self) -> SoundCategory {
        SoundCategory::Hostile
    }
}

impl Mob for PiglinBruteEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityPiglinBruteAmbient)
    }

    // piglin brute: home and golden axe (PiglinBrute.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mob = &self.mob_entity;
        mob.position_target
            .store(self.get_entity().block_pos.load());
        mob.position_target_range.store(100, Ordering::Relaxed);
        self.populate_default_equipment_slots(
            world,
            &RegionalDifficulty::at(world, self.get_entity().pos.load()),
        );
        self.finalize_spawn_super(group_data)
    }

    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        self.mob_entity.set_item_slot(
            &EquipmentSlot::MAIN_HAND,
            ItemStack::new(1, &Item::GOLDEN_AXE),
        );
    }

    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        stack.item == &Item::GOLDEN_AXE && self.can_hold_item(stack)
    }

    fn preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        Some(&tag::Item::MINECRAFT_PIGLIN_PREFERRED_WEAPONS)
    }
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityPiglinBruteStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        if self.is_immune_to_zombification() {
            entity.set_synced_data(
                tracked_data::piglin_brute::DATA_IMMUNE_TO_ZOMBIFICATION,
                true,
            );
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        if self.is_immune_to_zombification() {
            nbt.put_bool("IsImmuneToZombification", true);
        }
        let time_in_overworld = self.time_in_overworld.load(Ordering::Relaxed);
        if time_in_overworld > 0 {
            nbt.put_int("TimeInOverworld", time_in_overworld);
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(immune) = nbt.get_bool("IsImmuneToZombification") {
            self.set_immune_to_zombification(immune);
        }
        if let Some(time) = nbt.get_int("TimeInOverworld") {
            self.time_in_overworld.store(time, Ordering::Relaxed);
        }
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        if !entity.is_alive() {
            return;
        }

        let world = entity.world.load();
        let fighting = self.mob_entity.get_target().is_some();
        let was_fighting = self.was_fighting.swap(fighting, Ordering::Relaxed);
        if fighting && (!was_fighting || rand::random::<f32>() < 0.0125) {
            self.make_sound(Sound::EntityPiglinBruteAngry);
        }
        if self.is_converting(&world) {
            let time = self.time_in_overworld.fetch_add(1, Ordering::Relaxed) + 1;
            if time > Self::CONVERSION_TIME {
                self.convert_to_zombified();
            }
        } else {
            self.time_in_overworld.store(0, Ordering::Relaxed);
        }
    }

    fn on_damage_with_context(
        &self,
        _damage_type: DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let Some(source) = cause.or(source) else {
            return true;
        };
        if is_abstract_piglin(source.get_entity().entity_type) {
            return true;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        let Some(attacker) = find_by_uuid(&world, source.get_entity().entity_uuid) else {
            return true;
        };
        let area = entity.bounding_box.load().expand(16.0, 8.0, 16.0);
        for nearby in world.get_entities_at_box(&area) {
            if !is_abstract_piglin(nearby.get_entity().entity_type) {
                continue;
            }
            let Some(piglin) = nearby.get_mob() else {
                continue;
            };
            if piglin.is_baby_mob() {
                continue;
            }
            let nearby_pos = nearby.get_entity().pos.load();
            let new_distance =
                nearby_pos.squared_distance_to_vec(&attacker.get_entity().pos.load());
            let closer = piglin
                .get_mob_entity()
                .get_target()
                .as_ref()
                .is_none_or(|current| {
                    new_distance
                        < nearby_pos.squared_distance_to_vec(&current.get_entity().pos.load())
                });
            if closer {
                piglin.set_mob_target(
                    Some(attacker.clone()),
                    TargetReason::TargetAttackedNearbyEntity,
                );
            }
        }
        true
    }

    fn get_base_experience_reward(&self) -> u32 {
        Self::XP_REWARD
    }
}
