//! Chunk pipeline recorder for `/recording`: lock-free timing counters, a single
//! relaxed load when disabled.

use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use super::StagedChunkEnum;

static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Metric {
    GenStructureStart,
    GenStructureReferences,
    GenBiomes,
    GenNoise,
    GenSurface,
    GenCarvers,
    GenFeatures,
    GenLighting,
    GenSpawn,
    GenFull,
    /// dispatch on the scheduler thread until a generation worker picks the task up
    GenQueueWait,
    IoReadBatch,
    IoReadChunk,
    IoWriteBatch,
    SchedLoop,
    SchedResortWork,
    SchedReceiveChunk,
    SchedSortQueue,
    SchedCheckWaiting,
    SchedGcUnload,
    SchedSave,
    SendPlayerTick,
    SendEncodeChunk,
}

const METRIC_COUNT: usize = Metric::SendEncodeChunk as usize + 1;

const NAMES: [&str; METRIC_COUNT] = [
    "gen: structure_starts",
    "gen: structure_references",
    "gen: biomes",
    "gen: noise",
    "gen: surface",
    "gen: carvers",
    "gen: features",
    "gen: lighting",
    "gen: spawn",
    "gen: full (upgrade)",
    "gen: queue wait (dispatch -> worker)",
    "io: read batch",
    "io: read chunk (decode)",
    "io: write batch",
    "sched: loop iteration",
    "sched: resort_work",
    "sched: receive_chunk",
    "sched: sort_queue",
    "sched: check_waiting_tasks",
    "sched: gc + unload",
    "sched: save",
    "send: player chunk tick",
    "send: encode chunk",
];

struct Slot {
    count: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl Slot {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }
}

static SLOTS: [Slot; METRIC_COUNT] = [const { Slot::new() }; METRIC_COUNT];

// recording: gauges sampled once per second
pub static QUEUE_LEN: AtomicU64 = AtomicU64::new(0);
pub static RUNNING_TASKS: AtomicU64 = AtomicU64::new(0);
pub static MAX_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
pub static GEN_THREADS: AtomicU64 = AtomicU64::new(0);
pub static LOADED_HOLDERS: AtomicU64 = AtomicU64::new(0);
pub static CHUNKS_FULL: AtomicU64 = AtomicU64::new(0);
pub static CHUNKS_SENT: AtomicU64 = AtomicU64::new(0);
pub static SEND_BYTES: AtomicU64 = AtomicU64::new(0);
pub static SEND_PENDING: AtomicU64 = AtomicU64::new(0);

struct Recording {
    started: Instant,
    last_sample: Instant,
    last_full: u64,
    last_sent: u64,
    timeline: Vec<String>,
}

static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

#[inline]
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

#[inline]
pub fn add(metric: Metric, start: Instant) {
    let ns = start.elapsed().as_nanos() as u64;
    let slot = &SLOTS[metric as usize];
    slot.count.fetch_add(1, Relaxed);
    slot.total_ns.fetch_add(ns, Relaxed);
    slot.max_ns.fetch_max(ns, Relaxed);
}

/// Starts a timer when recording, `None` otherwise.
#[inline]
#[must_use]
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

#[inline]
pub fn end(metric: Metric, start: Option<Instant>) {
    if let Some(start) = start {
        add(metric, start);
    }
}

#[inline]
pub fn count(counter: &AtomicU64, n: u64) {
    if enabled() {
        counter.fetch_add(n, Relaxed);
    }
}

#[must_use]
pub const fn stage_metric(stage: StagedChunkEnum) -> Metric {
    match stage {
        StagedChunkEnum::StructureStart => Metric::GenStructureStart,
        StagedChunkEnum::StructureReferences => Metric::GenStructureReferences,
        StagedChunkEnum::Noise => Metric::GenNoise,
        StagedChunkEnum::Surface => Metric::GenSurface,
        StagedChunkEnum::Carvers => Metric::GenCarvers,
        StagedChunkEnum::Features => Metric::GenFeatures,
        StagedChunkEnum::Lighting => Metric::GenLighting,
        StagedChunkEnum::Spawn => Metric::GenSpawn,
        StagedChunkEnum::Full => Metric::GenFull,
        StagedChunkEnum::None | StagedChunkEnum::Empty | StagedChunkEnum::Biomes => {
            Metric::GenBiomes
        }
    }
}

/// Returns false when a recording is already running.
pub fn begin() -> bool {
    let mut rec = RECORDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if rec.is_some() {
        return false;
    }
    for slot in &SLOTS {
        slot.count.store(0, Relaxed);
        slot.total_ns.store(0, Relaxed);
        slot.max_ns.store(0, Relaxed);
    }
    for c in [&CHUNKS_FULL, &CHUNKS_SENT, &SEND_BYTES] {
        c.store(0, Relaxed);
    }
    let now = Instant::now();
    *rec = Some(Recording {
        started: now,
        last_sample: now,
        last_full: 0,
        last_sent: 0,
        timeline: Vec::new(),
    });
    ENABLED.store(true, Relaxed);
    true
}

/// Called from the scheduler loop; appends one timeline row per second.
pub fn sample() {
    if !enabled() {
        return;
    }
    let Ok(mut rec) = RECORDING.try_lock() else {
        return;
    };
    let Some(rec) = rec.as_mut() else {
        return;
    };
    if rec.last_sample.elapsed().as_millis() < 1000 {
        return;
    }
    rec.last_sample = Instant::now();
    let full = CHUNKS_FULL.load(Relaxed);
    let sent = CHUNKS_SENT.load(Relaxed);
    rec.timeline.push(format!(
        "{:>5.1}s  full/s {:>5}  sent/s {:>5}  queue {:>6}  running {:>3}/{:<3}  holders {:>6}  send_pending {:>6}",
        rec.started.elapsed().as_secs_f32(),
        full - rec.last_full,
        sent - rec.last_sent,
        QUEUE_LEN.load(Relaxed),
        RUNNING_TASKS.load(Relaxed),
        MAX_IN_FLIGHT.load(Relaxed),
        LOADED_HOLDERS.load(Relaxed),
        SEND_PENDING.load(Relaxed),
    ));
    rec.last_full = full;
    rec.last_sent = sent;
}

/// Stops the recording and returns the report, `None` when none was running.
#[must_use]
pub fn finish() -> Option<String> {
    let rec = RECORDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()?;
    ENABLED.store(false, Relaxed);
    let secs = rec.started.elapsed().as_secs_f64().max(0.001);
    let threads = GEN_THREADS.load(Relaxed).max(1);
    let mut out = String::new();
    let _ = writeln!(out, "Pumpkin chunk recording, {secs:.1} s");
    let _ = writeln!(
        out,
        "generation threads {threads}, max in flight {}, cpus {}",
        MAX_IN_FLIGHT.load(Relaxed),
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    );
    let _ = writeln!(
        out,
        "chunks full {} ({:.0}/s), chunks sent {} ({:.0}/s), sent {:.1} MB",
        CHUNKS_FULL.load(Relaxed),
        CHUNKS_FULL.load(Relaxed) as f64 / secs,
        CHUNKS_SENT.load(Relaxed),
        CHUNKS_SENT.load(Relaxed) as f64 / secs,
        SEND_BYTES.load(Relaxed) as f64 / 1_048_576.0
    );
    let _ = writeln!(
        out,
        "\n{:<38} {:>9} {:>11} {:>9} {:>9} {:>8}",
        "metric", "count", "total ms", "avg ms", "max ms", "busy %"
    );
    let mut gen_total = 0u64;
    for (i, slot) in SLOTS.iter().enumerate() {
        let count = slot.count.load(Relaxed);
        if count == 0 {
            continue;
        }
        let total = slot.total_ns.load(Relaxed);
        if i <= Metric::GenFull as usize {
            gen_total += total;
        }
        let total_ms = total as f64 / 1e6;
        let _ = writeln!(
            out,
            "{:<38} {:>9} {:>11.1} {:>9.3} {:>9.2} {:>7.1}%",
            NAMES[i],
            count,
            total_ms,
            total_ms / count as f64,
            slot.max_ns.load(Relaxed) as f64 / 1e6,
            total_ms / (secs * 1000.0) * 100.0
        );
    }
    let _ = writeln!(
        out,
        "\ngeneration worker utilisation {:.1}% of {threads} threads",
        gen_total as f64 / 1e9 / (secs * threads as f64) * 100.0
    );
    let _ = writeln!(out, "\ntimeline:");
    for row in &rec.timeline {
        let _ = writeln!(out, "{row}");
    }
    Some(out)
}

/// Records the elapsed time of the enclosing scope on drop.
pub struct Scope(Metric, Option<Instant>);

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        end(self.0, self.1);
    }
}

#[inline]
#[must_use]
pub fn scope(metric: Metric) -> Scope {
    Scope(metric, start())
}
