use super::block_attached::{self, BlockAttached};
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::world::World;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

pub struct LeashKnotEntity {
    entity: Entity,
    ticks_since_check: AtomicI32,
}

impl BlockAttached for LeashKnotEntity {
    fn ticks_since_check(&self) -> &AtomicI32 {
        &self.ticks_since_check
    }

    // leash knot: LeashFenceKnotEntity.survives
    fn survives(&self) -> bool {
        self.entity
            .world
            .load()
            .get_block(&self.entity.block_pos.load())
            .has_tag(&pumpkin_data::tag::Block::MINECRAFT_FENCES)
    }

    // leash knot: LeashFenceKnotEntity.dropItem, only the sound
    fn drop_item(&self, _caused_by: Option<&dyn EntityBase>) {
        self.entity.play_sound(Sound::ItemLeadUntied);
    }
}

impl LeashKnotEntity {
    pub const OFFSET_Y: f64 = 0.375;

    pub const fn new(entity: Entity) -> Self {
        Self {
            entity,
            ticks_since_check: AtomicI32::new(0),
        }
    }

    // leash knot: LeashFenceKnotEntity.notifyLeasheeRemoved, a knot without leashed mobs goes away
    pub fn notify_leashee_removed(knot: &Arc<dyn EntityBase>) {
        let entity = knot.get_entity();
        if entity.is_removed() {
            return;
        }
        let world = entity.world.load();
        let pos = entity.pos.load();
        let search = BoundingBox::new_from_pos(
            pos.x,
            pos.y,
            pos.z,
            &EntityDimensions {
                width: 64.0,
                height: 64.0,
                eye_height: 32.0,
            },
        );
        let still_used = world.get_entities_at_box(&search).into_iter().any(|other| {
            other
                .get_entity()
                .leashed_to
                .try_lock()
                .ok()
                .and_then(|holder| {
                    holder
                        .as_ref()
                        .map(|holder| holder.get_entity().entity_id == entity.entity_id)
                })
                .unwrap_or(true)
        });
        if !still_used {
            entity.remove();
        }
    }

    pub fn get_or_create(world: &Arc<World>, pos: BlockPos) -> Arc<dyn EntityBase> {
        Self::get_knot(world, pos).unwrap_or_else(|| Self::create_knot(world, pos))
    }

    pub fn get_knot(world: &Arc<World>, pos: BlockPos) -> Option<Arc<dyn EntityBase>> {
        let center = Vector3::new(
            f64::from(pos.0.x) + 0.5,
            f64::from(pos.0.y) + Self::OFFSET_Y,
            f64::from(pos.0.z) + 0.5,
        );

        let search_dim = EntityDimensions {
            width: 2.0,
            height: 2.0,
            eye_height: 1.0,
        };

        let search_box = BoundingBox::new_from_pos(center.x, center.y, center.z, &search_dim);
        world
            .get_entities_at_box(&search_box)
            .into_iter()
            .find(|entity| {
                let entity = entity.get_entity();
                entity.entity_type == &EntityType::LEASH_KNOT && entity.block_pos.load() == pos
            })
    }

    pub fn create_knot(world: &Arc<World>, pos: BlockPos) -> Arc<dyn EntityBase> {
        let raw_pos = Vector3::new(
            f64::from(pos.0.x) + 0.5,
            f64::from(pos.0.y) + Self::OFFSET_Y,
            f64::from(pos.0.z) + 0.5,
        );

        let entity = Entity::new(world.clone(), raw_pos, &EntityType::LEASH_KNOT);
        let knot: Arc<dyn EntityBase> = Arc::new(Self::new(entity));
        world.spawn_entity(knot.clone());

        world.play_sound(Sound::ItemLeadTied, SoundCategory::Neutral, &raw_pos);

        knot
    }

    pub fn play_placement_sound(&self, world: &World) {
        let pos = self.entity.pos.load();
        world.play_sound(Sound::ItemLeadTied, SoundCategory::Neutral, &pos);
    }
}

use crate::server::Server;

impl EntityBase for LeashKnotEntity {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
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

    fn ignore_explosion(&self, explosion: &crate::world::explosion::Explosion) -> bool {
        block_attached::ignores_explosion(&self.entity, explosion)
    }

    fn on_lightning_strike(
        &self,
        _caller: &dyn EntityBase,
        _lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
    }

    // leash knot: BlockAttachedEntity.hurtServer
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

    fn interact(&self, player: &Arc<Player>, _item_stack: &mut ItemStack) -> bool {
        let world = player.world();
        let knot_id = self.entity.entity_id;
        let player_id = player.entity_id();

        let search_dim = EntityDimensions {
            width: 32.0,
            height: 32.0,
            eye_height: 16.0,
        };
        let pos = self.entity.pos.load();
        let search_box = BoundingBox::new_from_pos(pos.x, pos.y, pos.z, &search_dim);
        let entities = world.get_entities_at_box(&search_box);

        let mut attached_mob = false;
        let mut player_leashed_mobs = Vec::new();

        for entity_base in &entities {
            let ent = entity_base.get_entity();
            if let Ok(guard) = ent.leashed_to.try_lock()
                && let Some(holder) = guard.as_ref()
                && holder.get_entity().entity_id == player_id
            {
                player_leashed_mobs.push(ent);
            }
        }

        if let Some(self_knot) = Self::get_knot(&world, self.entity.block_pos.load()) {
            for mob in player_leashed_mobs {
                mob.leash_to(self_knot.clone());
                attached_mob = true;
            }
        }

        let mut any_dropped = false;
        if !attached_mob && !player.get_entity().is_sneaking() {
            for entity_base in &entities {
                let ent = entity_base.get_entity();
                if let Ok(guard) = ent.leashed_to.try_lock()
                    && let Some(holder) = guard.as_ref()
                    && holder.get_entity().entity_id == knot_id
                {
                    ent.leash_to(player.clone() as Arc<dyn EntityBase>);
                    any_dropped = true;
                }
            }
        }

        if attached_mob || any_dropped {
            world.emit_game_event_with_context(
                GameEvent::BlockAttach.name(),
                self.entity.pos.load(),
                &crate::world::game_event::GameEventContext {
                    source_entity: Some(player.as_ref()),
                },
            );
            self.play_placement_sound(&world);
            true
        } else {
            false
        }
    }
    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
