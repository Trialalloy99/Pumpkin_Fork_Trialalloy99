use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::TextComponent;
use pumpkin_world::chunk_system::profiler;

use crate::command::argument_builder::{ArgumentBuilder, command, literal};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};

const DESCRIPTION: &str = "Records chunk loading, generation and sending timings.";
const PERMISSION: &str = "pumpkin:command.recording";

// recording: chunk pipeline profiler (/recording start|stop)
struct RecordingStartExecutor;

impl CommandExecutor for RecordingStartExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        let text = if profiler::begin() {
            "Chunk recording started. Stop it with /recording stop."
        } else {
            "A chunk recording is already running."
        };
        context
            .source
            .send_feedback(TextComponent::text(text), true);
        Ok(i32::from(text.starts_with("Chunk")))
    }
}

struct RecordingStopExecutor;

impl CommandExecutor for RecordingStopExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        let Some(report) = profiler::finish() else {
            context
                .source
                .send_feedback(TextComponent::text("No chunk recording is running."), false);
            return Ok(0);
        };
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = std::path::Path::new("recordings").join(format!("chunks-{secs}.txt"));
        let saved =
            std::fs::create_dir_all("recordings").and_then(|()| std::fs::write(&path, &report));
        let summary: String = report.lines().take(3).collect::<Vec<_>>().join("\n");
        let text = match saved {
            Ok(()) => format!("{summary}\nSaved to {}", path.display()),
            Err(err) => {
                tracing::warn!("Failed to save chunk recording: {err}");
                format!("{summary}\nCould not save the report: {err}")
            }
        };
        tracing::info!("Chunk recording:\n{report}");
        context
            .source
            .send_feedback(TextComponent::text(text), true);
        Ok(1)
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Four),
    ));

    dispatcher.register(
        command("recording", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("start").executes(RecordingStartExecutor))
            .then(literal("stop").executes(RecordingStopExecutor)),
    );
}
