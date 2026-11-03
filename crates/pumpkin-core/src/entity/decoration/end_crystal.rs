use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::entity::boss::ender_dragon::EnderDragonEntity;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::server::Server;
use pumpkin_data::{
    Block,
    damage::DamageType,
    tag::{self, Taggable},
};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::world::BlockFlags;

pub struct EndCrystalEntity {
    entity: Entity,
    beam_target: Mutex<Option<BlockPos>>,
    show_bottom: AtomicBool,
    invulnerable: AtomicBool,
}

impl EndCrystalEntity {
    #[must_use]
    pub const fn new(mut entity: Entity) -> Self {
        entity.blocks_building = true;
        Self {
            entity,
            beam_target: Mutex::new(None),
            show_bottom: AtomicBool::new(false),
            invulnerable: AtomicBool::new(false),
        }
    }

    pub fn set_show_bottom(&self, show_bottom: bool) {
        self.show_bottom.store(show_bottom, Ordering::Relaxed);
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::end_crystal::SHOW_BOTTOM,
            show_bottom,
        );
    }

    pub fn show_bottom(&self) -> bool {
        self.show_bottom.load(Ordering::Relaxed)
    }

    pub fn set_beam_target(&self, beam_target: Option<BlockPos>) {
        *self
            .beam_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = beam_target;
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::end_crystal::BEAM_TARGET,
            beam_target,
        );
    }

    pub fn beam_target(&self) -> Option<BlockPos> {
        *self
            .beam_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_invulnerable(&self, invulnerable: bool) {
        self.invulnerable.store(invulnerable, Ordering::Relaxed);
    }

    pub fn is_invulnerable(&self) -> bool {
        self.invulnerable.load(Ordering::Relaxed)
    }

    // end crystal: tell the fight and the dragon (EndCrystal.onDestroyedBy)
    fn on_destroyed_by(&self, attacker: Option<std::sync::Arc<crate::entity::player::Player>>) {
        let world = self.entity.world.load_full();
        let Some(fight) = world.dragon_fight.as_ref() else {
            return;
        };
        let dragon_uuid = fight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .on_crystal_destroyed(&world, self.entity.entity_uuid);
        if let Some(uuid) = dragon_uuid
            && let Some(dragon) = world.get_entity_by_uuid(uuid)
            && let Some(dragon) = dragon.cast_any().downcast_ref::<EnderDragonEntity>()
        {
            dragon.on_crystal_destroyed(self, self.entity.pos.load(), attacker);
        }
    }
}

impl EntityBase for EndCrystalEntity {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    // end crystal: fire below it during the dragon fight (EndCrystal.tick)
    fn tick(&self, caller: &dyn EntityBase, server: &Server) {
        self.entity.tick(caller, server);
        let world = self.entity.world.load();
        if world.dragon_fight.is_some() {
            let pos = self.entity.block_pos.load();
            if world.get_block_state(&pos).is_air() {
                world.set_block_state(&pos, Block::FIRE.default_state.id, BlockFlags::NOTIFY_ALL);
            }
        }
    }

    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        _amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        if self.is_invulnerable() || self.entity.is_removed() {
            return false;
        }
        let attacker = cause.or(source);
        // the dragon never destroys the crystals (EndCrystal.hurtServer)
        if attacker.is_some_and(|attacker| {
            attacker.get_entity().entity_type == &pumpkin_data::entity::EntityType::ENDER_DRAGON
        }) {
            return false;
        }

        self.entity.remove();
        let world = self.entity.world.load();
        if !damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_EXPLOSION) {
            let pos = self.entity.pos.load();
            world.explode(pos, 6.0, crate::world::ExplosionInteraction::Block);
        }
        let player = attacker
            .and_then(|attacker| attacker.get_player())
            .and_then(|player| world.get_player_by_id(player.entity_id()));
        self.on_destroyed_by(player);
        true
    }

    // end crystal: killing it counts as destroying it (EndCrystal.kill)
    fn kill(&self, _caller: &dyn EntityBase) {
        if !self.entity.is_removed() {
            self.entity.remove();
            self.on_destroyed_by(None);
        }
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
