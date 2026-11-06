use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

use crate::entity::item::ItemEntity;
use crate::entity::projectile::ThrownItemEntity;
use crate::entity::projectile::arrow::{ArrowEntity, ArrowPickup};
use crate::entity::projectile::lingering_potion::LingeringPotionEntity;
use crate::entity::projectile::owner::ProjectileOwner;
use crate::entity::projectile::small_fireball::SmallFireballEntity;
use crate::entity::projectile::splash_potion::SplashPotionEntity;
use crate::entity::projectile::wind_charge::WindChargeEntity;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::server::Server;
use crate::world::World;

// ominous item spawner: SPAWN_ITEM_DELAY_MIN/MAX, TICKS_BEFORE_ABOUT_TO_SPAWN_SOUND
const SPAWN_ITEM_DELAY_MIN: i64 = 60;
const SPAWN_ITEM_DELAY_MAX: i64 = 120;
const TICKS_BEFORE_ABOUT_TO_SPAWN_SOUND: i64 = 36;
// ominous item spawner: ProjectileItem.DispenseConfig defaults and the potion/charge overrides
const DEFAULT_POWER: f64 = 1.1;
const DEFAULT_UNCERTAINTY: f64 = 6.0;
const POTION_POWER: f64 = 1.375;
const POTION_UNCERTAINTY: f64 = 3.0;
const CHARGE_POWER: f64 = 1.0;
const CHARGE_UNCERTAINTY: f64 = 6.666_666_5;
const FIRE_CHARGE_SPREAD: f64 = 0.114_85;

/// Vanilla `OminousItemSpawner`: the trial spawner's delayed item or projectile drop.
pub struct OminousItemSpawnerEntity {
    entity: Entity,
    item: Mutex<ItemStack>,
    spawn_item_after_ticks: AtomicI64,
    spawned: AtomicBool,
}

impl OminousItemSpawnerEntity {
    // ominous item spawner: OminousItemSpawner.create
    pub fn create(world: &Arc<World>, pos: Vector3<f64>, item: ItemStack) -> Arc<Self> {
        let entity = Entity::new(world.clone(), pos, &EntityType::OMINOUS_ITEM_SPAWNER);
        entity.no_physics.store(true, Ordering::Relaxed);
        Arc::new(Self {
            entity,
            item: Mutex::new(item),
            spawn_item_after_ticks: AtomicI64::new(
                rand::rng().random_range(SPAWN_ITEM_DELAY_MIN..=SPAWN_ITEM_DELAY_MAX),
            ),
            spawned: AtomicBool::new(false),
        })
    }

    pub fn new(entity: Entity) -> Self {
        entity.no_physics.store(true, Ordering::Relaxed);
        Self {
            entity,
            item: Mutex::new(ItemStack::EMPTY.clone()),
            spawn_item_after_ticks: AtomicI64::new(0),
            spawned: AtomicBool::new(false),
        }
    }

    fn item(&self) -> ItemStack {
        self.item
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_item(&self, item: ItemStack) {
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::ominous_item_spawner::DATA_ITEM,
            ItemStackSerializer::from(item.clone()),
        );
        *self
            .item
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = item;
    }

    // ominous item spawner: OminousItemSpawner.spawnItem
    fn spawn_item(&self) {
        let item = self.item();
        if item.is_empty() {
            return;
        }
        let world = self.entity.world.load_full();
        let pos = self.entity.pos.load();
        let spawned: Arc<dyn EntityBase> = self
            .spawn_projectile(&world, &item, pos)
            .unwrap_or_else(|| {
                let drop = Arc::new(ItemEntity::new(
                    Entity::new(world.clone(), pos, &EntityType::ITEM),
                    item.clone(),
                ));
                world.spawn_entity(drop.clone());
                drop
            });
        world.sync_world_event(
            WorldEvent::ParticlesTrialSpawnerSpawnItem,
            self.entity.block_pos.load(),
            1,
        );
        world.emit_game_event_with_context(
            GameEvent::EntityPlace.name(),
            pos,
            &crate::world::game_event::GameEventContext {
                source_entity: Some(spawned.as_ref()),
            },
        );
        self.set_item(ItemStack::EMPTY.clone());
    }

    // ominous item spawner: OminousItemSpawner.spawnProjectile, shot straight down
    fn spawn_projectile(
        &self,
        world: &Arc<World>,
        item: &ItemStack,
        pos: Vector3<f64>,
    ) -> Option<Arc<dyn EntityBase>> {
        let item_type = item.item;
        let down = Vector3::new(0.0, -1.0, 0.0);
        let entity =
            |entity_type: &'static EntityType| Entity::new(world.clone(), pos, entity_type);
        let thrown = |entity: Entity, gravity: f64| ThrownItemEntity {
            entity,
            owner: ProjectileOwner::none(),
            collides_with_projectiles: false,
            has_hit: AtomicBool::new(false),
            gravity,
        };
        let projectile: Arc<dyn EntityBase> = if item_type == &Item::ARROW
            || item_type == &Item::SPECTRAL_ARROW
            || item_type == &Item::TIPPED_ARROW
        {
            let arrow = ArrowEntity::new_with_item(
                entity(ArrowEntity::entity_type_for_item(item_type)),
                Some(&self.entity),
                item,
                ArrowPickup::Allowed,
            );
            arrow.apply_on_projectile_spawned(item);
            arrow.set_velocity(down.x, down.y, down.z, DEFAULT_POWER, DEFAULT_UNCERTAINTY);
            Arc::new(arrow)
        } else if item_type == &Item::LINGERING_POTION {
            let potion = LingeringPotionEntity::new(entity(&EntityType::LINGERING_POTION));
            potion.set_item_stack(item.copy_with_count(1));
            potion.thrown.owner.set(Some(&self.entity));
            potion
                .thrown
                .set_velocity(down.x, down.y, down.z, POTION_POWER, POTION_UNCERTAINTY);
            Arc::new(potion)
        } else if item_type == &Item::SPLASH_POTION {
            let potion = SplashPotionEntity::new(entity(&EntityType::SPLASH_POTION));
            potion.set_item_stack(item.copy_with_count(1));
            potion.thrown.owner.set(Some(&self.entity));
            potion
                .thrown
                .set_velocity(down.x, down.y, down.z, POTION_POWER, POTION_UNCERTAINTY);
            Arc::new(potion)
        } else if item_type == &Item::FIRE_CHARGE {
            let fireball = SmallFireballEntity::new(entity(&EntityType::SMALL_FIREBALL));
            let mut rng = rand::rng();
            let mut spread = |center: f64| {
                center + FIRE_CHARGE_SPREAD * (rng.random::<f64>() - rng.random::<f64>())
            };
            let direction =
                Vector3::new(spread(down.x), spread(down.y), spread(down.z)).normalize();
            fireball.thrown.owner.set(Some(&self.entity));
            fireball.get_entity().set_velocity(direction);
            fireball.get_entity().set_rotation(
                direction.x.atan2(direction.z) as f32 * 57.295_776,
                direction.y.atan2(direction.horizontal_length()) as f32 * 57.295_776,
            );
            Arc::new(fireball)
        } else if item_type == &Item::WIND_CHARGE {
            let charge = thrown(entity(&EntityType::WIND_CHARGE), 0.0);
            charge.owner.set(Some(&self.entity));
            charge.set_velocity(down.x, down.y, down.z, CHARGE_POWER, CHARGE_UNCERTAINTY);
            Arc::new(WindChargeEntity::new_normal(charge))
        } else {
            return None;
        };
        world.spawn_entity(projectile.clone());
        Some(projectile)
    }
}

impl EntityBase for OminousItemSpawnerEntity {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn init_data_tracker(&self) {
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::ominous_item_spawner::DATA_ITEM,
            ItemStackSerializer::from(self.item()),
        );
    }

    // ominous item spawner: OminousItemSpawner.tickServer
    fn tick(&self, _caller: &dyn EntityBase, _server: &Server) {
        let ticks = i64::from(self.entity.tick_count.fetch_add(1, Ordering::Relaxed));
        let after = self.spawn_item_after_ticks.load(Ordering::Relaxed);
        let world = self.entity.world.load();
        if ticks == after - TICKS_BEFORE_ABOUT_TO_SPAWN_SOUND {
            world.play_sound(
                Sound::BlockTrialSpawnerAboutToSpawnItem,
                SoundCategory::Neutral,
                &self.entity.pos.load(),
            );
        }
        if ticks >= after && !self.spawned.swap(true, Ordering::Relaxed) {
            self.spawn_item();
            self.entity.remove();
        }
    }

    // ominous item spawner: hurtServer is always false
    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        _amount: f32,
        _damage_type: pumpkin_data::damage::DamageType,
        _position: Option<Vector3<f64>>,
        _source: Option<&dyn EntityBase>,
        _cause: Option<&dyn EntityBase>,
    ) -> bool {
        false
    }

    // ominous item spawner: getPistonPushReaction IGNORE_ENTITY, isIgnoringBlockTriggers
    fn is_affected_by_blocks(&self) -> bool {
        false
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let item = self.item();
        if !item.is_empty() {
            let mut stack = NbtCompound::new();
            item.write_item_stack(&mut stack);
            nbt.put_compound("item", stack);
        }
        nbt.put_long(
            "spawn_item_after_ticks",
            self.spawn_item_after_ticks.load(Ordering::Relaxed),
        );
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        self.set_item(
            nbt.get_compound("item")
                .and_then(ItemStack::read_item_stack)
                .unwrap_or_else(|| ItemStack::EMPTY.clone()),
        );
        self.spawn_item_after_ticks.store(
            nbt.get_long("spawn_item_after_ticks").unwrap_or(0),
            Ordering::Relaxed,
        );
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
