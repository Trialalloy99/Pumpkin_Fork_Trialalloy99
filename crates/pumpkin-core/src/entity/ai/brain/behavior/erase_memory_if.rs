use super::super::BrainTick;
use super::super::memory::{MemoryModuleType, MemoryStatus, MemoryValue};
use super::OneShot;

// brain: EraseMemoryIf.create
#[must_use]
pub fn create<T: MemoryValue>(
    predicate: fn(&BrainTick<'_>) -> bool,
    memory: MemoryModuleType<T>,
) -> OneShot {
    OneShot::new(
        "EraseMemoryIf",
        vec![(memory.id(), MemoryStatus::ValuePresent)],
        move |tick| {
            if predicate(tick) {
                tick.state.erase(memory.id());
                true
            } else {
                false
            }
        },
    )
}
