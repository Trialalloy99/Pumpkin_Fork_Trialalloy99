use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::entity::experience_orb::ExperienceOrbEntity;
use crate::entity::projectile::owner::ProjectileOwner;
use crate::entity::projectile::{ProjectileHit, ThrownItemEntity};
use crate::entity::{Entity, EntityBase};
use crate::server::Server;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use rand::RngExt;

const GRAVITY: f64 = 0.07;
const WATER_COLOR: i32 = -13_083_194; // 0xFF385DC6

pub struct ExperienceBottleEntity {
    pub thrown: ThrownItemEntity,
    pub item_stack: RwLock<ItemStack>,
}

impl ExperienceBottleEntity {
    pub fn new(entity: Entity) -> Self {
        Self {
            thrown: ThrownItemEntity {
                entity,
                owner: ProjectileOwner::none(),
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: GRAVITY,
            },
            item_stack: RwLock::new(ItemStack::new(1, &Item::EXPERIENCE_BOTTLE)),
        }
    }

    pub fn new_shot(entity: Entity, shooter: &Entity) -> Self {
        Self {
            thrown: ThrownItemEntity::new(entity, shooter, GRAVITY),
            item_stack: RwLock::new(ItemStack::new(1, &Item::EXPERIENCE_BOTTLE)),
        }
    }

    pub fn set_item_stack(&self, item_stack: ItemStack) {
        let mut write = self
            .item_stack
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *write = item_stack;
    }
}

impl EntityBase for ExperienceBottleEntity {
    fn get_projectile_owner(&self) -> Option<&ProjectileOwner> {
        Some(&self.thrown.owner)
    }

    fn init_data_tracker(&self) {
        let entity = self.get_entity();
        let stack = self
            .item_stack
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        entity.set_synced_data(
            pumpkin_data::tracked_data::experience_bottle::ITEM_STACK,
            ItemStackSerializer::from(stack.clone()),
        );
    }

    fn tick(&self, caller: &dyn EntityBase, _server: &Server) {
        self.thrown.process_tick(caller);
    }

    fn get_entity(&self) -> &Entity {
        self.thrown.get_entity()
    }

    fn get_living_entity(&self) -> Option<&crate::entity::living::LivingEntity> {
        None
    }

    fn on_hit(&self, hit: ProjectileHit) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let hit_pos = hit.hit_pos();
        // xp bottle: levelEvent at blockPosition (ThrownExperienceBottle.onHit)
        let block_pos = entity.block_pos.load();

        world.sync_world_event(
            WorldEvent::ParticlesSpellPotionSplash,
            block_pos,
            WATER_COLOR,
        );
        if !entity.silent.load(Ordering::Relaxed) {
            world.sync_world_event(WorldEvent::SoundSpellPotionSplash, block_pos, 0);
        }

        let mut rng = rand::rng();
        let xp_count = 3 + rng.random_range(0..5) + rng.random_range(0..5);
        // xp bottle: ExperienceOrb.awardWithDirection, block normal or against the flight
        let direction = match &hit {
            ProjectileHit::Block { normal, .. } => *normal,
            ProjectileHit::Entity { .. } => entity.velocity.load().multiply(-1.0, -1.0, -1.0),
        };
        ExperienceOrbEntity::spawn_with_direction(&world, hit_pos, direction, xp_count);
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let stack = self
            .item_stack
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut item = NbtCompound::new();
        stack.write_item_stack(&mut item);
        nbt.put_compound("Item", item);
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(stack) = nbt
            .get_compound("Item")
            .and_then(ItemStack::read_item_stack)
        {
            self.set_item_stack(stack.copy_with_count(1));
        }
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
