#[allow(clippy::wildcard_imports)]
use super::*;
use crate::block::entities::structure_block::StructureBlockBlockEntity;
use crate::block::structure_ops::MAX_SIZE;
use pumpkin_data::block_properties::{StructureBlockLikeProperties, StructureblockMode};
use pumpkin_protocol::java::server::play::SSetStructureBlock;

fn set<T>(value: &std::sync::Mutex<T>, to: T) {
    *value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = to;
}

impl JavaClient {
    // ServerGamePacketListenerImpl.handleSetStructureBlock
    pub fn handle_set_structure_block(&self, player: &Player, packet: &SSetStructureBlock<'_>) {
        if !player.can_use_game_master_blocks() {
            return;
        }
        let world = player.world();
        let Some(block_entity) = world.get_block_entity(&packet.location) else {
            return;
        };
        let Some(structure) = block_entity
            .as_any()
            .downcast_ref::<StructureBlockBlockEntity>()
        else {
            return;
        };

        let (mode_name, mode) = match packet.mode.0 {
            SSetStructureBlock::MODE_SAVE => ("SAVE", StructureblockMode::Save),
            SSetStructureBlock::MODE_LOAD => ("LOAD", StructureblockMode::Load),
            SSetStructureBlock::MODE_CORNER => ("CORNER", StructureblockMode::Corner),
            _ => ("DATA", StructureblockMode::Data),
        };
        set(&structure.mode, mode_name.to_string());
        let state = world.get_block_state(&packet.location);
        let mut props = StructureBlockLikeProperties::from_state_id(state.id);
        props.mode = mode;
        world.set_block_state(
            &packet.location,
            props.to_state_id(&Block::STRUCTURE_BLOCK),
            BlockFlags::NOTIFY_LISTENERS,
        );

        set(&structure.name, packet.name.to_string());
        let offset = |value: i8| i32::from(value).clamp(-MAX_SIZE, MAX_SIZE);
        let size = |value: u8| i32::from(value).clamp(0, MAX_SIZE);
        set(&structure.pos_x, offset(packet.offset_x));
        set(&structure.pos_y, offset(packet.offset_y));
        set(&structure.pos_z, offset(packet.offset_z));
        set(&structure.size_x, size(packet.size_x));
        set(&structure.size_y, size(packet.size_y));
        set(&structure.size_z, size(packet.size_z));
        set(
            &structure.mirror,
            match packet.mirror.0 {
                1 => "LEFT_RIGHT",
                2 => "FRONT_BACK",
                _ => "NONE",
            }
            .to_string(),
        );
        set(
            &structure.rotation,
            match packet.rotation.0 {
                1 => "CLOCKWISE_90",
                2 => "CLOCKWISE_180",
                3 => "COUNTERCLOCKWISE_90",
                _ => "NONE",
            }
            .to_string(),
        );
        set(&structure.metadata, packet.metadata.to_string());
        set(&structure.ignore_entities, packet.ignore_entities());
        set(&structure.show_air, packet.show_air());
        set(&structure.show_bounding_box, packet.show_bounding_box());
        set(&structure.integrity, packet.integrity.clamp(0.0, 1.0));
        set(&structure.seed, packet.seed.0);

        let name = packet.name.to_string();
        let message = |key: &'static str| {
            player.send_system_message_raw(
                &TextComponent::translate(key, [TextComponent::text(name.clone())]),
                false,
            );
        };
        if structure.structure_key().is_some() {
            match packet.action.0 {
                SSetStructureBlock::ACTION_SAVE_AREA => {
                    if mode_name == "SAVE" && structure.save_structure(&world, true) {
                        message(translation::java::STRUCTURE_BLOCK_SAVE_SUCCESS);
                    } else {
                        message(translation::java::STRUCTURE_BLOCK_SAVE_FAILURE);
                    }
                }
                SSetStructureBlock::ACTION_LOAD_AREA => {
                    if !structure.is_structure_loadable(&world) {
                        message(translation::java::STRUCTURE_BLOCK_LOAD_NOT_FOUND);
                    } else if structure.place_structure_if_same_size(&world) {
                        message(translation::java::STRUCTURE_BLOCK_LOAD_SUCCESS);
                    } else {
                        message(translation::java::STRUCTURE_BLOCK_LOAD_PREPARE);
                    }
                }
                SSetStructureBlock::ACTION_SCAN_AREA => {
                    if structure.detect_size(&world) {
                        message(translation::java::STRUCTURE_BLOCK_SIZE_SUCCESS);
                    } else {
                        message(translation::java::STRUCTURE_BLOCK_SIZE_FAILURE);
                    }
                }
                _ => {}
            }
        } else {
            message(translation::java::STRUCTURE_BLOCK_INVALID_STRUCTURE_NAME);
        }
        world.update_block_entity(&block_entity);
    }
}
