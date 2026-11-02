use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::sound::Sound;
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;

use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::mob::Mob;
use crate::entity::mob::evoker::IllagerSpell;
use crate::entity::{Entity, EntityBase};

const SPELL_TICKS_NBT: &str = "SpellTicks";
const DEFAULT_CAST_WARMUP: i32 = 20;

// spellcaster illager: spell state (SpellcasterIllager fields)
#[derive(Default)]
pub struct SpellcasterData {
    casting_ticks: AtomicI32,
    current_spell: AtomicU8,
}

impl SpellcasterData {
    pub fn is_casting(&self) -> bool {
        self.casting_ticks.load(Ordering::Relaxed) > 0
    }

    pub fn casting_ticks(&self) -> i32 {
        self.casting_ticks.load(Ordering::Relaxed)
    }

    pub fn set_spell(&self, entity: &Entity, spell: IllagerSpell) {
        self.current_spell.store(spell as u8, Ordering::Relaxed);
        entity.set_synced_data(
            tracked_data::spellcaster_illager::DATA_SPELL_CASTING_ID,
            spell as u8 as i8,
        );
    }

    // spellcaster: countdown (SpellcasterIllager.customServerAiStep)
    pub fn tick(&self) {
        if self.casting_ticks.load(Ordering::Relaxed) > 0 {
            self.casting_ticks.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int(SPELL_TICKS_NBT, self.casting_ticks());
    }

    pub fn read_nbt(&self, nbt: &NbtCompound) {
        self.casting_ticks
            .store(nbt.get_int(SPELL_TICKS_NBT).unwrap_or(0), Ordering::Relaxed);
    }
}

pub trait SpellcasterIllager: Mob {
    fn spellcaster_data(&self) -> &SpellcasterData;
    fn casting_sound(&self) -> Sound;

    // evoker: look at the wololo sheep while casting without a target (Evoker.EvokerCastingSpellGoal)
    fn casting_look_target(&self) -> Option<Arc<dyn EntityBase>> {
        None
    }
}

// spellcaster: stand still while casting (SpellcasterCastingSpellGoal)
pub struct SpellcasterCastingSpellGoal<T> {
    caster: Weak<T>,
}

impl<T> SpellcasterCastingSpellGoal<T> {
    #[must_use]
    pub const fn new(caster: Weak<T>) -> Self {
        Self { caster }
    }
}

impl<T: SpellcasterIllager + 'static> Goal for SpellcasterCastingSpellGoal<T> {
    fn can_start(&mut self, _mob: &dyn Mob) -> bool {
        self.caster
            .upgrade()
            .is_some_and(|caster| caster.spellcaster_data().casting_ticks() > 0)
    }

    fn start(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().steered().navigator().stop();
    }

    fn stop(&mut self, mob: &dyn Mob) {
        if let Some(caster) = self.caster.upgrade() {
            caster
                .spellcaster_data()
                .set_spell(mob.get_entity(), IllagerSpell::None);
        }
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let mob_entity = mob.get_mob_entity();
        let target = mob_entity.get_target().or_else(|| {
            self.caster
                .upgrade()
                .and_then(|caster| caster.casting_look_target())
        });
        if let Some(target) = target {
            mob_entity
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at_entity(mob, &target);
        }
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

// spellcaster: one spell's timing + effect (SpellcasterUseSpellGoal subclasses)
pub trait Spell: Send + Sync {
    fn casting_time(&self) -> i32;
    fn casting_interval(&self) -> i32;
    fn prepare_sound(&self) -> Option<Sound>;
    fn spell(&self) -> IllagerSpell;
    fn can_use(&mut self, _mob: &dyn Mob) -> bool {
        true
    }
    fn on_start(&mut self, _mob: &dyn Mob) {}
    fn on_stop(&mut self, _mob: &dyn Mob) {}
    // wololo: overrides the target check of SpellcasterUseSpellGoal
    fn requires_target(&self) -> bool {
        true
    }
    fn cast_warmup(&self) -> i32 {
        DEFAULT_CAST_WARMUP
    }
    fn can_continue(&mut self, _mob: &dyn Mob) -> bool {
        true
    }
    fn perform(&mut self, mob: &dyn Mob);
}

// spellcaster: warmup -> cast (SpellcasterUseSpellGoal)
pub struct SpellcasterUseSpellGoal<T, S> {
    caster: Weak<T>,
    spell: S,
    warmup_delay: i32,
    next_attack_tick: i32,
}

impl<T, S> SpellcasterUseSpellGoal<T, S> {
    #[must_use]
    pub const fn new(caster: Weak<T>, spell: S) -> Self {
        Self {
            caster,
            spell,
            warmup_delay: 0,
            next_attack_tick: 0,
        }
    }
}

fn target_alive(mob: &dyn Mob) -> bool {
    mob.get_mob_entity().get_target().is_some_and(|target| {
        target
            .get_living_entity()
            .is_some_and(crate::entity::living::LivingEntity::is_alive)
    })
}

impl<T: SpellcasterIllager + 'static, S: Spell> Goal for SpellcasterUseSpellGoal<T, S> {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(caster) = self.caster.upgrade() else {
            return false;
        };
        (!self.spell.requires_target() || target_alive(mob))
            && !caster.spellcaster_data().is_casting()
            && mob.get_entity().tick_count.load(Ordering::Relaxed) >= self.next_attack_tick
            && self.spell.can_use(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        if self.spell.requires_target() {
            target_alive(mob) && self.warmup_delay > 0
        } else {
            self.warmup_delay > 0 && self.spell.can_continue(mob)
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.spell.on_stop(mob);
    }

    fn start(&mut self, mob: &dyn Mob) {
        let Some(caster) = self.caster.upgrade() else {
            return;
        };
        let data = caster.spellcaster_data();
        self.warmup_delay = self.get_tick_count(self.spell.cast_warmup());
        data.casting_ticks
            .store(self.spell.casting_time(), Ordering::Relaxed);
        self.next_attack_tick =
            mob.get_entity().tick_count.load(Ordering::Relaxed) + self.spell.casting_interval();
        if let Some(sound) = self.spell.prepare_sound() {
            mob.play_entity_sound(sound, 1.0, 1.0);
        }
        data.set_spell(mob.get_entity(), self.spell.spell());
        self.spell.on_start(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        self.warmup_delay -= 1;
        if self.warmup_delay == 0 {
            self.spell.perform(mob);
            if let Some(caster) = self.caster.upgrade() {
                let caster_base: &dyn EntityBase = caster.as_ref();
                caster_base.play_entity_sound(caster.casting_sound(), 1.0, 1.0);
            }
        }
    }
}
