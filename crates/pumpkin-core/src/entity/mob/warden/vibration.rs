//! The warden's `VibrationSystem` user, ported from `Warden.VibrationUser` and
//! `VibrationSystem.Listener/Ticker`.

use std::sync::Arc;

use pumpkin_data::{game_event::GameEvent, particle::Particle, tag};
use pumpkin_protocol::{codec::var_int::VarInt, java::client::play::CParticle};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};

use super::WardenEntity;
use crate::entity::EntityBase;
use crate::entity::ai::brain::memory::types;
use crate::entity::passive::allay::vibration::{
    VibrationData, VibrationInfo, is_occluded, vibration_payload,
};
use crate::world::{
    World,
    game_event::{GameEventContext, GameEventListener},
};

pub const GAME_EVENT_LISTENER_RANGE: i32 = 16;
const PARTICLE_SEND_DISTANCE: f64 = 32.0;

/// Vanilla `VibrationSystem.VIBRATION_FREQUENCY_FOR_EVENT`.
const fn frequency(event: GameEvent) -> i32 {
    use GameEvent as E;
    match event {
        E::Step | E::Swim | E::Flap | E::Resonate1 => 1,
        E::ProjectileLand | E::HitGround | E::Splash | E::Bounce | E::Resonate2 => 2,
        E::ItemInteractFinish | E::ProjectileShoot | E::InstrumentPlay | E::Resonate3 => 3,
        E::EntityAction | E::ElytraGlide | E::Unequip | E::Resonate4 => 4,
        E::EntityDismount | E::Equip | E::Resonate5 => 5,
        E::EntityInteract | E::Shear | E::EntityMount | E::Resonate6 => 6,
        E::EntityDamage | E::Resonate7 => 7,
        E::Drink | E::Eat | E::Resonate8 => 8,
        E::ContainerClose | E::BlockClose | E::BlockDeactivate | E::BlockDetach | E::Resonate9 => 9,
        E::ContainerOpen
        | E::BlockOpen
        | E::BlockActivate
        | E::BlockAttach
        | E::PrimeFuse
        | E::NoteBlockPlay
        | E::Resonate10 => 10,
        E::BlockChange | E::Resonate11 => 11,
        E::BlockDestroy | E::FluidPickup | E::Resonate12 => 12,
        E::BlockPlace | E::FluidPlace | E::Resonate13 => 13,
        E::EntityPlace | E::LightningStrike | E::Teleport | E::Resonate14 => 14,
        E::EntityDie | E::Explode | E::Resonate15 => 15,
        _ => 0,
    }
}

pub struct WardenVibrationListener<'a>(pub &'a WardenEntity);

impl GameEventListener for WardenVibrationListener<'_> {
    fn listener_position(&self) -> Vector3<f64> {
        self.0.get_eye_pos()
    }

    fn listener_radius(&self) -> i32 {
        GAME_EVENT_LISTENER_RANGE
    }

    fn handle_game_event(
        &self,
        world: &World,
        event: GameEvent,
        context: &GameEventContext<'_>,
        source_position: Vector3<f64>,
    ) -> bool {
        let warden = self.0;
        if lock(&warden.vibration).current.is_some() || !is_valid_vibration(event, context) {
            return false;
        }
        let destination = warden.get_eye_pos();
        if !warden.can_receive_vibration(world, context)
            || is_occluded(world, source_position, destination)
        {
            return false;
        }
        let time = world.get_world_age();
        let distance = (source_position - destination).length() as f32;
        let source = context.source_entity;
        let info = VibrationInfo {
            event,
            distance,
            pos: source_position,
            source: source.map(|entity| entity.get_entity().entity_uuid),
            projectile_owner: source
                .and_then(|entity| entity.get_projectile_owner())
                .and_then(|owner| owner.uuid()),
        };
        let mut data = lock(&warden.vibration);
        // vanilla VibrationSelector.shouldReplaceVibration
        let replace = data
            .candidate
            .as_ref()
            .is_none_or(|(previous, previous_time)| {
                *previous_time == time
                    && (distance < previous.distance
                        || (distance <= previous.distance
                            && frequency(event) > frequency(previous.event)))
            });
        if replace {
            data.candidate = Some((info, time));
        }
        true
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Vanilla `VibrationSystem.User.isValidVibration`.
fn is_valid_vibration(event: GameEvent, context: &GameEventContext<'_>) -> bool {
    if !tag::GameEvent::MINECRAFT_WARDEN_CAN_LISTEN
        .0
        .contains(&event.name())
    {
        return false;
    }
    let Some(source) = context.source_entity else {
        return true;
    };
    if source.is_spectator() {
        return false;
    }
    if source.get_entity().is_sneaking()
        && tag::GameEvent::MINECRAFT_IGNORE_VIBRATIONS_SNEAKING
            .0
            .contains(&event.name())
    {
        // the warden lets players earn the avoid vibration advancement
        if let Some(player) = source.get_player() {
            player.trigger_advancement(
                crate::entity::player::advancement::trigger::AdvancementTrigger::AvoidVibration,
            );
        }
        return false;
    }
    !source.dampens_vibrations()
}

impl WardenEntity {
    /// Vanilla `VibrationUser.canReceiveVibration`; a busy brain counts as not listening.
    pub(super) fn can_receive_vibration(
        &self,
        world: &World,
        context: &GameEventContext<'_>,
    ) -> bool {
        if self.mob_entity.is_no_ai()
            || self.mob_entity.living_entity.is_dead_or_dying()
            || self.is_digging_or_emerging()
        {
            return false;
        }
        if context.source_entity.is_some_and(|source| {
            source.get_living_entity().is_some() && !self.can_target_entity(source)
        }) {
            return false;
        }
        let _ = world;
        self.mob_entity
            .try_with_brain(self, |brain| {
                !brain.has_memory_value(types::VIBRATION_COOLDOWN.id())
            })
            .unwrap_or(false)
    }

    /// Vanilla `VibrationSystem.Ticker.tick`.
    pub(super) fn tick_vibration(&self) {
        let world = self.get_entity().world.load_full();
        let mut data: std::sync::MutexGuard<'_, VibrationData> = lock(&self.vibration);
        if data.current.is_none()
            && data
                .candidate
                .as_ref()
                .is_some_and(|(_, time)| *time < world.get_world_age())
            && let Some((event, _)) = data.candidate.take()
        {
            data.delay = event.distance.floor() as i32;
            self.send_vibration_particle(event.pos, data.delay);
            data.current = Some(event);
        }
        if data.reload_particle {
            if let Some(event) = &data.current {
                let initial = (event.distance.floor() as i32).max(1);
                let alpha = 1.0 - f64::from(data.delay) / f64::from(initial);
                let pos = event.pos.lerp(&self.get_eye_pos(), alpha);
                if self.send_vibration_particle(pos, data.delay) {
                    data.reload_particle = false;
                }
            } else {
                data.reload_particle = false;
            }
        }
        if data.current.is_none() {
            return;
        }
        data.delay = (data.delay - 1).max(0);
        if data.delay == 0 {
            let event = data.current.take();
            drop(data);
            if let Some(event) = event {
                self.receive_vibration(&world, &event);
            }
        }
    }

    fn receive_vibration(&self, world: &Arc<World>, event: &VibrationInfo) {
        let origin = BlockPos::floored_v(event.pos);
        let destination = BlockPos::floored_v(self.get_eye_pos());
        let source = event.source.and_then(|uuid| world.get_entity_by_uuid(uuid));
        let projectile_owner = event
            .projectile_owner
            .and_then(|uuid| world.get_entity_by_uuid(uuid));
        let distance = (origin.0.x - destination.0.x).pow(2)
            + (origin.0.y - destination.0.y).pow(2)
            + (origin.0.z - destination.0.z).pow(2);
        self.on_receive_vibration(
            world,
            origin,
            source.as_ref(),
            projectile_owner.as_ref(),
            (distance as f32).sqrt(),
        );
    }

    fn send_vibration_particle(&self, pos: Vector3<f64>, delay: i32) -> bool {
        let Ok(payload) = vibration_payload(
            self.get_entity().entity_id,
            self.get_entity().get_eye_height() as f32,
            delay,
        ) else {
            return false;
        };
        let packet = CParticle::new(
            false,
            false,
            pos,
            Vector3::new(0.0, 0.0, 0.0),
            0.0,
            1,
            VarInt(i32::from(Particle::Vibration.to_id())),
            &payload,
        );
        let world = self.get_entity().world.load();
        let mut sent = false;
        for player in world.players.load().iter() {
            if player.position().squared_distance_to_vec(&pos)
                <= PARTICLE_SEND_DISTANCE * PARTICLE_SEND_DISTANCE
            {
                player.try_send_client_packet(&packet);
                sent = true;
            }
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closer_or_louder_vibrations_win_the_tick() {
        assert!(frequency(GameEvent::Explode) > frequency(GameEvent::Step));
        assert_eq!(frequency(GameEvent::Resonate7), 7);
        assert_eq!(frequency(GameEvent::JukeboxPlay), 0);
    }
}
