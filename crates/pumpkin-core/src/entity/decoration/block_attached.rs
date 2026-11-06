use std::sync::atomic::{AtomicI32, Ordering};

use pumpkin_data::damage::DamageType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_util::math::position::BlockPos;

use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::world::explosion::Explosion;
use crate::world::game_event::GameEventContext;

// block attached: BlockAttachedEntity.CHECK_INTERVAL
pub const CHECK_INTERVAL: i32 = 100;

/// Vanilla `BlockAttachedEntity`: cushions, paintings, frames and leash knots.
pub trait BlockAttached: EntityBase + Sized {
    fn ticks_since_check(&self) -> &AtomicI32;

    fn survives(&self) -> bool;

    fn drop_item(&self, caused_by: Option<&dyn EntityBase>);

    fn tick_at_check_interval(&self) {}

    fn attached_pos(&self) -> BlockPos {
        self.get_entity().block_pos.load()
    }

    // block attached: BlockAttachedEntity.tick
    fn block_attached_tick(&self) {
        if self.ticks_since_check().fetch_add(1, Ordering::Relaxed) < CHECK_INTERVAL {
            return;
        }
        self.ticks_since_check().store(0, Ordering::Relaxed);
        self.tick_at_check_interval();
        if !self.get_entity().is_removed() && !self.survives() {
            self.get_entity().remove();
            self.drop_item(None);
        }
    }

    // block attached: BlockAttachedEntity.kill, ENTITY_DIE
    fn kill_attached(&self, attributed_to: Option<&dyn EntityBase>) {
        let entity = self.get_entity();
        entity.remove();
        entity.world.load().emit_game_event_with_context(
            GameEvent::EntityDie.name(),
            entity.pos.load(),
            &GameEventContext {
                source_entity: attributed_to.or(Some(self as &dyn EntityBase)),
            },
        );
    }

    // block attached: BlockAttachedEntity.hurtServer
    fn block_attached_hurt(
        &self,
        damage_type: &DamageType,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let entity = self.get_entity();
        let attacker = cause.or(source);
        if entity.is_invulnerable_to(damage_type, attacker) {
            return false;
        }
        let world = entity.world.load();
        if !world.level_info.load().game_rules.mob_griefing
            && attacker.is_some_and(|attacker| attacker.get_mob().is_some())
        {
            return false;
        }
        if !entity.is_removed() {
            self.kill_attached(attacker);
            self.drop_item(attacker);
        }
        true
    }

    // block attached: BlockAttachedEntity.move and push, any movement breaks it
    fn break_when_moved(&self) {
        let entity = self.get_entity();
        if !entity.is_removed() {
            self.kill_attached(None);
            self.drop_item(None);
        }
    }
}

// block attached: BlockAttachedEntity.ignoreExplosion
#[must_use]
pub fn ignores_explosion(entity: &Entity, explosion: &Explosion) -> bool {
    if explosion.direct_source_in_water() {
        return true;
    }
    let mob_griefing = entity
        .world
        .load()
        .level_info
        .load()
        .game_rules
        .mob_griefing;
    !explosion.should_affect_blocklike_entities(mob_griefing)
}

// block attached: causedBy instanceof Player player && player.hasInfiniteMaterials()
#[must_use]
pub fn infinite_materials(caused_by: Option<&dyn EntityBase>) -> bool {
    caused_by
        .and_then(EntityBase::get_player)
        .is_some_and(Player::is_creative)
}
