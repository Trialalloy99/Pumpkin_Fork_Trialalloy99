use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use pumpkin_data::Block;
use pumpkin_data::entity::{EntityPose, EntityStatus, EntityType};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ai::behavior::{
        feline,
        fleeing::{self, Flee},
        hunting::{self, Hunt},
    },
    ai::goal::{swim::SwimGoal, tempt::TemptGoal},
    ai::target_match::TargetMatch,
    mob::{Mob, MobEntity, spawn::SpawnGroupData},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

const CROUCH_SPEED_MOD: f64 = 0.6;
const SPRINT_SPEED_MOD: f64 = 1.33;

const OCELOT_FLEES: &[Flee] = &[feline::flee_players(4, is_wary)];

const OCELOT_HUNTS: &[Hunt] = &[
    Hunt::new(1, TargetMatch::Type(&EntityType::CHICKEN), false),
    hunting::baby_turtles(1, false),
];

fn is_wary(mob: &dyn Mob) -> bool {
    mob.cast_any()
        .downcast_ref::<OcelotEntity>()
        .is_some_and(|ocelot| !ocelot.is_trusting())
}

/// Represents an Ocelot, a shy passive mob found in jungles.
///
/// Wiki: <https://minecraft.wiki/w/Ocelot>
pub struct OcelotEntity {
    pub mob_entity: MobEntity,
    pub is_trusting: AtomicBool,
}

impl OcelotEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let ocelot = Self {
            mob_entity,
            is_trusting: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(ocelot);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // Goal 1: FloatGoal (SwimGoal)
            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            // Goal 3: OcelotTemptGoal
            goal_selector.add_goal(3, feline::tempt_goal());
            // Goal 4: OcelotAvoidEntityGoal (when not trusting)
            fleeing::apply(goal_selector, OCELOT_FLEES);
            // Goal 7-11: LeapAtTarget, OcelotAttack, Breed, Stroll, LookAtPlayer
            feline::apply_tail(goal_selector, mob_weak, 7);
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;

            // Target Goal 1: NearestAttackableTargetGoal for Chicken and Turtle (baby on land)
            hunting::apply(target_selector, &mob_arc.mob_entity, OCELOT_HUNTS);
        };

        mob_arc
    }

    pub fn is_trusting(&self) -> bool {
        self.is_trusting.load(Ordering::Relaxed)
    }

    pub fn set_trusting(&self, trusting: bool) {
        self.is_trusting.store(trusting, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::ocelot::TRUSTING, trusting);
    }
}

impl Animal for OcelotEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        let item = item_stack.get_item();
        item.has_tag(&tag::Item::MINECRAFT_OCELOT_FOOD)
            || item == &Item::COD
            || item == &Item::SALMON
    }
}

impl Mob for OcelotEntity {
    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("Trusting", self.is_trusting.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(trusting) = nbt.get_bool("Trusting") {
            self.is_trusting.store(trusting, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // ocelot: Ocelot.removeWhenFarAway
    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        !self.is_trusting() && self.get_entity().tick_count.load(Ordering::Relaxed) > 2400
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        900
    }

    // ocelot: BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.3,
            height: 0.35,
            eye_height: 0.343_75,
        })
    }

    // ocelot: kittens follow adults in a spawn group (Ocelot.finalizeSpawn)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_super(group_data.or(Some(SpawnGroupData::Ageable {
            group_size: 0,
            should_spawn_baby: true,
            baby_spawn_chance: 1.0,
        })))
    }

    // ocelot: Ocelot.checkSpawnObstruction, above sea level on grass or leaves
    fn check_spawn_obstruction(&self, world: &World) -> bool {
        let bounding_box = self.get_entity().bounding_box.load();
        if !world.get_entities_at_box(&bounding_box).is_empty()
            || world.contains_any_liquid(bounding_box)
        {
            return false;
        }
        let pos = self.get_entity().block_pos.load();
        if pos.0.y < world.sea_level {
            return false;
        }
        let below = world.get_block(&pos.down());
        below == &Block::GRASS_BLOCK || below.has_tag(&tag::Block::MINECRAFT_LEAVES)
    }

    // ocelot: crouch and sprint follow the move speed (Ocelot.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let (pose, sprinting) = {
            let steered = self.mob_entity.steered();
            let control = steered.move_control();
            if control.has_wanted() {
                let speed = control.speed_modifier();
                if speed == CROUCH_SPEED_MOD {
                    (EntityPose::Crouching, false)
                } else if speed == SPRINT_SPEED_MOD {
                    (EntityPose::Standing, true)
                } else {
                    (EntityPose::Standing, false)
                }
            } else {
                (EntityPose::Standing, false)
            }
        };
        entity.set_pose(pose);
        entity.set_sprinting(sprinting);
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::ocelot::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::ocelot::TRUSTING,
            self.is_trusting.load(Ordering::Relaxed),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let is_food = self.is_food(item_stack);
        let dist_sqr = self
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&player.get_entity().pos.load());

        // ocelot: trust only while the tempt goal runs (Ocelot.mobInteract)
        if self.mob_entity.goals_selector.is_running::<TemptGoal>()
            && !self.is_trusting()
            && is_food
            && dist_sqr < 9.0
        {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);

            let mut rng = rand::rng();
            if rng.random_range(0..3) == 0 {
                self.set_trusting(true);
                self.get_entity().world.load().send_entity_status(
                    self.get_entity(),
                    EntityStatus::TrustingSucceeded,
                    ActorEventID::TamingSucceeded,
                );
            } else {
                self.get_entity().world.load().send_entity_status(
                    self.get_entity(),
                    EntityStatus::TrustingFailed,
                    ActorEventID::TamingFailed,
                );
            }

            return true;
        }

        self.animal_interact(
            player,
            item_stack,
            pumpkin_data::sound::Sound::EntityOcelotAmbient,
        )
    }
}
