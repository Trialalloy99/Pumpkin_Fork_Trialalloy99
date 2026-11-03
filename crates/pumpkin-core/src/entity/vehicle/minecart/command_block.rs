use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::{math::position::BlockPos, text::TextComponent};

use crate::block::entities::command_block::CommandBlockEntity;
use crate::command::CommandSender;
use crate::entity::{Entity, EntityBase};

// minecart command block: MinecartCommandBlock.ACTIVATION_DELAY
const ACTIVATION_DELAY: i32 = 4;

// minecart command block: MinecartCommandBlock + BaseCommandBlock state
pub(super) struct CommandBlockMinecart {
    store: Arc<CommandBlockEntity>,
    last_activated: AtomicI32,
}

impl CommandBlockMinecart {
    pub(super) fn new() -> Self {
        Self {
            store: Arc::new(CommandBlockEntity::new(BlockPos::new(0, 0, 0), true, false)),
            last_activated: AtomicI32::new(0),
        }
    }

    fn command(&self) -> String {
        self.store
            .command
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn last_output(&self) -> String {
        self.store
            .last_output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    // minecart command block: MinecartCommandBase.onUpdated
    pub(super) fn sync(&self, entity: &Entity) {
        entity.set_synced_data(
            pumpkin_data::tracked_data::minecart_command_block::ID_COMMAND_NAME,
            self.command(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::minecart_command_block::ID_LAST_OUTPUT,
            TextComponent::text(self.last_output()),
        );
    }

    // minecart command block: ServerGamePacketListenerImpl.handleSetCommandMinecart
    pub(super) fn set_command(&self, entity: &Entity, command: &str, track_output: bool) {
        *self
            .store
            .command
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            command.strip_prefix('/').unwrap_or(command).to_string();
        self.store
            .track_output
            .store(track_output, Ordering::SeqCst);
        if !track_output {
            self.store
                .last_output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
        self.sync(entity);
    }

    // minecart command block: activateMinecart on a powered activator rail
    pub(super) fn activate(&self, entity: &Arc<dyn EntityBase>) {
        let base = entity.get_entity();
        let tick = base.tick_count.load(Ordering::Relaxed);
        if tick - self.last_activated.load(Ordering::Relaxed) < ACTIVATION_DELAY {
            return;
        }
        self.perform_command(entity);
        self.last_activated.store(tick, Ordering::Relaxed);
    }

    // minecart command block: BaseCommandBlock.performCommand
    fn perform_command(&self, entity: &Arc<dyn EntityBase>) {
        let base = entity.get_entity();
        let world = base.world.load_full();
        if !world.level_info.load().game_rules.command_blocks_work {
            return;
        }
        self.store.success_count.store(0, Ordering::Release);
        let command = self.command();
        if !command.is_empty() {
            self.store
                .last_output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            if let Some(server) = world.server.upgrade() {
                let source = CommandSender::CommandMinecart(self.store.clone(), entity.clone())
                    .into_source(&server);
                server
                    .command_dispatcher
                    .load()
                    .handle_command(&source, &command);
            }
        }
        self.sync(base);
    }

    pub(super) fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_string("Command", self.command());
        nbt.put_int(
            "SuccessCount",
            self.store
                .success_count
                .load(Ordering::SeqCst)
                .cast_signed(),
        );
        let track_output = self.store.track_output.load(Ordering::SeqCst);
        nbt.put_bool("TrackOutput", track_output);
        let last_output = self.last_output();
        if track_output && !last_output.is_empty() {
            nbt.put_string("LastOutput", last_output);
        }
    }

    pub(super) fn read_nbt(&self, nbt: &NbtCompound) {
        *self
            .store
            .command
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            nbt.get_string("Command").unwrap_or("").to_string();
        self.store.success_count.store(
            nbt.get_int("SuccessCount").unwrap_or(0).cast_unsigned(),
            Ordering::SeqCst,
        );
        let track_output = nbt.get_bool("TrackOutput").unwrap_or(true);
        self.store
            .track_output
            .store(track_output, Ordering::SeqCst);
        *self
            .store
            .last_output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = if track_output {
            nbt.get_string("LastOutput").unwrap_or("").to_string()
        } else {
            String::new()
        };
    }
}
