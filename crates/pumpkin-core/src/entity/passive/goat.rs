use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use crate::item::items::bucket::fill_bucket_stack;
use pumpkin_data::BlockState;
use pumpkin_data::entity::EntityType;
use pumpkin_data::entity_status::EntityStatus;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::ActorEventID;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::goal::Controls;
use crate::entity::ai::goal::Goal;
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::ai::target_match::{SearchArea, TargetMatch};
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::WHEAT];

// goat: ram attack goal (GoatAi.initRamActivity)
pub struct GoatRamGoal {
    cooldown: i32,
    prepare_ticks: i32,
    target: Option<Arc<dyn EntityBase>>,
    target_pos: Vector3<f64>,
}

impl Default for GoatRamGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl GoatRamGoal {
    #[must_use]
    pub fn new() -> Self {
        let mut rng = rand::rng();
        Self {
            cooldown: rng.random_range(600..6000),
            prepare_ticks: 0,
            target: None,
            target_pos: Vector3::new(0.0, 0.0, 0.0),
        }
    }
}

impl Goal for GoatRamGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }

        let entity = &mob.get_mob_entity().living_entity.entity;
        let world = entity.world.load();
        let area = SearchArea::from_feet(entity, 7.0, 3.0);

        // Find non-goat living target within 4..7 blocks
        let found = TargetMatch::Any.find_nearest(&world, &area, |candidate| {
            let t = candidate.get_entity().entity_type;
            candidate.get_entity().entity_id != entity.entity_id
                && t != &EntityType::GOAT
                && candidate.get_living_entity().is_some()
        });

        if let Some(target) = found {
            let dist_sq = entity
                .pos
                .load()
                .squared_distance_to_vec(&target.get_entity().pos.load());
            if (16.0..=49.0).contains(&dist_sq) {
                self.target_pos = target.get_entity().pos.load();
                self.target = Some(target);
                self.prepare_ticks = 20;
                return true;
            }
        }

        // Reset cooldown if no target found
        let mut rng = rand::rng();
        let is_screamer = mob
            .cast_any()
            .downcast_ref::<GoatEntity>()
            .is_some_and(GoatEntity::is_screaming);
        self.cooldown = if is_screamer {
            rng.random_range(100..300)
        } else {
            rng.random_range(600..6000)
        };
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        self.target.is_some()
    }

    fn start(&mut self, mob: &dyn Mob) {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let world = entity.world.load();
        world.send_entity_status(entity, EntityStatus::StartRam, ActorEventID::None);

        let is_screamer = mob
            .cast_any()
            .downcast_ref::<GoatEntity>()
            .is_some_and(GoatEntity::is_screaming);
        let sound = if is_screamer {
            Sound::EntityGoatScreamingPrepareRam
        } else {
            Sound::EntityGoatPrepareRam
        };
        world.play_sound(sound, SoundCategory::Neutral, &entity.pos.load());
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let mob_pos = entity.pos.load();

        if self.prepare_ticks > 0 {
            self.prepare_ticks -= 1;
            if self.prepare_ticks == 0 {
                let steered = mob.get_mob_entity().steered();
                let mut navigator = steered.navigator();
                navigator.set_progress(NavigatorGoal::new(mob_pos, self.target_pos, 3.0));
            }
            return;
        }

        let Some(ref target) = self.target else {
            return;
        };
        let target_pos = target.get_entity().pos.load();
        let dist_sq = mob_pos.squared_distance_to_vec(&target_pos);

        if dist_sq <= 2.25 {
            let is_baby = mob.is_baby_mob();
            let is_screamer = mob
                .cast_any()
                .downcast_ref::<GoatEntity>()
                .is_some_and(GoatEntity::is_screaming);
            let damage = if is_baby { 1.0 } else { 2.0 };
            let knockback = if is_baby { 1.0 } else { 2.5 };

            let _ = target.damage(
                mob.get_entity(),
                damage,
                pumpkin_data::damage::DamageType::MOB_ATTACK,
            );

            // Apply knockback
            let dx = target_pos.x - mob_pos.x;
            let dz = target_pos.z - mob_pos.z;
            let len = dx.hypot(dz).max(0.001);
            let target_ent = target.get_entity();
            let mut vel = target_ent.velocity.load();
            vel.x += (dx / len) * knockback;
            vel.y += 0.3;
            vel.z += (dz / len) * knockback;
            target_ent.velocity.store(vel);

            let world = entity.world.load();
            let impact_sound = if is_screamer {
                Sound::EntityGoatScreamingRamImpact
            } else {
                Sound::EntityGoatRamImpact
            };
            world.play_sound(impact_sound, SoundCategory::Neutral, &mob_pos);

            self.target = None;
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let world = entity.world.load();
        world.send_entity_status(entity, EntityStatus::EndRam, ActorEventID::None);

        let mut rng = rand::rng();
        let is_screamer = mob
            .cast_any()
            .downcast_ref::<GoatEntity>()
            .is_some_and(GoatEntity::is_screaming);
        self.cooldown = if is_screamer {
            rng.random_range(100..300)
        } else {
            rng.random_range(600..6000)
        };
        self.target = None;
    }

    fn controls(&self) -> Controls {
        Controls::MOVE
    }
}

// goat: long jump goal (GoatAi.initLongJumpActivity)
pub struct GoatLongJumpGoal {
    cooldown: i32,
}

impl Default for GoatLongJumpGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl GoatLongJumpGoal {
    #[must_use]
    pub fn new() -> Self {
        let mut rng = rand::rng();
        Self {
            cooldown: rng.random_range(600..1200),
        }
    }
}

impl Goal for GoatLongJumpGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }

        let entity = &mob.get_mob_entity().living_entity.entity;
        if !entity.on_ground.load(Ordering::Relaxed) {
            return false;
        }

        let mut rng = rand::rng();
        let is_screamer = mob
            .cast_any()
            .downcast_ref::<GoatEntity>()
            .is_some_and(GoatEntity::is_screaming);

        // Perform long jump
        let yaw_rad = f64::from(entity.yaw.load()).to_radians();
        let mut vel = entity.velocity.load();
        vel.y = 0.85;
        vel.x += -yaw_rad.sin() * 0.75;
        vel.z += yaw_rad.cos() * 0.75;
        entity.velocity.store(vel);

        let world = entity.world.load();
        let jump_sound = if is_screamer {
            Sound::EntityGoatScreamingLongJump
        } else {
            Sound::EntityGoatLongJump
        };
        world.play_sound(jump_sound, SoundCategory::Neutral, &entity.pos.load());

        self.cooldown = rng.random_range(600..1200);
        false
    }

    fn should_continue(&mut self, _mob: &dyn Mob) -> bool {
        false
    }

    fn controls(&self) -> Controls {
        Controls::JUMP
    }
}

pub struct GoatEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub is_screaming: AtomicBool,
    pub has_left_horn: AtomicBool,
    pub has_right_horn: AtomicBool,
}

impl GoatEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let goat = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            is_screaming: AtomicBool::new(false),
            has_left_horn: AtomicBool::new(true),
            has_right_horn: AtomicBool::new(true),
        };
        let mob_arc = Arc::new(goat);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // goat: vanilla goal priorities (GoatAi.getActivities)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(2.0));
            goal_selector.add_goal(2, Box::new(GoatRamGoal::new()));
            goal_selector.add_goal(3, Box::new(GoatLongJumpGoal::new()));
            goal_selector.add_goal(4, BreedGoal::new(1.0));
            goal_selector.add_goal(5, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(6, Box::new(FollowParentGoal::new(1.25)));
            goal_selector.add_goal(7, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                8,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(9, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn is_screaming(&self) -> bool {
        self.is_screaming.load(Ordering::Relaxed)
    }

    pub fn set_screaming(&self, screaming: bool) {
        self.is_screaming.store(screaming, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_IS_SCREAMING_GOAT,
            screaming,
        );
    }

    #[must_use]
    pub fn has_left_horn(&self) -> bool {
        self.has_left_horn.load(Ordering::Relaxed)
    }

    pub fn set_has_left_horn(&self, has_horn: bool) {
        self.has_left_horn.store(has_horn, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_LEFT_HORN,
            has_horn,
        );
    }

    #[must_use]
    pub fn has_right_horn(&self) -> bool {
        self.has_right_horn.load(Ordering::Relaxed)
    }

    pub fn set_has_right_horn(&self, has_horn: bool) {
        self.has_right_horn.store(has_horn, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_RIGHT_HORN,
            has_horn,
        );
    }

    // goat: drop horn when hitting a hard block (Goat.dropHorn)
    pub fn drop_horn(&self) -> bool {
        if self.is_baby() {
            return false;
        }

        let has_left = self.has_left_horn();
        let has_right = self.has_right_horn();
        if !has_left && !has_right {
            return false;
        }

        let mut rng = rand::rng();
        let drop_left = if !has_left {
            false
        } else if !has_right {
            true
        } else {
            rng.random_bool(0.5)
        };

        if drop_left {
            self.set_has_left_horn(false);
        } else {
            self.set_has_right_horn(false);
        }

        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        world.drop_stack(&pos.to_block_pos(), ItemStack::new(1, &Item::GOAT_HORN));
        world.play_sound(Sound::EntityGoatHornBreak, SoundCategory::Neutral, &pos);
        true
    }
}

impl AgeableMob for GoatEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for GoatEntity {
    // goat: (screaming) eat sound (Goat.playEatingSound)
    fn eating_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingEat
        } else {
            Sound::EntityGoatEat
        })
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_GOAT_FOOD)
            || TEMPT_ITEMS.iter().any(|i| i.id == item_stack.item.id)
    }

    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby_base = from_type(
            &EntityType::GOAT,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        if let Some(baby) = baby_base.cast_any().downcast_ref::<Self>() {
            let mut rng = rand::rng();
            // goat: screaming inheritance (50% from either parent)
            let other_goat = partner.cast_any().downcast_ref::<Self>();
            let screaming = if let Some(p2) = other_goat {
                if rng.random_bool(0.5) {
                    self.is_screaming()
                } else {
                    p2.is_screaming()
                }
            } else {
                rng.random_range(0.0..1.0) < 0.02
            };
            baby.set_screaming(screaming);
            baby.get_entity().age.store(-24000, Ordering::Relaxed);
        }
        baby_base
    }
}

impl CustomSound for GoatEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingDeath
        } else {
            Sound::EntityGoatDeath
        })
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingHurt
        } else {
            Sound::EntityGoatHurt
        })
    }
}

impl Mob for GoatEntity {
    // goat: reduced fall damage by 10 (Goat.GOAT_FALL_DAMAGE_REDUCTION)
    fn max_fall_distance(&self) -> i32 {
        13
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(if self.is_screaming() {
            Sound::EntityGoatScreamingAmbient
        } else {
            Sound::EntityGoatAmbient
        })
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityGoatStep, 0.15, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let result = self.finalize_spawn_super(group_data);
        let mut rng = rand::rng();
        // goat: 2% screaming goat on natural spawn (Goat.GOAT_SCREAMING_CHANCE)
        self.set_screaming(rng.random_range(0.0..1.0) < 0.02);
        // goat: 10% chance single horn on adult spawn (Goat.UNIHORN_CHANCE)
        if !self.is_baby() && rng.random_range(0.0..1.0) < 0.1 {
            if rng.random_bool(0.5) {
                self.set_has_left_horn(false);
            } else {
                self.set_has_right_horn(false);
            }
        }
        result
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("IsScreamingGoat", self.is_screaming());
        nbt.put_bool("HasLeftHorn", self.has_left_horn());
        nbt.put_bool("HasRightHorn", self.has_right_horn());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(screaming) = nbt.get_bool("IsScreamingGoat") {
            self.set_screaming(screaming);
        }
        if let Some(left) = nbt.get_bool("HasLeftHorn") {
            self.set_has_left_horn(left);
        }
        if let Some(right) = nbt.get_bool("HasRightHorn") {
            self.set_has_right_horn(right);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::goat::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_IS_SCREAMING_GOAT,
            self.is_screaming(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_LEFT_HORN,
            self.has_left_horn(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::goat::DATA_HAS_RIGHT_HORN,
            self.has_right_horn(),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();

        // goat: milking with bucket (Goat.mobInteract)
        if item == &Item::BUCKET && !self.is_baby() {
            let entity = self.get_entity();
            let world = entity.world.load();
            let sound = if self.is_screaming() {
                Sound::EntityGoatScreamingMilk
            } else {
                Sound::EntityGoatMilk
            };
            world.play_sound(sound, SoundCategory::Neutral, &entity.pos.load());
            fill_bucket_stack(item_stack, player, &Item::MILK_BUCKET);
            return true;
        }

        let ambient_sound = if self.is_screaming() {
            Sound::EntityGoatScreamingAmbient
        } else {
            Sound::EntityGoatAmbient
        };
        self.animal_interact(player, item_stack, ambient_sound)
    }
}
