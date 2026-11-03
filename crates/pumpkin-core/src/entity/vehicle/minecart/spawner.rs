use pumpkin_data::entity::EntityStatus;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;

use crate::entity::Entity;
use crate::world::base_spawner::BaseSpawner;

// minecart spawner: MinecartSpawner
pub(super) struct SpawnerMinecart {
    spawner: BaseSpawner,
}

impl SpawnerMinecart {
    pub(super) const fn new() -> Self {
        Self {
            spawner: BaseSpawner::new(),
        }
    }

    // minecart spawner: tick -> spawner.serverTick(level, blockPosition())
    pub(super) fn tick(&self, entity: &Entity) {
        let world = entity.world.load_full();
        let pos = entity.block_pos.load();
        self.spawner.server_tick(&world, pos, &|id| {
            // BaseSpawner.broadcastEvent -> level.broadcastEntityEvent(minecart, id)
            if id == crate::world::base_spawner::EVENT_SPAWN {
                world.send_entity_status(entity, EntityStatus::Jump, ActorEventID::None);
            }
        });
    }

    pub(super) fn write_nbt(&self, nbt: &mut NbtCompound) {
        self.spawner.save(nbt);
    }

    pub(super) fn read_nbt(&self, nbt: &NbtCompound) {
        self.spawner.load(nbt);
    }
}
