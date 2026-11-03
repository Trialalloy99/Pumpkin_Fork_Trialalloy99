#[allow(clippy::wildcard_imports)]
use super::*;
use crate::entity::vehicle::minecart::MinecartEntity;
use pumpkin_protocol::java::server::play::SSetCommandMinecart;

impl JavaClient {
    // ServerGamePacketListenerImpl.handleSetCommandMinecart
    pub fn handle_set_command_minecart(&self, player: &Player, packet: &SSetCommandMinecart<'_>) {
        if !player.is_creative() || player.permission_lvl.load() < PermissionLvl::Two {
            return;
        }

        let world = player.world();
        let Some(entity) = world.get_entity_by_id(packet.entity_id.0) else {
            return;
        };
        let Some(minecart) = entity.cast_any().downcast_ref::<MinecartEntity>() else {
            return;
        };
        if minecart.set_command_block_command(packet.command, packet.track_output) {
            player.send_system_message(&TextComponent::text(format!(
                "Command set: {}",
                packet.command
            )));
        }
    }
}
