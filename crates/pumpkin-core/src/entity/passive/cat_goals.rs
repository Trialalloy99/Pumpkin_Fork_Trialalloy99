use std::sync::Arc;

use pumpkin_data::{
    Block,
    block_properties::{BedPart, FurnaceLikeProperties, WhiteBedLikeProperties},
    entity::EntityType,
    item_stack::ItemStack,
    tag::{self, Taggable},
};
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use rand::RngExt;

use super::{cat::CatEntity, tamable::TamableAnimal};
use crate::block::entities::chest::ChestBlockEntity;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        Controls, Goal, ParentHandle,
        move_to_target_pos::{MoveToTargetPos, MoveToTargetPosGoal},
        to_goal_ticks,
    },
    item::ItemEntity,
    mob::Mob,
    player::Player,
};
use crate::world::World;

// cat: CatLieOnBlockGoal.nextStartTick
const LIE_NEXT_START_TICK: i32 = 40;
const LIE_SPEED_MODIFIER: f64 = 1.1;
const LIE_SEARCH_RANGE: i32 = 8;
const LIE_VERTICAL_SEARCH_RANGE: i32 = 6;
const LIE_VERTICAL_SEARCH_START: i32 = -2;
const SIT_SPEED_MODIFIER: f64 = 0.8;
const SIT_SEARCH_RANGE: i32 = 8;
const SIT_VERTICAL_SEARCH_RANGE: i32 = 1;
const RELAX_SPEED_MODIFIER: f64 = 1.1;
const RELAX_MAX_OWNER_DISTANCE_SQ: f64 = 100.0;
const RELAX_REACHED_DISTANCE_SQ: f64 = 2.5;
const RELAX_LIE_DELAY: i32 = 16;
const MIN_SLEEP_TICKS_FOR_GIFT: u8 = 100;

fn cat(mob: &dyn Mob) -> Option<&CatEntity> {
    mob.cast_any().downcast_ref::<CatEntity>()
}

fn is_empty_block(world: &World, pos: &BlockPos) -> bool {
    world.get_block_state(pos).is_air()
}

// cat: CatLieOnBlockGoal.isValidTarget
struct LieTarget;
static LIE_TARGET: LieTarget = LieTarget;

impl MoveToTargetPos for LieTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        is_empty_block(&world, &block_pos.up())
            && world
                .get_block(&block_pos)
                .has_tag(&tag::Block::MINECRAFT_CATS_CAN_LIE_ON)
    }
}

// cat: CatSitOnBlockGoal.isValidTarget with BLOCK_TYPE_VALIDATION
struct SitTarget;
static SIT_TARGET: SitTarget = SitTarget;

impl MoveToTargetPos for SitTarget {
    fn is_target_pos(&self, world: Arc<World>, block_pos: BlockPos) -> bool {
        if !is_empty_block(&world, &block_pos.up()) {
            return false;
        }
        let (block, state_id) = world.get_block_and_state_id(&block_pos);
        if !block.has_tag(&tag::Block::MINECRAFT_CATS_CAN_SIT_ON) {
            return false;
        }
        if block == &Block::CHEST {
            return world
                .get_block_entity(&block_pos)
                .and_then(|entity| {
                    entity
                        .as_any()
                        .downcast_ref::<ChestBlockEntity>()
                        .map(|chest| chest.get_viewer_count() < 1)
                })
                .unwrap_or(true);
        }
        if block == &Block::FURNACE {
            return FurnaceLikeProperties::from_state_id(state_id).lit;
        }
        if block.has_tag(&tag::Block::MINECRAFT_BEDS) {
            return WhiteBedLikeProperties::from_state_id(state_id).part != BedPart::Head;
        }
        true
    }
}

// cat: Cat.CatLieOnBlockGoal
pub struct CatLieOnBlockGoal {
    inner: MoveToTargetPosGoal<LieTarget>,
}

impl CatLieOnBlockGoal {
    #[must_use]
    pub fn new() -> Self {
        let mut inner = MoveToTargetPosGoal::new(
            // SAFETY: LIE_TARGET is a static.
            unsafe { ParentHandle::new(&LIE_TARGET) },
            LIE_SPEED_MODIFIER,
            LIE_SEARCH_RANGE,
            LIE_VERTICAL_SEARCH_RANGE,
        );
        inner.lowest_y = LIE_VERTICAL_SEARCH_START;
        Self { inner }
    }
}

impl Default for CatLieOnBlockGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for CatLieOnBlockGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(cat) = cat(mob) else { return false };
        if !(cat.is_tame() && !cat.is_ordered_to_sit() && !cat.is_lying()) {
            return false;
        }
        if self.inner.cooldown > 0 {
            self.inner.cooldown -= 1;
            return false;
        }
        self.inner.cooldown = to_goal_ticks(LIE_NEXT_START_TICK);
        self.inner.find_target_pos(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
        if let Some(cat) = cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(cat) = cat(mob) {
            cat.set_lying(false);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
        let Some(cat) = cat(mob) else { return };
        cat.set_in_sitting_pose(false);
        if !self.inner.is_reached_target() {
            cat.set_lying(false);
        } else if !cat.is_lying() {
            cat.set_lying(true);
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::JUMP | Controls::MOVE
    }
}

// cat: Cat.CatSitOnBlockGoal
pub struct CatSitOnBlockGoal {
    inner: MoveToTargetPosGoal<SitTarget>,
}

impl CatSitOnBlockGoal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: MoveToTargetPosGoal::new(
                // SAFETY: SIT_TARGET is a static.
                unsafe { ParentHandle::new(&SIT_TARGET) },
                SIT_SPEED_MODIFIER,
                SIT_SEARCH_RANGE,
                SIT_VERTICAL_SEARCH_RANGE,
            ),
        }
    }
}

impl Default for CatSitOnBlockGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for CatSitOnBlockGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        cat(mob).is_some_and(|cat| cat.is_tame() && !cat.is_ordered_to_sit())
            && self.inner.can_start(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
        if let Some(cat) = cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(cat) = cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.inner.tick(mob);
        if let Some(cat) = cat(mob) {
            cat.set_in_sitting_pose(self.inner.is_reached_target());
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::JUMP | Controls::MOVE
    }
}

// cat: Cat.CatRelaxOnOwnerGoal
pub struct CatRelaxOnOwnerGoal {
    owner: Option<Arc<Player>>,
    goal_pos: Option<BlockPos>,
    on_bed_ticks: i32,
}

impl CatRelaxOnOwnerGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            owner: None,
            goal_pos: None,
            on_bed_ticks: 0,
        }
    }

    // cat: spaceIsOccupied, another cat lying or relaxing near the goal
    fn space_is_occupied(mob: &dyn Mob, goal_pos: &BlockPos) -> bool {
        let world = mob.get_entity().world.load();
        let center = goal_pos.to_centered_f64();
        let area = BoundingBox::new(
            Vector3::new(center.x - 2.5, center.y - 2.5, center.z - 2.5),
            Vector3::new(center.x + 2.5, center.y + 2.5, center.z + 2.5),
        );
        let own_id = mob.get_entity().entity_id;
        world.get_entities_at_box(&area).iter().any(|entity| {
            entity.get_entity().entity_type == &EntityType::CAT
                && entity.get_entity().entity_id != own_id
                && entity
                    .get_mob()
                    .and_then(|other| other.cast_any().downcast_ref::<CatEntity>())
                    .is_some_and(|other| other.is_lying() || other.is_relax_state_one())
        })
    }

    fn move_to_goal(mob: &dyn Mob, goal_pos: &BlockPos) {
        let mut navigator = mob
            .get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        navigator.move_to_coords(
            f64::from(goal_pos.0.x),
            f64::from(goal_pos.0.y),
            f64::from(goal_pos.0.z),
            RELAX_SPEED_MODIFIER,
            &mob.get_mob_entity().living_entity,
        );
    }

    // cat: giveMorningGift, teleport a few blocks and drop CAT_MORNING_GIFT loot
    fn give_morning_gift(mob: &dyn Mob) {
        let entity = mob.get_entity();
        let world = entity.world.load_full();
        let origin = entity.block_pos.load();
        let (dx, dy, dz) = {
            let mut rng = mob.get_random();
            (
                rng.random_range(0..11) - 5,
                rng.random_range(0..5) - 2,
                rng.random_range(0..11) - 5,
            )
        };
        // cat: randomTeleport(avoiding CAT_DOES_NOT_TELEPORT_TO) around the leash holder or itself
        let holder = entity
            .leashed_to
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let center = holder.map_or(origin, |holder| holder.get_entity().block_pos.load());
        entity.random_teleport(
            f64::from(center.0.x + dx),
            f64::from(center.0.y + dy),
            f64::from(center.0.z + dz),
            false,
            |state| {
                pumpkin_data::Block::from_state_id(state.id)
                    .has_tag(&tag::Block::MINECRAFT_CAT_DOES_NOT_TELEPORT_TO)
            },
        );
        mob.get_mob_entity().steered().navigator().stop();
        let cat_pos = entity.block_pos.load();
        let yaw = f64::from(entity.body_yaw.load()).to_radians();
        let seed = rand::random::<i64>();
        for stack in crate::world::loot::generate_loot(
            &pumpkin_data::loot_table::GAMEPLAY_CAT_MORNING_GIFT,
            seed,
        ) {
            spawn_gift(
                &world,
                Vector3::new(
                    f64::from(cat_pos.0.x) - yaw.sin(),
                    f64::from(cat_pos.0.y),
                    f64::from(cat_pos.0.z) + yaw.cos(),
                ),
                stack,
            );
        }
    }
}

fn spawn_gift(world: &Arc<World>, position: Vector3<f64>, stack: ItemStack) {
    let item = ItemEntity::new(
        Entity::new(world.clone(), position, &EntityType::ITEM),
        stack,
    );
    world.spawn_entity(Arc::new(item));
}

impl Default for CatRelaxOnOwnerGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for CatRelaxOnOwnerGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(cat) = cat(mob) else { return false };
        if !cat.is_tame() || cat.is_ordered_to_sit() {
            return false;
        }
        let Some(owner_uuid) = cat.get_owner() else {
            return false;
        };
        let world = mob.get_entity().world.load_full();
        let Some(owner) = world.get_player_by_uuid(owner_uuid) else {
            return false;
        };
        self.owner = Some(owner.clone());
        if !owner.is_sleeping() {
            return false;
        }
        let cat_pos = mob.get_entity().pos.load();
        if cat_pos.squared_distance_to_vec(&owner.get_entity().pos.load())
            > RELAX_MAX_OWNER_DISTANCE_SQ
        {
            return false;
        }
        let owner_pos = owner.get_entity().block_pos.load();
        let (block, state_id) = world.get_block_and_state_id(&owner_pos);
        if !block.has_tag(&tag::Block::MINECRAFT_CATS_CAN_LIE_ON) {
            return false;
        }
        let goal_pos = if block.has_tag(&tag::Block::MINECRAFT_BEDS) {
            let facing = WhiteBedLikeProperties::from_state_id(state_id).facing;
            let behind = pumpkin_data::HorizontalFacingExt::to_block_direction(&facing).opposite();
            owner_pos.offset(behind.to_offset())
        } else {
            owner_pos
        };
        self.goal_pos = Some(goal_pos);
        !Self::space_is_occupied(mob, &goal_pos)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let Some(cat) = cat(mob) else { return false };
        cat.is_tame()
            && !cat.is_ordered_to_sit()
            && self.owner.as_ref().is_some_and(|owner| owner.is_sleeping())
            && self
                .goal_pos
                .as_ref()
                .is_some_and(|goal_pos| !Self::space_is_occupied(mob, goal_pos))
    }

    fn start(&mut self, mob: &dyn Mob) {
        if let Some(goal_pos) = self.goal_pos {
            if let Some(cat) = cat(mob) {
                cat.set_in_sitting_pose(false);
            }
            Self::move_to_goal(mob, &goal_pos);
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        let Some(cat) = cat(mob) else { return };
        cat.set_lying(false);
        let world = mob.get_entity().world.load_full();
        let gift_chance = world.environment_attributes().get_dimension_value_f32(
            pumpkin_data::environment_attribute::EnvironmentAttribute::GameplayCatWakingUpGiftChance,
        );
        if self.owner.as_ref().is_some_and(|owner| {
            owner
                .sleeping_since
                .load()
                .is_some_and(|ticks| ticks >= MIN_SLEEP_TICKS_FOR_GIFT)
        }) && rand::random::<f32>() < gift_chance
        {
            Self::give_morning_gift(mob);
        }
        self.on_bed_ticks = 0;
        cat.set_relax_state_one(false);
        mob.get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop();
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let (Some(owner), Some(goal_pos)) = (self.owner.clone(), self.goal_pos) else {
            return;
        };
        let Some(cat) = cat(mob) else { return };
        cat.set_in_sitting_pose(false);
        Self::move_to_goal(mob, &goal_pos);
        let distance_sq = mob
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&owner.get_entity().pos.load());
        if distance_sq < RELAX_REACHED_DISTANCE_SQ {
            self.on_bed_ticks += 1;
            if self.on_bed_ticks > to_goal_ticks(RELAX_LIE_DELAY) {
                cat.set_lying(true);
                cat.set_relax_state_one(false);
            } else {
                mob.get_mob_entity()
                    .look_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .look_at_with_range(
                        owner.get_entity().pos.load().x,
                        owner.get_entity().pos.load().y
                            + f64::from(owner.get_entity().height()) * 0.5,
                        owner.get_entity().pos.load().z,
                        45.0,
                        45.0,
                    );
                cat.set_relax_state_one(true);
            }
        } else {
            cat.set_lying(false);
        }
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}
