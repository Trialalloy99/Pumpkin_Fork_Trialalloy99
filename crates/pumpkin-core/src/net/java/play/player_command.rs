#[allow(clippy::wildcard_imports)]
use super::*;

impl JavaClient {
    pub fn handle_player_command(
        &self,
        player: &Arc<Player>,
        command: &SPlayerCommand,
        server: &Arc<Server>,
    ) {
        if command.entity_id != player.entity_id().into() {
            return;
        }
        if !player.has_client_loaded() {
            return;
        }
        player.update_last_action_time();

        let entity = &player.get_entity();
        match command.action {
            Action::StartSprinting => {
                if !entity.is_sprinting() {
                    send_cancellable_blocking! {{
                        server;
                        PlayerToggleSprintEvent::new(player.clone(), true);
                        'after: {
                            player.set_sprinting(event.is_sprinting);
                            player.update_player_pose();
                        }
                    }}
                }
            }
            Action::StopSprinting => {
                if entity.is_sprinting() {
                    send_cancellable_blocking! {{
                        server;
                        PlayerToggleSprintEvent::new(player.clone(), false);
                        'after: {
                            player.set_sprinting(event.is_sprinting);
                            player.update_player_pose();
                        }
                    }}
                }
            }
            Action::LeaveBed => player.wake_up(),

            Action::StartHorseJump => {
                if let Some(vehicle) = player.get_entity().get_vehicle()
                    && let Some(mob) = vehicle.get_mob()
                {
                    mob.on_player_start_jump(command.jump_boost.0);
                }
            }
            Action::StopHorseJump => {
                if let Some(vehicle) = player.get_entity().get_vehicle()
                    && let Some(mob) = vehicle.get_mob()
                {
                    mob.on_player_stop_jump();
                }
            }
            // player: open the ridden vehicle inventory (ServerGamePacketListenerImpl OPEN_INVENTORY)
            Action::OpenVehicleInventory => {
                if let Some(vehicle) = player.get_entity().get_vehicle() {
                    if let Some(boat) = vehicle
                        .cast_any()
                        .downcast_ref::<crate::entity::vehicle::boat::BoatEntity>()
                    {
                        boat.open_chest(player);
                    } else if let Some(mob) = vehicle.get_mob() {
                        mob.open_custom_inventory(player);
                    }
                }
            }
            Action::StartFlyingElytra => {
                player.try_start_fall_flying(server);
            }
            // <= 1.21.5
            Action::StartSneaking | Action::StopSneaking => {
                self.handle_player_input(
                    player,
                    &SPlayerInput {
                        input: SPlayerInput::SNEAK,
                    },
                    server,
                );
            }
        }
    }
}
