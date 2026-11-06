use std::sync::{
    Arc,
    atomic::{AtomicI32, AtomicU8, Ordering},
};

use pumpkin_data::Block;
use pumpkin_data::damage::DamageType;
use pumpkin_data::dye_color::DyeColor;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::version::JavaMinecraftVersion;

use super::block_attached::{self, BlockAttached};
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::server::Server;
use crate::world::World;
use crate::world::explosion::Explosion;

// cushion: Cushion.showBreakingParticles count, Cushion anchor thickness
const BREAK_PARTICLES: u32 = 10;
const ANCHOR_THICKNESS: f64 = 0.015_625;
const ANCHOR_REACH: f64 = 0.125;
const LIGHTNING_DROP_INVULNERABLE_TICKS: i32 = 20;

pub struct CushionEntity {
    pub entity: Entity,
    color: AtomicU8,
    ticks_since_check: AtomicI32,
}

impl CushionEntity {
    pub const fn new(entity: Entity, color: u8) -> Self {
        Self {
            entity,
            color: AtomicU8::new(color),
            ticks_since_check: AtomicI32::new(0),
        }
    }

    pub fn color(&self) -> u8 {
        self.color.load(Ordering::Relaxed)
    }

    pub fn set_color(&self, color: u8) {
        self.color.store(color, Ordering::Relaxed);
        self.sync_color();
    }

    fn sync_color(&self) {
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::cushion::COLOR,
            VarInt(i32::from(self.color())),
        );
    }

    #[must_use]
    pub const fn item_for_color(color: u8) -> &'static Item {
        match color {
            0 => &Item::WHITE_CUSHION,
            1 => &Item::ORANGE_CUSHION,
            2 => &Item::MAGENTA_CUSHION,
            3 => &Item::LIGHT_BLUE_CUSHION,
            4 => &Item::YELLOW_CUSHION,
            5 => &Item::LIME_CUSHION,
            6 => &Item::PINK_CUSHION,
            7 => &Item::GRAY_CUSHION,
            8 => &Item::LIGHT_GRAY_CUSHION,
            9 => &Item::CYAN_CUSHION,
            10 => &Item::PURPLE_CUSHION,
            11 => &Item::BLUE_CUSHION,
            12 => &Item::BROWN_CUSHION,
            13 => &Item::GREEN_CUSHION,
            14 => &Item::RED_CUSHION,
            _ => &Item::BLACK_CUSHION,
        }
    }

    #[must_use]
    pub fn color_from_item(item: &Item) -> u8 {
        for (id, comp) in item.components {
            if *id == pumpkin_data::data_component::DataComponent::CushionColor {
                return pumpkin_data::data_component_impl::get::<
                    pumpkin_data::data_component_impl::CushionColorImpl,
                >(*comp)
                .color
                .id();
            }
        }
        0
    }

    #[must_use]
    pub fn color_from_stack(stack: &ItemStack) -> u8 {
        stack
            .get_data_component::<pumpkin_data::data_component_impl::CushionColorImpl>()
            .map_or_else(|| Self::color_from_item(stack.get_item()), |c| c.color.id())
    }

    // cushion: Cushion.getCushionItemStackWithData
    fn item_with_data(&self) -> ItemStack {
        let mut stack = ItemStack::new(1, Self::item_for_color(self.color()));
        if let Some(name) = (**self.entity.custom_name.load()).clone() {
            stack.set_data_component(pumpkin_data::data_component_impl::CustomNameImpl { name });
        }
        stack
    }

    // cushion: Cushion.showBreakingParticles, wool of the colour
    fn show_breaking_particles(&self) {
        let color = DyeColor::ALL
            .get(usize::from(self.color()))
            .copied()
            .unwrap_or_default();
        let Some(wool) = Block::from_name(&format!("minecraft:{}_wool", color.name())) else {
            return;
        };
        let mut data = Vec::with_capacity(5);
        let mut state = u32::from(wool.default_state.id.as_u16());
        loop {
            let byte = (state & 0x7F) as u8;
            state >>= 7;
            if state == 0 {
                data.push(byte);
                break;
            }
            data.push(byte | 0x80);
        }
        let pos = self.entity.pos.load();
        let width = self.entity.width();
        let height = self.entity.height();
        self.entity.world.load().spawn_particles_with_data(
            Particle::Block,
            Vector3::new(
                pos.x,
                pos.y + f64::from(height) * 0.666_666_666_666_666_6,
                pos.z,
            ),
            BREAK_PARTICLES,
            Vector3::new(width / 4.0, height / 4.0, width / 4.0),
            0.05,
            &data,
        );
    }

    // cushion: Cushion.destroyIfInFire
    pub fn destroy_if_in_fire(&self) {
        if self.entity.is_removed() {
            return;
        }
        let world = self.entity.world.load();
        let bounds = self
            .entity
            .bounding_box
            .load()
            .expand(-1.0e-7, -1.0e-7, -1.0e-7);
        let in_fire = BlockPos::iterate(bounds.min_block_pos(), bounds.max_block_pos())
            .any(|pos| world.get_block(&pos).has_tag(&tag::Block::MINECRAFT_FIRE));
        if in_fire {
            self.damage_with_context(self, 1.0, DamageType::IN_FIRE, None, None, None);
        }
    }

    // cushion: Cushion.canBePlacedAt
    #[must_use]
    pub fn can_be_placed_at(world: &World, bounds: &BoundingBox) -> bool {
        Self::would_survive_at(world, bounds) && !Self::is_anchor_buried(world, bounds)
    }

    // cushion: Cushion.wouldSurviveAt
    #[must_use]
    pub fn would_survive_at(world: &World, bounds: &BoundingBox) -> bool {
        Self::has_anchor_below(world, bounds)
            && !Self::is_covered_by_suffocating_blocks(world, bounds)
    }

    fn has_anchor_below(world: &World, bounds: &BoundingBox) -> bool {
        let anchor = BoundingBox::new(
            Vector3::new(bounds.min.x, bounds.min.y - ANCHOR_THICKNESS, bounds.min.z),
            Vector3::new(
                bounds.max.x - f64::EPSILON,
                bounds.min.y,
                bounds.max.z - f64::EPSILON,
            ),
        );
        let search = BoundingBox::new(
            Vector3::new(anchor.min.x, anchor.min.y - ANCHOR_REACH, anchor.min.z),
            anchor.max,
        );
        BlockPos::iterate(search.min_block_pos(), search.max_block_pos()).any(|pos| {
            let state = world.get_block_state(&pos);
            state
                .get_block_outline_shapes()
                .any(|shape| shape.at_pos(pos).intersects(&anchor))
        })
    }

    // cushion: Cushion.isAnchorBuried, the resting slice is fully inside collisions
    fn is_anchor_buried(world: &World, bounds: &BoundingBox) -> bool {
        let slice = BoundingBox::new(
            bounds.min,
            Vector3::new(bounds.max.x, bounds.min.y + ANCHOR_THICKNESS, bounds.max.z),
        )
        .expand(-1.0e-7, -1.0e-7, -1.0e-7);
        let mut uncovered = vec![slice];
        for pos in BlockPos::iterate(slice.min_block_pos(), slice.max_block_pos()) {
            let state = world.get_block_state(&pos);
            for shape in state.get_block_collision_shapes_at(&pos) {
                let shape = shape.at_pos(pos);
                uncovered = uncovered
                    .iter()
                    .flat_map(|part| part.subtract(&shape))
                    .collect();
                if uncovered.is_empty() {
                    return true;
                }
            }
        }
        false
    }

    // cushion: Cushion.isCoveredBySuffocatingBlocks
    fn is_covered_by_suffocating_blocks(world: &World, bounds: &BoundingBox) -> bool {
        let inner = bounds.expand(-1.0e-7, -1.0e-7, -1.0e-7);
        BlockPos::iterate(inner.min_block_pos(), inner.max_block_pos())
            .all(|pos| world.get_block_state(&pos).is_suffocating())
    }
}

impl BlockAttached for CushionEntity {
    fn ticks_since_check(&self) -> &AtomicI32 {
        &self.ticks_since_check
    }

    // cushion: Cushion.survives
    fn survives(&self) -> bool {
        Self::would_survive_at(&self.entity.world.load(), &self.entity.bounding_box.load())
    }

    // cushion: Cushion.dropItem
    fn drop_item(&self, caused_by: Option<&dyn EntityBase>) {
        let world = self.entity.world.load();
        self.entity.play_sound(Sound::EntityCushionBreak);
        self.show_breaking_particles();
        if !world.level_info.load().game_rules.entity_drops
            || block_attached::infinite_materials(caused_by)
        {
            return;
        }
        if let Some(item) =
            world.drop_stack_entity(&self.entity.block_pos.load(), self.item_with_data())
            && caused_by.is_some_and(|cause| {
                cause.get_entity().entity_type == &pumpkin_data::entity::EntityType::LIGHTNING_BOLT
            })
        {
            item.get_entity()
                .invulnerable_time
                .store(LIGHTNING_DROP_INVULNERABLE_TICKS, Ordering::Relaxed);
        }
    }

    // cushion: Cushion.tickAtCheckInterval, fluid damage and fire
    fn tick_at_check_interval(&self) {
        let world = self.entity.world.load();
        let pos = self.entity.block_pos.load();
        if world.get_fluid(&pos).has_tag(&tag::Fluid::MINECRAFT_LAVA) {
            self.damage_with_context(self, 4.0, DamageType::LAVA, None, None, None);
        }
        self.destroy_if_in_fire();
    }
}

impl EntityBase for CushionEntity {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn init_data_tracker(&self) {
        self.sync_color();
    }

    /// The colour has to ride along with the spawn packet; a client seeing the
    /// cushion for the first time never got the tracked data update.
    fn java_spawn_metadata(&self, version: JavaMinecraftVersion) -> Option<Box<[u8]>> {
        let mut metadata = Vec::new();
        Metadata::new(
            pumpkin_data::tracked_data::cushion::COLOR,
            VarInt(i32::from(self.color())),
        )
        .write(&mut metadata, &version)
        .ok()?;
        metadata.push(255);
        Some(metadata.into_boxed_slice())
    }

    fn tick(&self, _caller: &dyn EntityBase, _server: &Server) {
        self.block_attached_tick();
    }

    fn can_hit(&self) -> bool {
        self.entity.is_alive()
    }

    fn is_pickable(&self) -> bool {
        true
    }

    // cushion: BlockAttachedEntity.ignoreExplosion
    fn ignore_explosion(&self, explosion: &Explosion) -> bool {
        block_attached::ignores_explosion(&self.entity, explosion)
    }

    // cushion: thunderHit kills it and drops the item
    fn on_lightning_strike(
        &self,
        _caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        if !self.entity.is_removed() {
            self.kill_attached(Some(lightning));
            self.drop_item(Some(lightning));
        }
    }

    // cushion: Cushion.addAdditionalSaveData, the colour is a dye name
    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let pos = self.entity.block_pos.load();
        nbt.put(
            "block_pos",
            pumpkin_nbt::tag::NbtTag::IntArray(vec![pos.0.x, pos.0.y, pos.0.z]),
        );
        let color = DyeColor::ALL
            .get(usize::from(self.color()))
            .copied()
            .unwrap_or_default();
        nbt.put_string("color", color.name().to_string());
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        let color = nbt
            .get_string("color")
            .and_then(|name| DyeColor::ALL.iter().find(|color| color.name() == name))
            .map(DyeColor::id)
            .or_else(|| nbt.get_byte("Color").map(|byte| byte as u8))
            .unwrap_or(0);
        self.set_color(color);
    }

    // cushion: Cushion.interact, sit down
    fn interact(&self, player: &Arc<Player>, _item_stack: &mut ItemStack) -> bool {
        if player.get_entity().is_sneaking()
            || !self
                .entity
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        {
            return false;
        }
        let world = self.entity.world.load();
        let (Some(vehicle), Some(passenger)) = (
            world.get_entity_by_id(self.entity.entity_id),
            world.get_player_by_id(player.entity_id()),
        ) else {
            return false;
        };
        self.entity
            .add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
        if player.get_entity().has_vehicle() {
            self.entity.play_sound(Sound::EntityCushionSit);
        }
        true
    }

    // cushion: Cushion.hurtServer, the player must be allowed to build there
    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        _amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        if let Some(player) = cause.or(source).and_then(EntityBase::get_player) {
            let may_build = player
                .abilities
                .try_lock()
                .is_ok_and(|abilities| abilities.allow_modify_world);
            if !may_build
                || self
                    .entity
                    .world
                    .load()
                    .is_in_spawn_protection(player, &self.entity.block_pos.load())
            {
                return false;
            }
        }
        self.block_attached_hurt(&damage_type, source, cause)
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
