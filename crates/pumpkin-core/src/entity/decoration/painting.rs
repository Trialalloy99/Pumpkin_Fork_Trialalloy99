use core::f32;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use super::block_attached::{self, BlockAttached};
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::server::Server;
use pumpkin_data::BlockDirection;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::painting_variant::PaintingVariant;
use pumpkin_data::sound::Sound;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::version::JavaMinecraftVersion;
use std::sync::Arc;

/// The world stores a painting facing as a horizontal value: 0 south, 1 west,
/// 2 north, 3 east (`Direction.get2DDataValue`). The entity data, and the spawn
/// packet built from it, use the 3D index instead: north 2, south 3, west 4,
/// east 5. Sending the raw horizontal value makes the client read 0 as down,
/// fail the horizontal check in `HangingEntity.setDirection` and disconnect.
const fn facing_from_horizontal(value: u8) -> BlockDirection {
    match value & 3 {
        1 => BlockDirection::West,
        2 => BlockDirection::North,
        3 => BlockDirection::East,
        _ => BlockDirection::South,
    }
}

/// Vanilla reads the field with `getByte`, which yields 0 (south) when it is
/// absent, so a partial entity file keeps a horizontal default.
fn facing_from_nbt(nbt: &NbtCompound) -> BlockDirection {
    facing_from_horizontal(nbt.get_byte("facing").unwrap_or(0) as u8)
}

const fn facing_to_horizontal(direction: BlockDirection) -> u8 {
    match direction {
        BlockDirection::West => 1,
        BlockDirection::North => 2,
        BlockDirection::East => 3,
        _ => 0,
    }
}

// painting: Painting.DEPTH
const DEPTH: f64 = 0.0625;
// hanging entity: spawnAtLocation offset from the wall
const DROP_OFFSET: f64 = 0.15;

pub struct PaintingEntity {
    pub entity: Entity,
    variant_id: AtomicU32,
    ticks_since_check: AtomicI32,
}

impl PaintingEntity {
    #[must_use]
    pub const fn new(entity: Entity) -> Self {
        Self::new_with_variant(entity, PaintingVariant::Alban)
    }

    #[must_use]
    pub const fn new_with_variant(entity: Entity, variant: PaintingVariant) -> Self {
        Self {
            entity,
            variant_id: AtomicU32::new(variant.id()),
            ticks_since_check: AtomicI32::new(0),
        }
    }

    #[must_use]
    pub fn direction(&self) -> BlockDirection {
        BlockDirection::from_index(self.entity.data.load(Ordering::Relaxed) as u8)
            .filter(pumpkin_data::BlockDirection::is_horizontal)
            .unwrap_or(BlockDirection::South)
    }

    // painting: Painting.calculateBoundingBox, 1/16 deep box around the centre
    #[must_use]
    pub fn hitbox(
        center: Vector3<f64>,
        direction: BlockDirection,
        variant: PaintingVariant,
    ) -> BoundingBox {
        let x_size = if matches!(direction, BlockDirection::East | BlockDirection::West) {
            DEPTH
        } else {
            f64::from(variant.width())
        };
        let z_size = if matches!(direction, BlockDirection::North | BlockDirection::South) {
            DEPTH
        } else {
            f64::from(variant.width())
        };
        let y_size = f64::from(variant.height());
        BoundingBox::new(
            Vector3::new(
                center.x - x_size / 2.0,
                center.y - y_size / 2.0,
                center.z - z_size / 2.0,
            ),
            Vector3::new(
                center.x + x_size / 2.0,
                center.y + y_size / 2.0,
                center.z + z_size / 2.0,
            ),
        )
    }

    // painting: Painting.recalculateBoundingBox
    pub fn refresh_bounding_box(&self) {
        self.entity.bounding_box.store(Self::hitbox(
            self.entity.pos.load(),
            self.direction(),
            self.variant(),
        ));
    }

    // hanging entity: HangingEntity.survives, free of collisions, backed by solid blocks, no overlap
    #[must_use]
    pub fn fits_at(
        world: &crate::world::World,
        center: Vector3<f64>,
        direction: BlockDirection,
        variant: PaintingVariant,
        own_id: Option<i32>,
    ) -> bool {
        let bounds = Self::hitbox(center, direction, variant);
        if !world.is_space_empty(bounds) {
            return false;
        }
        let offset = direction.to_offset().to_f64();
        let support = BoundingBox::new(
            Vector3::new(
                bounds.min.x - offset.x * 0.5 + 1.0e-7,
                bounds.min.y + 1.0e-7,
                bounds.min.z - offset.z * 0.5 + 1.0e-7,
            ),
            Vector3::new(
                bounds.max.x - offset.x * 0.5 - 1.0e-7,
                bounds.max.y - 1.0e-7,
                bounds.max.z - offset.z * 0.5 - 1.0e-7,
            ),
        );
        let supported =
            BlockPos::iterate(support.min_block_pos(), support.max_block_pos()).all(|pos| {
                let (block, state) = world.get_block_and_state(&pos);
                state.is_solid()
                    || block == &pumpkin_data::Block::REPEATER
                    || block == &pumpkin_data::Block::COMPARATOR
            });
        if !supported {
            return false;
        }
        // hanging entity: canCoexist(false), same type or same direction blocks it
        let own_type = &EntityType::PAINTING;
        !world.get_entities_at_box(&bounds).iter().any(|other| {
            let other_entity = other.get_entity();
            if Some(other_entity.entity_id) == own_id {
                return false;
            }
            let other_direction = if let Some(painting) = other.cast_any().downcast_ref::<Self>() {
                painting.direction()
            } else if let Some(frame) = other
                .cast_any()
                .downcast_ref::<super::item_frame::ItemFrameEntity>()
            {
                frame.get_facing()
            } else {
                return false;
            };
            other_entity.entity_type == own_type || other_direction == direction
        })
    }

    #[must_use]
    pub fn variant(&self) -> PaintingVariant {
        let id = self.variant_id.load(Ordering::Relaxed);
        PaintingVariant::all()
            .get(id as usize)
            .copied()
            .unwrap_or(PaintingVariant::Alban)
    }

    pub fn set_variant(&self, variant: PaintingVariant) {
        self.variant_id.store(variant.id(), Ordering::Relaxed);
        self.sync_variant();
        self.refresh_bounding_box();
    }

    pub fn sync_variant(&self) {
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::painting::DATA_PAINTING_VARIANT_ID,
            VarInt(self.variant_id.load(Ordering::Relaxed) as i32),
        );
    }

    /// Calculates the exact floating-point center position for a painting of given width and height (in blocks),
    /// placed on `face` of `location`.
    #[must_use]
    pub fn calculate_center_pos(
        location: BlockPos,
        face: BlockDirection,
        width: u32,
        height: u32,
    ) -> Vector3<f64> {
        let target_pos = location.offset(face.to_offset());
        let face_ccw = face.rotate_counter_clockwise();
        let step_x = f64::from(face.to_offset().x);
        let step_z = f64::from(face.to_offset().z);
        let ccw_step_x = f64::from(face_ccw.to_offset().x);
        let ccw_step_z = f64::from(face_ccw.to_offset().z);

        let mut x = f64::from(target_pos.0.x) + 0.5 - step_x * 0.46875;
        let mut y = f64::from(target_pos.0.y) + 0.5;
        let mut z = f64::from(target_pos.0.z) + 0.5 - step_z * 0.46875;

        let width_offset = if width.is_multiple_of(2) { 0.5 } else { 0.0 };
        let height_offset = if height.is_multiple_of(2) { 0.5 } else { 0.0 };

        x += width_offset * ccw_step_x;
        z += width_offset * ccw_step_z;
        y += height_offset;

        Vector3::new(x, y, z)
    }

    /// Checks if a painting of the specified variant fits on the wall at `location` facing `face`.
    #[must_use]
    pub fn painting_fits(
        world: &crate::world::World,
        location: BlockPos,
        face: BlockDirection,
        variant: PaintingVariant,
    ) -> bool {
        let center = Self::calculate_center_pos(location, face, variant.width(), variant.height());
        Self::fits_at(world, center, face, variant, None)
    }

    /// Vanilla `Painting.create`: variants that survive, the largest area, a random one of those.
    #[must_use]
    pub fn choose_variant(
        world: &crate::world::World,
        location: BlockPos,
        face: BlockDirection,
    ) -> Option<PaintingVariant> {
        use rand::seq::IndexedRandom;

        let mut fitting_variants = Vec::new();
        let mut max_area = 0;

        for &variant in PaintingVariant::all_placeable() {
            if Self::painting_fits(world, location, face, variant) {
                let area = variant.width() * variant.height();
                if area > max_area {
                    max_area = area;
                    fitting_variants.clear();
                    fitting_variants.push(variant);
                } else if area == max_area {
                    fitting_variants.push(variant);
                }
            }
        }

        let mut rng = rand::rng();
        fitting_variants.choose(&mut rng).copied()
    }
}

impl BlockAttached for PaintingEntity {
    fn ticks_since_check(&self) -> &AtomicI32 {
        &self.ticks_since_check
    }

    // painting: HangingEntity.survives
    fn survives(&self) -> bool {
        Self::fits_at(
            &self.entity.world.load(),
            self.entity.pos.load(),
            self.direction(),
            self.variant(),
            Some(self.entity.entity_id),
        )
    }

    // painting: Painting.dropItem
    fn drop_item(&self, caused_by: Option<&dyn EntityBase>) {
        let world = self.entity.world.load();
        if !world.level_info.load().game_rules.entity_drops {
            return;
        }
        self.entity.play_sound(Sound::EntityPaintingBreak);
        if block_attached::infinite_materials(caused_by) {
            return;
        }
        let mut stack = ItemStack::new(1, &Item::PAINTING);
        if let Some(name) = (**self.entity.custom_name.load()).clone() {
            stack.set_data_component(pumpkin_data::data_component_impl::CustomNameImpl { name });
        }
        let direction = self.direction().to_offset().to_f64();
        let pos = self.entity.pos.load();
        let item = crate::entity::item::ItemEntity::new_with_velocity(
            Entity::new(
                world.clone(),
                Vector3::new(
                    pos.x + direction.x * DROP_OFFSET,
                    pos.y,
                    pos.z + direction.z * DROP_OFFSET,
                ),
                &EntityType::ITEM,
            ),
            stack,
            Vector3::new(0.0, 0.0, 0.0),
            crate::entity::item::ItemEntity::DEFAULT_PICKUP_DELAY,
        );
        world.spawn_entity(Arc::new(item));
    }
}

impl EntityBase for PaintingEntity {
    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let index = self.entity.data.load(Ordering::Relaxed) as u8;
        let direction = BlockDirection::from_index(index).unwrap_or(BlockDirection::South);
        nbt.put_byte("facing", facing_to_horizontal(direction) as i8);
        let variant_id = self.variant_id.load(Ordering::Relaxed);
        let variant_str =
            self.entity
                .resolve_dynamic_entry_id("minecraft:painting_variant", variant_id, |_id| {
                    self.variant().asset_id().to_string()
                });
        nbt.put_string("variant", variant_str);
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        let facing = facing_from_nbt(nbt);
        self.entity
            .data
            .store(i32::from(facing.to_index()), Ordering::Relaxed);
        if let Some(variant_str) = nbt
            .get_string("variant")
            .or_else(|| nbt.get_string("Motive"))
            && let Some(raw_id) =
                self.entity
                    .resolve_dynamic_raw_id("minecraft:painting_variant", variant_str, |n| {
                        PaintingVariant::from_name(n).map(|v| v.id())
                    })
        {
            self.variant_id.store(raw_id, Ordering::Relaxed);
            self.sync_variant();
        }
        self.refresh_bounding_box();
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn init_data_tracker(&self) {
        self.sync_variant();
        self.refresh_bounding_box();
    }

    fn java_spawn_metadata(&self, version: JavaMinecraftVersion) -> Option<Box<[u8]>> {
        let mut metadata = Vec::new();
        Metadata::new(
            pumpkin_data::tracked_data::painting::DATA_PAINTING_VARIANT_ID,
            VarInt(self.variant_id.load(Ordering::Relaxed) as i32),
        )
        .write(&mut metadata, &version)
        .ok()?;
        metadata.push(255);
        Some(metadata.into_boxed_slice())
    }

    fn set_variant_name(&self, name: &str) {
        if let Some(variant) = PaintingVariant::from_name(name) {
            self.set_variant(variant);
        } else if let Some(server) = self.entity.world.load().server.upgrade()
            && let Some(raw_id) = server
                .datapack_manager
                .get_dynamic_registry_raw_id("minecraft:painting_variant", name)
        {
            self.variant_id.store(raw_id, Ordering::Relaxed);
            self.sync_variant();
        }
    }

    fn tick(&self, _caller: &dyn EntityBase, _server: &Server) {
        self.block_attached_tick();
    }

    // painting: BlockAttachedEntity.hurtServer, the player must be allowed to interact there
    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        _amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        if let Some(player) = cause.or(source).and_then(EntityBase::get_player)
            && self
                .entity
                .world
                .load()
                .is_in_spawn_protection(player, &self.entity.block_pos.load())
        {
            return false;
        }
        self.block_attached_hurt(&damage_type, source, cause)
    }

    fn can_hit(&self) -> bool {
        self.entity.is_alive()
    }

    fn is_pickable(&self) -> bool {
        true
    }

    fn ignore_explosion(&self, explosion: &crate::world::explosion::Explosion) -> bool {
        block_attached::ignores_explosion(&self.entity, explosion)
    }

    fn on_lightning_strike(
        &self,
        _caller: &dyn EntityBase,
        _lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_and_3d_facings_round_trip() {
        for horizontal in 0u8..4 {
            let direction = facing_from_horizontal(horizontal);
            assert!(direction.is_horizontal(), "{direction:?} is vertical");
            assert_eq!(facing_to_horizontal(direction), horizontal);
        }
    }

    #[test]
    fn south_two_is_not_down_zero() {
        // The old bug: south (0 in the world file) was sent as index 0, which the
        // client reads as down and rejects.
        assert_eq!(facing_from_horizontal(0), BlockDirection::South);
        assert_eq!(facing_from_horizontal(0).to_index(), 3);
    }

    #[test]
    fn missing_facing_defaults_to_south() {
        let nbt = NbtCompound::new();
        assert_eq!(facing_from_nbt(&nbt), BlockDirection::South);
        assert_eq!(facing_from_nbt(&nbt).to_index(), 3);

        let mut with_facing = NbtCompound::new();
        with_facing.put_byte("facing", 1);
        assert_eq!(facing_from_nbt(&with_facing), BlockDirection::West);
    }

    #[test]
    fn out_of_range_values_wrap_into_horizontal_directions() {
        assert!(facing_from_horizontal(4).is_horizontal());
        assert!(facing_from_horizontal(255).is_horizontal());
    }

    #[test]
    fn calculate_center_pos_works() {
        let location = BlockPos(Vector3::new(10, 64, 20));

        // 1x1 painting facing North: placed against block at (10, 64, 20)
        let pos_1x1 = PaintingEntity::calculate_center_pos(location, BlockDirection::North, 1, 1);
        assert_eq!(pos_1x1.x, 10.5);
        assert_eq!(pos_1x1.y, 64.5);
        assert!((pos_1x1.z - 19.96875).abs() < 1e-6);

        // 2x1 painting facing North: width is 2, shifts along counter-clockwise (West = -X)
        let pos_2x1 = PaintingEntity::calculate_center_pos(location, BlockDirection::North, 2, 1);
        assert_eq!(pos_2x1.x, 10.0);
        assert_eq!(pos_2x1.y, 64.5);
        assert!((pos_2x1.z - 19.96875).abs() < 1e-6);

        // 2x2 painting facing North: height is 2, shifts +0.5 along Y
        let pos_2x2 = PaintingEntity::calculate_center_pos(location, BlockDirection::North, 2, 2);
        assert_eq!(pos_2x2.x, 10.0);
        assert_eq!(pos_2x2.y, 65.0);
        assert!((pos_2x2.z - 19.96875).abs() < 1e-6);

        // 4x4 painting facing North
        let pos_4x4 = PaintingEntity::calculate_center_pos(location, BlockDirection::North, 4, 4);
        assert_eq!(pos_4x4.x, 10.0);
        assert_eq!(pos_4x4.y, 65.0);
        assert!((pos_4x4.z - 19.96875).abs() < 1e-6);

        // 1x1 painting facing South: placed on south face of (10, 64, 20) -> target is (10, 64, 21)
        let pos_south = PaintingEntity::calculate_center_pos(location, BlockDirection::South, 1, 1);
        assert_eq!(pos_south.x, 10.5);
        assert_eq!(pos_south.y, 64.5);
        assert!((pos_south.z - 21.03125).abs() < 1e-6);
    }

    #[test]
    fn variant_resolution() {
        assert_eq!(
            PaintingVariant::from_name("minecraft:kebab"),
            Some(PaintingVariant::Kebab)
        );
        assert_eq!(
            PaintingVariant::from_name("kebab"),
            Some(PaintingVariant::Kebab)
        );
        assert_eq!(PaintingVariant::from_name("invalid"), None);
        assert_eq!(PaintingVariant::Kebab.asset_id(), "minecraft:kebab");
    }

    #[tokio::test]
    async fn painting_needs_a_solid_wall_behind_it() -> Result<(), Box<dyn std::error::Error>> {
        use crate::entity::ai::test_support::world;
        use pumpkin_world::world::BlockFlags;

        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        world.level.loaded_chunks.insert(
            pumpkin_util::math::vector2::Vector2::new(0, 0),
            pumpkin_world::chunk::ChunkData::empty_sync(0, 0),
        );
        let wall = BlockPos(Vector3::new(4, 64, 4));
        let front =
            |variant| PaintingEntity::painting_fits(&world, wall, BlockDirection::South, variant);
        assert!(!front(PaintingVariant::Kebab));
        world.set_block_state(
            &wall,
            pumpkin_data::Block::STONE.default_state.id,
            BlockFlags::NOTIFY_ALL,
        );
        assert!(front(PaintingVariant::Kebab));
        // a painting wider than the one wall block has nothing behind its other half
        assert!(!front(PaintingVariant::Pool));
        world.level.shutdown().await;
        Ok(())
    }
}
