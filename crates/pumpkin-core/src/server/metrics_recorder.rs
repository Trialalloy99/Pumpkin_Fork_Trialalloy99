use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_data::translation;
use pumpkin_util::text::TextComponent;
use tracing::error;

use crate::command::context::command_source::CommandSource;
use crate::server::Server;

pub const PROFILING_MAX_DURATION_SECONDS: u64 = 10;
pub const PROFILING_RESULTS_DIR: &str = "debug/profiling";

struct MetricsRecording {
    source: Arc<CommandSource>,
    started_at: Instant,
    kill_switch: bool,
    tick_nanos: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRecordingError {
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRecordingError {
    NotRunning,
}

/// Vanilla `ActiveMetricsRecorder`: the `/perf` session, ended after ten seconds or on `/perf stop`.
#[derive(Default)]
pub struct MetricsRecorder {
    recording: Mutex<Option<MetricsRecording>>,
}

impl MetricsRecorder {
    pub fn is_recording(&self) -> bool {
        self.recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub fn start(&self, source: Arc<CommandSource>) -> Result<(), StartRecordingError> {
        let mut recording = self
            .recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recording.is_some() {
            return Err(StartRecordingError::AlreadyRunning);
        }
        *recording = Some(MetricsRecording {
            source,
            started_at: Instant::now(),
            kill_switch: false,
            tick_nanos: Vec::new(),
        });
        Ok(())
    }

    pub fn end(&self) -> Result<(), StopRecordingError> {
        self.recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .map(|recording| recording.kill_switch = true)
            .ok_or(StopRecordingError::NotRunning)
    }

    pub fn end_tick(&self, server: &Arc<Server>, tick_duration_nanos: i64) {
        let finished = {
            let mut guard = self
                .recording
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(recording) = guard.as_mut() else {
                return;
            };
            recording.tick_nanos.push(tick_duration_nanos);
            if !recording.kill_switch
                && recording.started_at.elapsed()
                    <= Duration::from_secs(PROFILING_MAX_DURATION_SECONDS)
            {
                return;
            }
            guard.take()
        };
        if let Some(recording) = finished {
            finish(server, recording);
        }
    }
}

fn finish(server: &Arc<Server>, recording: MetricsRecording) {
    let duration = recording.started_at.elapsed().as_secs_f64();
    let ticks = recording.tick_nanos.len();
    recording.source.send_feedback(
        TextComponent::translate_cross(
            translation::java::COMMANDS_PERF_STOPPED,
            translation::java::COMMANDS_PERF_STOPPED,
            [
                TextComponent::text(format!("{duration:.2}")),
                TextComponent::text(ticks.to_string()),
                TextComponent::text(format!("{:.2}", ticks as f64 / duration)),
            ],
        ),
        false,
    );

    let profiling_name = format!(
        "{}-{}-{}",
        filename_formatted_date_time(),
        server.basic_config.default_level_name,
        CURRENT_MC_VERSION
    );
    let system_report = system_report(server);
    let profiling_report = profiling_report(&recording.tick_nanos, duration);
    let tick_csv = tick_csv(&recording.tick_nanos);
    let source = recording.source;
    server.spawn_task(async move {
        let saved = tokio::task::spawn_blocking(move || {
            save_results(
                &profiling_name,
                &system_report,
                &profiling_report,
                &tick_csv,
            )
        })
        .await;
        match saved {
            Ok(Ok(zip_file)) => source.send_feedback(
                TextComponent::translate_cross(
                    translation::java::COMMANDS_PERF_REPORTSAVED,
                    translation::java::COMMANDS_PERF_REPORTSAVED,
                    [TextComponent::text(zip_file)],
                ),
                false,
            ),
            Ok(Err(err)) => {
                error!("Failed to save profiling results: {err}");
                source.send_error(TextComponent::translate_cross(
                    translation::java::COMMANDS_PERF_REPORTFAILED,
                    translation::java::COMMANDS_PERF_REPORTFAILED,
                    [],
                ));
            }
            Err(err) => {
                error!("Profiling report task failed: {err}");
                source.send_error(TextComponent::translate_cross(
                    translation::java::COMMANDS_PERF_REPORTFAILED,
                    translation::java::COMMANDS_PERF_REPORTFAILED,
                    [],
                ));
            }
        }
    });
}

fn filename_formatted_date_time() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(time::macros::format_description!(
        "[year]-[month]-[day]_[hour].[minute].[second]"
    ))
    .unwrap_or_default()
}

fn system_report(server: &Server) -> String {
    let mut report = String::new();
    let _ = writeln!(report, "Pumpkin Version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(report, "Minecraft Version: {CURRENT_MC_VERSION}");
    let _ = writeln!(
        report,
        "Operating System: {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(
        report,
        "CPUs: {}",
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    );
    let _ = writeln!(report, "Player Count: {}", server.get_player_count());
    for world in server.worlds.load().iter() {
        let _ = writeln!(
            report,
            "World {}: {} loaded chunks, {} entities",
            world.dimension.minecraft_name,
            world.level.loaded_chunk_count(),
            world.entities.load().len()
        );
    }
    report
}

fn profiling_report(tick_nanos: &[i64], duration: f64) -> String {
    let mut sorted: Vec<f64> = tick_nanos.iter().map(|&nanos| nanos as f64 / 1e6).collect();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        sorted
            .get(((sorted.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let average = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    let mut report = String::new();
    let _ = writeln!(report, "Duration: {duration:.2} s");
    let _ = writeln!(report, "Ticks: {}", sorted.len());
    let _ = writeln!(
        report,
        "Ticks per second: {:.2}",
        sorted.len() as f64 / duration
    );
    let _ = writeln!(report, "Average tick: {average:.3} ms");
    let _ = writeln!(report, "Median tick: {:.3} ms", percentile(0.5));
    let _ = writeln!(report, "95th percentile tick: {:.3} ms", percentile(0.95));
    let _ = writeln!(report, "Longest tick: {:.3} ms", percentile(1.0));
    report
}

fn tick_csv(tick_nanos: &[i64]) -> String {
    let mut csv = String::from("@tick,ticktime\n");
    for (tick, nanos) in tick_nanos.iter().enumerate() {
        let _ = writeln!(csv, "{},{:.6}", tick + 1, *nanos as f64 / 1e6);
    }
    csv
}

/// Vanilla `FileUtil.findAvailableName`: `name.zip`, else `name (1).zip`, `name (2).zip`, ...
fn find_available_name(dir: &Path, name: &str) -> String {
    let mut candidate = format!("{name}.zip");
    let mut index = 1;
    while dir.join(&candidate).exists() {
        candidate = format!("{name} ({index}).zip");
        index += 1;
    }
    candidate
}

fn save_results(
    profiling_name: &str,
    system_report: &str,
    profiling_report: &str,
    tick_csv: &str,
) -> std::io::Result<String> {
    let dir = PathBuf::from(PROFILING_RESULTS_DIR);
    std::fs::create_dir_all(&dir)?;
    let zip_file = find_available_name(&dir, profiling_name);
    let mut zip = zip::ZipWriter::new(std::fs::File::create(dir.join(&zip_file))?);
    let options = zip::write::SimpleFileOptions::default();
    for (path, contents) in [
        ("system.txt", system_report),
        ("server/profiling.txt", profiling_report),
        ("server/metrics/tick_loop.csv", tick_csv),
    ] {
        zip.start_file(path, options)
            .map_err(std::io::Error::other)?;
        zip.write_all(contents.as_bytes())?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok(zip_file)
}
