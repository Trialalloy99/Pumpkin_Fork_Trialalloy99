use crate::entity::ai::control::Control;
use crate::entity::mob::Mob;
use std::sync::atomic::Ordering;

#[derive(Default)]
pub struct JumpControl {
    jump: bool,
}

impl Control for JumpControl {}

impl JumpControl {
    pub const fn jump(&mut self) {
        self.jump = true;
    }

    // jump: pending jump request (RabbitJumpControl.wantJump)
    #[must_use]
    pub const fn wants_jump(&self) -> bool {
        self.jump
    }

    pub fn tick(&mut self, mob: &dyn Mob) {
        if mob.jump_control_tick(self.jump) {
            self.jump = false;
            return;
        }
        mob.get_mob_entity()
            .living_entity
            .jumping
            .store(self.jump, Ordering::SeqCst);
        self.jump = false;
    }
}
