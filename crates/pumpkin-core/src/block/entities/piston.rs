use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::block_properties::{PistonHeadLikeProperties, PistonType};
use pumpkin_data::{Block, BlockDirection, BlockState, FacingExt};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};

use crate::block::blocks::piston::piston::PistonProps;
use crate::entity::EntityBase;
use crate::world::{BlockFlags, World};

use super::BlockEntity;

thread_local! {
    // piston: direction of the piston currently moving an entity (PistonMovingBlockEntity.NOCLIP)
    static NOCLIP: Cell<Option<BlockDirection>> = const { Cell::new(None) };
}

#[must_use]
pub(crate) fn noclip_direction() -> Option<BlockDirection> {
    NOCLIP.get()
}

// piston: head shape of a moving piston arm (PistonHeadBlock.SHORT)
fn head_state(facing: BlockDirection, sticky: bool, short: bool) -> &'static BlockState {
    let mut props = PistonHeadLikeProperties::default(&Block::PISTON_HEAD);
    props.facing = facing.to_facing();
    props.r#type = if sticky {
        PistonType::Sticky
    } else {
        PistonType::Normal
    };
    props.short = short;
    BlockState::from_id(props.to_state_id(&Block::PISTON_HEAD))
}

// entity: Entity.getPistonPushReaction == IGNORE_ENTITY
fn ignored_by_piston(entity: &dyn EntityBase) -> bool {
    let id = entity.get_entity().entity_type.id;
    if id == pumpkin_data::entity::EntityType::ARMOR_STAND.id {
        return entity
            .cast_any()
            .downcast_ref::<crate::entity::decoration::armor_stand::ArmorStandEntity>()
            .is_some_and(crate::entity::decoration::armor_stand::ArmorStandEntity::is_marker);
    }
    [
        pumpkin_data::entity::EntityType::INTERACTION.id,
        pumpkin_data::entity::EntityType::BLOCK_DISPLAY.id,
        pumpkin_data::entity::EntityType::ITEM_DISPLAY.id,
        pumpkin_data::entity::EntityType::TEXT_DISPLAY.id,
        pumpkin_data::entity::EntityType::AREA_EFFECT_CLOUD.id,
        pumpkin_data::entity::EntityType::OMINOUS_ITEM_SPAWNER.id,
        pumpkin_data::entity::EntityType::MARKER.id,
    ]
    .contains(&id)
}

pub struct PistonBlockEntity {
    pub position: BlockPos,
    pub pushed_block_state: &'static BlockState,
    pub facing: BlockDirection,
    pub current_progress: AtomicCell<f32>,
    pub last_progress: AtomicCell<f32>,
    // piston: game time of the last tick (PistonMovingBlockEntity.lastTicked)
    pub last_ticked: AtomicI64,
    pub extending: bool,
    pub source: bool,
}

impl PistonBlockEntity {
    pub const ID: &'static str = "minecraft:piston";

    const fn movement_direction(&self) -> BlockDirection {
        if self.extending {
            self.facing
        } else {
            self.facing.opposite()
        }
    }

    /// Vanilla's `getAmountExtended`: how far back from the block's final position
    /// the visual is at a given animation progress. Negative for extending.
    fn amount_extended(&self, progress: f32) -> f32 {
        if self.extending {
            progress - 1.0
        } else {
            1.0 - progress
        }
    }

    fn dir_vec(dir: BlockDirection, scale: f64) -> Vector3<f64> {
        let off = dir.to_offset();
        Vector3::new(
            f64::from(off.x) * scale,
            f64::from(off.y) * scale,
            f64::from(off.z) * scale,
        )
    }

    /// Ports vanilla `PistonBlockEntity.pushEntities`: pushes entities whose
    /// bounding box intersects the moving block's swept volume during this tick.
    fn push_entities(&self, world: &Arc<World>, new_progress: f32) {
        let last = self.last_progress.load();
        let delta = f64::from(new_progress - last);
        if delta <= 0.0 {
            return;
        }

        let motion_dir = self.movement_direction();
        let amount = f64::from(self.amount_extended(last));

        // piston: collision shape of the moved block, head for a retracting base (PistonMovingBlockEntity.getCollisionRelatedBlockState)
        let origin = self.position.0.to_f64() + Self::dir_vec(self.facing, amount);
        let shapes: Vec<BoundingBox> = self
            .collision_related_state()
            .get_block_collision_shapes()
            .map(|shape| shape.shift(origin))
            .collect();
        let Some(bounds) = shapes.iter().copied().reduce(Self::minmax) else {
            return;
        };

        let swept = Self::minmax(Self::movement_area(bounds, motion_dir, delta), bounds);
        let bounce = self.pushed_block_state.id.to_block() == &Block::SLIME_BLOCK;

        for entity in world.get_entities_at_box(&swept) {
            let e = entity.get_entity();
            // Player movement is client-authoritative, the client runs this push itself
            if entity.get_player().is_some() || ignored_by_piston(entity.as_ref()) {
                continue;
            }

            // piston: slime block launches along the push axis (moveCollidedEntities causeBounce)
            if bounce {
                let mut velocity = e.velocity.load();
                let step = Self::dir_vec(motion_dir, 1.0);
                match motion_dir {
                    BlockDirection::East | BlockDirection::West => velocity.x = step.x,
                    BlockDirection::Up | BlockDirection::Down => velocity.y = step.y,
                    BlockDirection::North | BlockDirection::South => velocity.z = step.z,
                }
                e.velocity.store(velocity);
                e.velocity_dirty.store(true, Ordering::SeqCst);
            }

            let entity_aabb = e.bounding_box.load();
            let mut intersection = 0.0f64;
            for shape in &shapes {
                let moving = Self::movement_area(*shape, motion_dir, delta);
                if moving.intersects(&entity_aabb) {
                    intersection =
                        intersection.max(Self::intersection_size(moving, motion_dir, entity_aabb));
                    if intersection >= delta {
                        break;
                    }
                }
            }
            if intersection <= 0.0 {
                continue;
            }
            let push_amount = intersection.min(delta) + 0.01;
            Self::move_entity(motion_dir, entity.as_ref(), motion_dir, push_amount);

            // For a retracting head, vanilla also shoves the entity OUT of the
            // piston body cube. Without this, entities get pulled into the
            // piston and "stick" to it (looks like a sticky-piston drag).
            // For a retract-head BE, `self.position` already IS the piston
            // block position (it replaces the piston during animation).
            if !self.extending && self.source {
                Self::push_out_of_piston_body(entity.as_ref(), &self.position, motion_dir, delta);
            }
        }

        self.move_stuck_entities(world, amount, delta);
    }

    // piston: slab swept in front of the moving face (PistonMath.getMovementArea)
    fn movement_area(aabb: BoundingBox, dir: BlockDirection, delta: f64) -> BoundingBox {
        let mut area = aabb;
        match dir {
            BlockDirection::East => {
                area.min.x = aabb.max.x;
                area.max.x = aabb.max.x + delta;
            }
            BlockDirection::West => {
                area.max.x = aabb.min.x;
                area.min.x = aabb.min.x - delta;
            }
            BlockDirection::Up => {
                area.min.y = aabb.max.y;
                area.max.y = aabb.max.y + delta;
            }
            BlockDirection::Down => {
                area.max.y = aabb.min.y;
                area.min.y = aabb.min.y - delta;
            }
            BlockDirection::South => {
                area.min.z = aabb.max.z;
                area.max.z = aabb.max.z + delta;
            }
            BlockDirection::North => {
                area.max.z = aabb.min.z;
                area.min.z = aabb.min.z - delta;
            }
        }
        area
    }

    // piston: box around both (AABB.minmax)
    const fn minmax(a: BoundingBox, b: BoundingBox) -> BoundingBox {
        BoundingBox::new(
            Vector3::new(
                a.min.x.min(b.min.x),
                a.min.y.min(b.min.y),
                a.min.z.min(b.min.z),
            ),
            Vector3::new(
                a.max.x.max(b.max.x),
                a.max.y.max(b.max.y),
                a.max.z.max(b.max.z),
            ),
        )
    }

    // piston: honey block carries entities standing on it sideways (PistonMovingBlockEntity.moveStuckEntities)
    fn move_stuck_entities(&self, world: &Arc<World>, amount: f64, delta: f64) {
        if self.pushed_block_state.id.to_block() != &Block::HONEY_BLOCK {
            return;
        }
        let motion_dir = self.movement_direction();
        if matches!(motion_dir, BlockDirection::Up | BlockDirection::Down) {
            return;
        }
        let sticky_top = self
            .pushed_block_state
            .get_block_collision_shapes()
            .map(|shape| shape.max.y)
            .fold(0.0f64, f64::max);
        let origin = self.position.0.to_f64() + Self::dir_vec(self.facing, amount);
        let area = BoundingBox::new(
            Vector3::new(origin.x, origin.y + sticky_top, origin.z),
            Vector3::new(origin.x + 1.0, origin.y + 1.500_001, origin.z + 1.0),
        );
        for entity in world.get_entities_at_box(&area) {
            let e = entity.get_entity();
            if entity.get_player().is_some()
                || ignored_by_piston(entity.as_ref())
                || !e.on_ground.load(Ordering::Relaxed)
            {
                continue;
            }
            let pos = e.pos.load();
            let supported = e.supporting_block_pos.load() == Some(self.position);
            if supported
                || (pos.x >= area.min.x
                    && pos.x <= area.max.x
                    && pos.z >= area.min.z
                    && pos.z <= area.max.z)
            {
                Self::move_entity(motion_dir, entity.as_ref(), motion_dir, delta);
            }
        }
    }

    /// Vanilla `getIntersectionSize`: how much `entity` overlaps `swept` along
    /// `motion_dir`. Positive means the entity is in the path of the moving block.
    fn intersection_size(
        swept: BoundingBox,
        motion_dir: BlockDirection,
        entity: BoundingBox,
    ) -> f64 {
        match motion_dir {
            BlockDirection::East => swept.max.x - entity.min.x,
            BlockDirection::West => entity.max.x - swept.min.x,
            BlockDirection::Up => swept.max.y - entity.min.y,
            BlockDirection::Down => entity.max.y - swept.min.y,
            BlockDirection::South => swept.max.z - entity.min.z,
            BlockDirection::North => entity.max.z - swept.min.z,
        }
    }

    // piston: collide-aware push with NOCLIP set (PistonMovingBlockEntity.moveEntityByPiston)
    fn move_entity(
        piston_dir: BlockDirection,
        entity: &dyn EntityBase,
        movement: BlockDirection,
        distance: f64,
    ) {
        NOCLIP.set(Some(piston_dir));
        entity
            .get_entity()
            .move_by_piston(entity, Self::dir_vec(movement, distance));
        NOCLIP.set(None);
    }

    // piston: keep entities out of the base while the head retracts (fixEntityWithinPistonBase)
    fn push_out_of_piston_body(
        entity: &dyn EntityBase,
        piston_pos: &BlockPos,
        motion_dir: BlockDirection,
        amount: f64,
    ) {
        let body_aabb = BoundingBox::from_block(piston_pos);
        let entity_aabb = entity.get_entity().bounding_box.load();
        if !body_aabb.intersects(&entity_aabb) {
            return;
        }
        let back = motion_dir.opposite();
        let e = Self::intersection_size(body_aabb, back, entity_aabb) + 0.01;
        let f = Self::intersection_size(
            body_aabb,
            back,
            Self::aabb_intersection(body_aabb, entity_aabb),
        ) + 0.01;
        if (e - f).abs() < 0.01 {
            let distance = e.min(amount) + 0.01;
            Self::move_entity(motion_dir, entity, back, distance);
        }
    }

    fn is_piston_base(&self) -> bool {
        let block = self.pushed_block_state.id.to_block();
        block == &Block::PISTON || block == &Block::STICKY_PISTON
    }

    // piston: PistonMovingBlockEntity.getCollisionRelatedBlockState
    fn collision_related_state(&self) -> &'static BlockState {
        if !self.extending && self.source && self.is_piston_base() {
            let base = PistonProps::from_state_id(self.pushed_block_state.id);
            let sticky = self.pushed_block_state.id.to_block() == &Block::STICKY_PISTON;
            return head_state(
                base.facing.to_block_direction(),
                sticky,
                self.current_progress.load() > 0.25,
            );
        }
        self.pushed_block_state
    }

    /// Entity collision boxes in world space (`PistonMovingBlockEntity.getCollisionShape`).
    #[must_use]
    pub(crate) fn collision_boxes(&self) -> Vec<BoundingBox> {
        let progress = self.current_progress.load();
        let block_origin = self.position.0.to_f64();
        let mut boxes = Vec::new();
        // piston: base left behind while the head retracts
        if !self.extending && self.source && self.is_piston_base() {
            let mut props = PistonProps::from_state_id(self.pushed_block_state.id);
            props.extended = true;
            let base = self.pushed_block_state.id.to_block();
            boxes.extend(
                BlockState::from_id(props.to_state_id(base))
                    .get_block_collision_shapes()
                    .map(|shape| shape.shift(block_origin)),
            );
        }
        if progress < 1.0 && noclip_direction() == Some(self.movement_direction()) {
            return boxes;
        }
        let state = if self.source {
            let short = self.extending != (1.0 - progress < 0.25);
            head_state(self.facing, false, short)
        } else {
            self.pushed_block_state
        };
        let origin =
            block_origin + Self::dir_vec(self.facing, f64::from(self.amount_extended(progress)));
        boxes.extend(
            state
                .get_block_collision_shapes()
                .map(|shape| shape.shift(origin)),
        );
        boxes
    }

    const fn aabb_intersection(a: BoundingBox, b: BoundingBox) -> BoundingBox {
        BoundingBox::new(
            Vector3::new(
                a.min.x.max(b.min.x),
                a.min.y.max(b.min.y),
                a.min.z.max(b.min.z),
            ),
            Vector3::new(
                a.max.x.min(b.max.x),
                a.max.y.min(b.max.y),
                a.max.z.min(b.max.z),
            ),
        )
    }

    pub fn finish(&self, world: &Arc<World>) {
        if self.last_progress.load() < 1.0 {
            let pos = self.position;
            world.remove_block_entity(&pos);
            if world.get_block(&pos) == &Block::MOVING_PISTON {
                let state = if self.source {
                    Block::AIR.default_state.id
                } else {
                    world.update_from_neighbor_shapes(self.pushed_block_state.id, &pos)
                };
                world.set_block_state(&pos, state, BlockFlags::NOTIFY_ALL);
                world.update_neighbors(&pos, None);
            }
        }
    }
}

const FACING: &str = "facing";
const LAST_PROGRESS: &str = "progress";
const EXTENDING: &str = "extending";
const SOURCE: &str = "source";
const BLOCK_STATE: &str = "blockState";

impl BlockEntity for PistonBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn tick(&self, world: &Arc<World>) {
        self.last_ticked
            .store(world.get_world_age(), Ordering::Relaxed);
        let current_progress = self.current_progress.load();
        self.last_progress.store(current_progress);
        if current_progress >= 1.0 {
            let pos = self.position;
            world.remove_block_entity(&pos);
            if world.get_block(&pos) == &Block::MOVING_PISTON {
                let moved = self.pushed_block_state;
                let updated = world.update_from_neighbor_shapes(moved.id, &pos);
                if BlockState::from_id(updated).is_air() {
                    // piston: Block.updateOrDestroy, moved block destroyed with drops
                    world.set_block_state(
                        &pos,
                        moved.id,
                        BlockFlags::FORCE_STATE | BlockFlags::MOVED,
                    );
                    if !moved.is_air() {
                        world.break_block(&pos, None, BlockFlags::NOTIFY_ALL);
                    }
                } else {
                    // piston: waterlogged state is dropped when the block settles
                    let updated = updated
                        .to_block()
                        .set_waterlogged(updated, false)
                        .unwrap_or(updated);
                    world.set_block_state(
                        &pos,
                        updated,
                        BlockFlags::NOTIFY_ALL | BlockFlags::MOVED,
                    );
                    world.update_neighbors(&pos, None);
                }
            }
            return;
        }
        let new_progress = (current_progress + 0.5).min(1.0);
        self.push_entities(world, new_progress);
        self.current_progress.store(new_progress);
    }

    fn from_nbt(nbt: &pumpkin_nbt::compound::NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let pushed_block_state = nbt
            .get_compound(BLOCK_STATE)
            .and_then(crate::block::read_block_state_nbt)
            .map_or(Block::AIR.default_state, BlockState::from_id);
        let facing = nbt.get_byte(FACING).unwrap_or(0);
        let last_progress = nbt.get_float(LAST_PROGRESS).unwrap_or(0.0);
        let extending = nbt.get_bool(EXTENDING).unwrap_or(false);
        let source = nbt.get_bool(SOURCE).unwrap_or(false);
        Self {
            pushed_block_state,
            position,
            facing: BlockDirection::from_index(facing as u8).unwrap_or(BlockDirection::Down),
            current_progress: last_progress.into(),
            last_progress: last_progress.into(),
            last_ticked: AtomicI64::new(0),
            extending,
            source,
        }
    }

    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_compound(
            BLOCK_STATE,
            crate::block::write_block_state_nbt(self.pushed_block_state.id),
        );
        nbt.put_byte(FACING, self.facing.to_index() as i8);
        nbt.put_float(LAST_PROGRESS, self.last_progress.load());
        nbt.put_bool(EXTENDING, self.extending);
        nbt.put_bool(SOURCE, self.source);
    }

    fn chunk_data_nbt(&self) -> Option<NbtCompound> {
        let mut nbt = NbtCompound::new();
        self.write_nbt(&mut nbt);
        Some(nbt)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(state: &Block, extending: bool, source: bool, progress: f32) -> PistonBlockEntity {
        PistonBlockEntity {
            position: BlockPos::new(10, 64, 10),
            pushed_block_state: BlockState::from_id(state.default_state.id),
            facing: BlockDirection::East,
            current_progress: progress.into(),
            last_progress: progress.into(),
            last_ticked: AtomicI64::new(0),
            extending,
            source,
        }
    }

    #[test]
    fn moving_block_collision_follows_progress() {
        // PistonMovingBlockEntity.getCollisionShape: extending block starts one block behind
        let stone = entity(&Block::STONE, true, false, 0.0);
        let boxes = stone.collision_boxes();
        assert_eq!(boxes.len(), 1);
        assert!((boxes[0].min.x - 9.0).abs() < 1.0e-9);
        let stone = entity(&Block::STONE, true, false, 0.5);
        assert!((stone.collision_boxes()[0].min.x - 9.5).abs() < 1.0e-9);
        // retracting moves the other way
        let stone = entity(&Block::STONE, false, false, 0.5);
        assert!((stone.collision_boxes()[0].min.x - 10.5).abs() < 1.0e-9);
    }

    #[test]
    fn moving_block_does_not_block_the_entity_it_pushes() {
        let stone = entity(&Block::STONE, true, false, 0.5);
        NOCLIP.set(Some(BlockDirection::East));
        let pushed = stone.collision_boxes();
        NOCLIP.set(Some(BlockDirection::West));
        let other = stone.collision_boxes();
        NOCLIP.set(None);
        assert!(pushed.is_empty());
        assert_eq!(other.len(), 1);
    }
}
