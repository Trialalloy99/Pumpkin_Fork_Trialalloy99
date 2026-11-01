use pumpkin_data::translation;
use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::TextComponent;

use crate::command::argument_builder::{ArgumentBuilder, command, literal};
use crate::command::context::command_context::CommandContext;
use crate::command::errors::error_types::CommandErrorType;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::metrics_recorder::{StartRecordingError, StopRecordingError};

const DESCRIPTION: &str = "Captures a ten second performance profile of the server.";
const PERMISSION: &str = "minecraft:command.perf";

const ALREADY_RUNNING_ERROR_TYPE: CommandErrorType<0> = CommandErrorType::new(
    translation::java::COMMANDS_PERF_ALREADYRUNNING,
    translation::java::COMMANDS_PERF_ALREADYRUNNING,
);

const NOT_RUNNING_ERROR_TYPE: CommandErrorType<0> = CommandErrorType::new(
    translation::java::COMMANDS_PERF_NOTRUNNING,
    translation::java::COMMANDS_PERF_NOTRUNNING,
);

struct PerfStartExecutor;

impl CommandExecutor for PerfStartExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        context
            .server()
            .metrics_recorder
            .start(context.source.clone())
            .map_err(|StartRecordingError::AlreadyRunning| {
                ALREADY_RUNNING_ERROR_TYPE.create_without_context()
            })?;
        context.source.send_feedback(
            TextComponent::translate_cross(
                translation::java::COMMANDS_PERF_STARTED,
                translation::java::COMMANDS_PERF_STARTED,
                [],
            ),
            false,
        );
        Ok(0)
    }
}

struct PerfStopExecutor;

impl CommandExecutor for PerfStopExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        context
            .server()
            .metrics_recorder
            .end()
            .map_err(|StopRecordingError::NotRunning| {
                NOT_RUNNING_ERROR_TYPE.create_without_context()
            })?;
        Ok(0)
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Four),
    ));

    dispatcher.register(
        command("perf", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("start").executes(PerfStartExecutor))
            .then(literal("stop").executes(PerfStopExecutor)),
    );
}
