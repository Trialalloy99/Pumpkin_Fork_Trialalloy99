use std::path::PathBuf;
use std::sync::Arc;

use pumpkin_data::{Block, Mirror, Rotation};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use pumpkin_world::generation::structure::template::{
    Palette, PaletteEntry, StructureBlockInfo, StructureEntityInfo, StructureTemplate,
    get_template, global_cache, place_template_mirrored, processor::StructureProcessor,
};

use crate::block::entities::structure_block::StructureBlockBlockEntity;
use crate::entity::r#type::load_entity_from_nbt;
use crate::world::World;
use crate::world::block_placer::WorldBlockPlacer;
use pumpkin_nbt::tag::NbtTag;

// structure block: StructureBlockEntity.UpdateType
pub const MAX_SIZE: i32 = 48;
const SCAN_RANGE: i32 = 80;

fn parse_rotation(name: &str) -> Rotation {
    match name {
        "CLOCKWISE_90" => Rotation::Clockwise90,
        "CLOCKWISE_180" => Rotation::Rotate180,
        "COUNTERCLOCKWISE_90" => Rotation::CounterClockwise90,
        _ => Rotation::None,
    }
}

fn parse_mirror(name: &str) -> Mirror {
    match name {
        "LEFT_RIGHT" => Mirror::LeftRight,
        "FRONT_BACK" => Mirror::FrontBack,
        _ => Mirror::None,
    }
}

// Identifier.tryParse: lowercase namespace:path, bare names are minecraft
#[must_use]
pub fn parse_structure_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    let namespace_ok = !namespace.is_empty()
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'));
    let path_ok = !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
        });
    (namespace_ok && path_ok).then(|| format!("{namespace}:{path}"))
}

fn template_path(world: &World, key: &str) -> Option<PathBuf> {
    let (namespace, path) = key.split_once(':')?;
    Some(
        world
            .level
            .level_folder
            .root_folder
            .join("generated")
            .join(namespace)
            .join("structures")
            .join(format!("{path}.nbt")),
    )
}

// StructureTemplateManager.get: generated folder first, then data packs and built-ins
#[must_use]
pub fn load_template(world: &World, key: &str) -> Option<Arc<StructureTemplate>> {
    if let Some(path) = template_path(world, key)
        && let Ok(bytes) = std::fs::read(&path)
    {
        let loaded = StructureTemplate::from_nbt_bytes(&bytes).is_ok();
        if loaded {
            global_cache().register_template(key, bytes.into());
        }
    }
    get_template(key)
}

const fn sort_key(info: &StructureBlockInfo) -> (i32, i32, i32) {
    (info.pos.y, info.pos.x, info.pos.z)
}

// StructureTemplate.fillFromWorld
fn fill_from_world(
    world: &Arc<World>,
    origin: BlockPos,
    size: Vector3<i32>,
    include_entities: bool,
) -> StructureTemplate {
    let mut full = Vec::new();
    let mut with_entity = Vec::new();
    let mut other = Vec::new();
    for y in 0..size.y {
        for x in 0..size.x {
            for z in 0..size.z {
                let pos = BlockPos::new(origin.0.x + x, origin.0.y + y, origin.0.z + z);
                let (block, state) = world.get_block_and_state(&pos);
                if block == &Block::STRUCTURE_VOID {
                    continue;
                }
                let nbt = world.get_block_entity_nbt(&pos).map(|mut nbt| {
                    for key in ["x", "y", "z"] {
                        nbt.child_tags.remove(key);
                    }
                    nbt
                });
                let properties = block
                    .properties(state.id)
                    .map(|properties| properties.to_props())
                    .unwrap_or_default();
                let entry =
                    PaletteEntry::with_properties(format!("minecraft:{}", block.name), properties);
                let info = StructureBlockInfo::new(Vector3::new(x, y, z), entry, nbt);
                if info.nbt.is_some() {
                    with_entity.push(info);
                } else if state.is_solid_render() {
                    full.push(info);
                } else {
                    other.push(info);
                }
            }
        }
    }
    for list in [&mut full, &mut with_entity, &mut other] {
        list.sort_by_key(sort_key);
    }
    let blocks: Vec<StructureBlockInfo> =
        full.into_iter().chain(with_entity).chain(other).collect();

    let mut template = StructureTemplate::default();
    template.size = size;
    template.palettes = vec![Palette::new(blocks)];
    if include_entities {
        let area = BoundingBox::new(
            origin.0.to_f64(),
            Vector3::new(
                f64::from(origin.0.x + size.x),
                f64::from(origin.0.y + size.y),
                f64::from(origin.0.z + size.z),
            ),
        );
        for entity in world.get_entities_at_box(&area) {
            if entity.get_player().is_some() {
                continue;
            }
            let mut nbt = NbtCompound::new();
            entity.write_nbt(&mut nbt);
            nbt.child_tags.remove("UUID");
            let position = entity.get_entity().pos.load();
            let relative = Vector3::new(
                position.x - f64::from(origin.0.x),
                position.y - f64::from(origin.0.y),
                position.z - f64::from(origin.0.z),
            );
            let block_pos = Vector3::new(
                relative.x.floor() as i32,
                relative.y.floor() as i32,
                relative.z.floor() as i32,
            );
            template
                .entity_info_list
                .push(StructureEntityInfo::new(relative, block_pos, nbt));
        }
    }
    template
}

impl StructureBlockBlockEntity {
    #[must_use]
    pub fn structure_name(&self) -> String {
        self.name
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn structure_key(&self) -> Option<String> {
        parse_structure_name(&self.structure_name())
    }

    fn get_int(value: &std::sync::Mutex<i32>) -> i32 {
        *value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get_bool(value: &std::sync::Mutex<bool>) -> bool {
        *value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn mode_name(&self) -> String {
        self.mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn structure_offset(&self) -> Vector3<i32> {
        Vector3::new(
            Self::get_int(&self.pos_x),
            Self::get_int(&self.pos_y),
            Self::get_int(&self.pos_z),
        )
    }

    fn structure_size(&self) -> Vector3<i32> {
        Vector3::new(
            Self::get_int(&self.size_x),
            Self::get_int(&self.size_y),
            Self::get_int(&self.size_z),
        )
    }

    fn set_size_and_author(&self, template: &StructureTemplate) {
        *self
            .size_x
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = template.size.x;
        *self
            .size_y
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = template.size.y;
        *self
            .size_z
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = template.size.z;
        self.author
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(&template.author);
    }

    // StructureBlockEntity.saveStructure
    pub fn save_structure(&self, world: &Arc<World>, save_to_disk: bool) -> bool {
        let Some(key) = self.structure_key() else {
            return false;
        };
        let origin = self.position.offset(self.structure_offset());
        let include_entities = !Self::get_bool(&self.ignore_entities);
        let mut template = fill_from_world(world, origin, self.structure_size(), include_entities);
        template.author.clone_from(
            &self
                .author
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        template.name = Some(key.clone());
        let mut bytes = Vec::new();
        if pumpkin_nbt::nbt_compress::write_gzip_compound_tag(template.save(), &mut bytes).is_err()
        {
            return false;
        }
        global_cache().register_template(&key, bytes.clone().into());
        if !save_to_disk {
            return true;
        }
        let Some(path) = template_path(world, &key) else {
            return false;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return false;
        }
        std::fs::write(path, bytes).is_ok()
    }

    pub fn is_structure_loadable(&self, world: &World) -> bool {
        self.mode_name() == "LOAD"
            && self
                .structure_key()
                .is_some_and(|key| load_template(world, &key).is_some())
    }

    // StructureBlockEntity.placeStructureIfSameSize
    pub fn place_structure_if_same_size(&self, world: &Arc<World>) -> bool {
        if self.mode_name() != "LOAD" {
            return false;
        }
        let Some(template) = self
            .structure_key()
            .and_then(|key| load_template(world, &key))
        else {
            return false;
        };
        if template.size == self.structure_size() {
            self.place_structure_template(world, &template);
            true
        } else {
            self.set_size_and_author(&template);
            false
        }
    }

    pub fn place_structure(&self, world: &Arc<World>) {
        if let Some(template) = self
            .structure_key()
            .and_then(|key| load_template(world, &key))
        {
            self.place_structure_template(world, &template);
        }
    }

    // StructureBlockEntity.placeStructure
    fn place_structure_template(&self, world: &Arc<World>, template: &StructureTemplate) {
        self.set_size_and_author(template);
        let rotation = parse_rotation(
            &self
                .rotation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mirror = parse_mirror(
            &self
                .mirror
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let integrity = *self
            .integrity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let processors = if integrity < 1.0 {
            vec![StructureProcessor::BlockRot {
                integrity: integrity.clamp(0.0, 1.0),
                rottable_blocks: None,
            }]
        } else {
            Vec::new()
        };
        let origin = self.position.offset(self.structure_offset());
        // rotated templates extend to negative offsets (StructureTemplate.transform pivot 0,0)
        let corner = rotation.rotate_offset(template.size.x - 1, template.size.z - 1);
        let placement_origin = Vector3::new(
            origin.0.x + corner.0.min(0),
            origin.0.y,
            origin.0.z + corner.1.min(0),
        );
        let mut placer = WorldBlockPlacer::new(world);
        place_template_mirrored(
            &mut placer,
            template,
            placement_origin,
            (0, 0),
            rotation,
            mirror,
            false,
            false,
            &processors,
            None,
            false,
        );
        placer.finalize();
        world.queue_block_updates(&placer.changed_positions);
        world.flush_block_updates();

        if !Self::get_bool(&self.ignore_entities) {
            Self::place_entities(world, template, origin, rotation, mirror);
        }
    }

    // StructureTemplate.placeEntities: transform(Vec3, mirror, rotation, pivot 0)
    fn place_entities(
        world: &Arc<World>,
        template: &StructureTemplate,
        origin: BlockPos,
        rotation: Rotation,
        mirror: Mirror,
    ) {
        for info in &template.entity_info_list {
            let (mut x, y, mut z) = (info.pos.x, info.pos.y, info.pos.z);
            match mirror {
                Mirror::LeftRight => z = 1.0 - z,
                Mirror::FrontBack => x = 1.0 - x,
                Mirror::None => {}
            }
            let (x, z) = match rotation {
                Rotation::CounterClockwise90 => (z, 1.0 - x),
                Rotation::Clockwise90 => (1.0 - z, x),
                Rotation::Rotate180 => (1.0 - x, 1.0 - z),
                Rotation::None => (x, z),
            };
            let position = Vector3::new(
                f64::from(origin.0.x) + x,
                f64::from(origin.0.y) + y,
                f64::from(origin.0.z) + z,
            );
            let mut nbt = info.nbt.clone();
            nbt.child_tags.remove("UUID");
            nbt.put(
                "Pos",
                NbtTag::List(vec![
                    position.x.into(),
                    position.y.into(),
                    position.z.into(),
                ]),
            );
            let yaw_offset = match rotation {
                Rotation::Clockwise90 => 90.0,
                Rotation::Rotate180 => 180.0,
                Rotation::CounterClockwise90 => 270.0,
                Rotation::None => 0.0,
            };
            if let Some(rotation_list) = nbt.get_list("Rotation")
                && let Some(NbtTag::Float(yaw)) = rotation_list.first()
            {
                let pitch = rotation_list.get(1).cloned().unwrap_or(NbtTag::Float(0.0));
                let mut yaw = *yaw;
                match mirror {
                    Mirror::LeftRight => yaw = 180.0 - yaw,
                    Mirror::FrontBack => yaw = -yaw,
                    Mirror::None => {}
                }
                nbt.put(
                    "Rotation",
                    NbtTag::List(vec![NbtTag::Float(yaw + yaw_offset), pitch]),
                );
            }
            if let Some(entity) = load_entity_from_nbt(&nbt, position, world) {
                world.spawn_entity(entity);
            }
        }
    }

    pub const fn unload_structure(&self) {
        // cached copies are replaced the next time the file is read
    }

    // StructureBlockEntity.detectSize
    pub fn detect_size(&self, world: &Arc<World>) -> bool {
        if self.mode_name() != "SAVE" {
            return false;
        }
        let name = self.structure_name();
        let mut count = 0;
        let mut bounds: Option<(BlockPos, BlockPos)> = None;
        for x in (self.position.0.x - SCAN_RANGE)..=(self.position.0.x + SCAN_RANGE) {
            for z in (self.position.0.z - SCAN_RANGE)..=(self.position.0.z + SCAN_RANGE) {
                if !world
                    .level
                    .is_chunk_loaded(&BlockPos::new(x, 0, z).chunk_position())
                {
                    continue;
                }
                for y in world.get_bottom_y()..=world.get_top_y() {
                    let pos = BlockPos::new(x, y, z);
                    if world.get_block(&pos) != &Block::STRUCTURE_BLOCK {
                        continue;
                    }
                    let Some(other) = world.get_block_entity(&pos) else {
                        continue;
                    };
                    let Some(other) = other.as_any().downcast_ref::<Self>() else {
                        continue;
                    };
                    if other.mode_name() == "CORNER" && other.structure_name() == name {
                        count += 1;
                        bounds = Some(bounds.map_or((pos, pos), |(min, max)| {
                            (
                                BlockPos::new(min.0.x.min(x), min.0.y.min(y), min.0.z.min(z)),
                                BlockPos::new(max.0.x.max(x), max.0.y.max(y), max.0.z.max(z)),
                            )
                        }));
                    }
                }
            }
        }
        let Some((mut min, mut max)) = bounds else {
            return false;
        };
        if count == 1 {
            min = BlockPos::new(
                min.0.x.min(self.position.0.x),
                min.0.y.min(self.position.0.y),
                min.0.z.min(self.position.0.z),
            );
            max = BlockPos::new(
                max.0.x.max(self.position.0.x),
                max.0.y.max(self.position.0.y),
                max.0.z.max(self.position.0.z),
            );
        }
        let (dx, dy, dz) = (max.0.x - min.0.x, max.0.y - min.0.y, max.0.z - min.0.z);
        if dx > 1 && dy > 1 && dz > 1 {
            let set = |value: &std::sync::Mutex<i32>, to: i32| {
                *value
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = to;
            };
            set(&self.pos_x, min.0.x - self.position.0.x + 1);
            set(&self.pos_y, min.0.y - self.position.0.y + 1);
            set(&self.pos_z, min.0.z - self.position.0.z + 1);
            set(&self.size_x, dx - 1);
            set(&self.size_y, dy - 1);
            set(&self.size_z, dz - 1);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::parse_structure_name;

    #[test]
    fn structure_names_follow_identifier_rules() {
        assert_eq!(
            parse_structure_name("house").as_deref(),
            Some("minecraft:house")
        );
        assert_eq!(
            parse_structure_name("my:a/b_c-1.x").as_deref(),
            Some("my:a/b_c-1.x")
        );
        assert_eq!(parse_structure_name(""), None);
        assert_eq!(parse_structure_name("Upper"), None);
        assert_eq!(parse_structure_name("a b"), None);
    }
}
