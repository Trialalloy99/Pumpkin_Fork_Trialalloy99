use std::sync::Arc;

use super::{Controls, Goal, GoalReach, to_goal_ticks};
use crate::entity::EntityBase;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::{ai::pathfinder::NavigatorGoal, mob::Mob, player::Player};
use pumpkin_data::attributes::Attributes;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

const DEFAULT_STOP_DISTANCE: f64 = 2.5;
/// Following stops once the player moves further than this while being watched.
const SCARE_RANGE_SQ: f64 = 36.0;
const SCARE_MOVE_SQ: f64 = 0.010_000_000_000_000_002;
const SCARE_ROTATION: f32 = 5.0;

pub struct TemptGoal {
    goal_control: Controls,
    speed: f64,
    tempt_items: &'static [&'static Item],
    predicate: Option<fn(&dyn Mob, &ItemStack) -> bool>,
    non_pathfinder: bool,
    can_scare: bool,
    // cat: CatTemptGoal.canUse, extra gate on top of the tempting player
    use_gate: Option<fn(&dyn Mob) -> bool>,
    // ocelot: OcelotTemptGoal.canScare, extra gate on top of the scare flag
    scare_gate: Option<fn(&dyn Mob) -> bool>,
    // cat: CatTemptGoal keeps one picked player that never scares the cat
    picks_unscary_player: bool,
    selected_player: Option<i32>,
    stop_distance: f64,
    target_player: Option<Arc<Player>>,
    watched_pos: Vector3<f64>,
    watched_yaw: f32,
    watched_pitch: f32,
    cooldown: i32,
    running: bool,
}

impl TemptGoal {
    #[must_use]
    pub fn new(speed: f64, tempt_items: &'static [&'static Item], can_scare: bool) -> Self {
        Self::with_stop_distance(speed, tempt_items, can_scare, DEFAULT_STOP_DISTANCE)
    }

    #[must_use]
    pub fn with_stop_distance(
        speed: f64,
        tempt_items: &'static [&'static Item],
        can_scare: bool,
        stop_distance: f64,
    ) -> Self {
        Self {
            goal_control: Controls::MOVE | Controls::LOOK,
            speed,
            tempt_items,
            predicate: None,
            non_pathfinder: false,
            can_scare,
            use_gate: None,
            scare_gate: None,
            picks_unscary_player: false,
            selected_player: None,
            stop_distance,
            target_player: None,
            watched_pos: Vector3::new(0.0, 0.0, 0.0),
            watched_yaw: 0.0,
            watched_pitch: 0.0,
            cooldown: 0,
            running: false,
        }
    }

    /// Vanilla `TemptGoal.ForNonPathfinders`: steers through the move control, tempt items by predicate.
    #[must_use]
    pub const fn for_non_pathfinders(
        mut self,
        predicate: fn(&dyn Mob, &ItemStack) -> bool,
    ) -> Self {
        self.predicate = Some(predicate);
        self.non_pathfinder = true;
        self
    }

    /// Vanilla `CatTemptGoal`: only while the gate holds, with a picked player that never scares.
    #[must_use]
    pub const fn for_cat(mut self, use_gate: fn(&dyn Mob) -> bool) -> Self {
        self.use_gate = Some(use_gate);
        self.picks_unscary_player = true;
        self
    }

    /// Vanilla `OcelotTemptGoal`: scares only while the gate holds.
    #[must_use]
    pub const fn with_scare_gate(mut self, gate: fn(&dyn Mob) -> bool) -> Self {
        self.scare_gate = Some(gate);
        self
    }

    // tempt: canScare with the species overrides
    fn can_scare(&self, mob: &dyn Mob, player: &Player) -> bool {
        if self.picks_unscary_player && self.selected_player == Some(player.entity_id()) {
            return false;
        }
        self.can_scare && self.scare_gate.is_none_or(|gate| gate(mob))
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    fn is_tempt_item(&self, mob: &dyn Mob, stack: &ItemStack) -> bool {
        if let Some(predicate) = self.predicate {
            return predicate(mob, stack);
        }
        stack.item_count > 0 && self.tempt_items.iter().any(|i| i.id == stack.item.id)
    }

    fn is_holding_tempt_item(&self, mob: &dyn Mob, player: &Player) -> bool {
        self.is_tempt_item(mob, &player.inventory().held_item())
            || self.is_tempt_item(mob, &player.inventory().off_hand_item())
    }

    /// Non-combat, line of sight ignored, ranged by `tempt_range` and filtered on the held item.
    fn find_tempting_player(&self, mob: &dyn Mob) -> Option<Arc<Player>> {
        let mob_entity = mob.get_mob_entity();
        let range = mob_entity
            .living_entity
            .get_attribute_value(&Attributes::TEMPT_RANGE);
        let world = mob_entity.living_entity.entity.world.load();
        let conditions = TargetPredicate::create_non_attackable()
            .ignore_visibility()
            .set_base_max_distance(range);

        // The conditions scale the range down, so sneaking shortens it.
        world.get_nearest_player(
            mob_entity.living_entity.entity.pos.load(),
            range,
            |player| {
                self.is_holding_tempt_item(mob, player)
                    && conditions.test(&world, Some(mob), player.as_ref())
            },
        )
    }

    fn watch(&mut self, player: &Player) {
        let entity = player.get_entity();
        self.watched_pos = entity.pos.load();
        self.watched_yaw = entity.yaw.load();
        self.watched_pitch = entity.pitch.load();
    }
}

impl Goal for TemptGoal {
    fn tick_reach(&self, mob: &dyn Mob, running: bool) -> Option<GoalReach> {
        let mob_entity = mob.get_mob_entity();
        let range = mob_entity
            .living_entity
            .get_attribute_value(&Attributes::TEMPT_RANGE);
        let tempted = running
            || !mob_entity
                .living_entity
                .entity
                .world
                .load()
                .get_nearby_players(mob_entity.living_entity.entity.pos.load(), range)
                .is_empty();
        Some(GoalReach {
            players: range,
            speed: if tempted { self.speed } else { 0.0 },
            links: self
                .target_player
                .iter()
                .map(|player| player.entity_id())
                .collect(),
            ..GoalReach::default()
        })
    }

    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        self.target_player = self.find_tempting_player(mob);
        self.target_player.is_some() && self.use_gate.is_none_or(|gate| gate(mob))
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let scares = self
            .target_player
            .as_ref()
            .is_some_and(|player| self.can_scare(mob, player));
        if scares {
            let Some(player) = self.target_player.clone() else {
                return false;
            };
            let entity = player.get_entity();
            let player_pos = entity.pos.load();
            let mob_pos = mob.get_mob_entity().living_entity.entity.pos.load();

            if mob_pos.squared_distance_to_vec(&player_pos) < SCARE_RANGE_SQ {
                if player_pos.squared_distance_to_vec(&self.watched_pos) > SCARE_MOVE_SQ {
                    return false;
                }
                if (entity.pitch.load() - self.watched_pitch).abs() > SCARE_ROTATION
                    || (entity.yaw.load() - self.watched_yaw).abs() > SCARE_ROTATION
                {
                    return false;
                }
            } else {
                self.watched_pos = player_pos;
            }

            self.watched_yaw = entity.yaw.load();
            self.watched_pitch = entity.pitch.load();
        }

        self.can_start(mob)
    }

    fn start(&mut self, _mob: &dyn Mob) {
        if let Some(player) = self.target_player.clone() {
            self.watch(&player);
        }
        self.running = true;
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(player) = self.target_player.clone() else {
            return;
        };
        // cat: CatTemptGoal.tick picks and drops the player that does not scare
        if self.picks_unscary_player {
            let mut rng = mob.get_random();
            if self.selected_player.is_none() && rng.random_range(0..to_goal_ticks(600)) == 0 {
                self.selected_player = Some(player.entity_id());
            } else if rng.random_range(0..to_goal_ticks(500)) == 0 {
                self.selected_player = None;
            }
        }
        let mob_entity = mob.get_mob_entity();
        let player_pos = player.get_entity().pos.load();

        mob_entity
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .look_at_with_range(
                player_pos.x,
                player.get_entity().get_eye_y(),
                player_pos.z,
                mob.get_max_head_rotation() + 20.0,
                mob.get_max_look_pitch_change(),
            );

        let mob_pos = mob_entity.living_entity.entity.pos.load();
        let close =
            mob_pos.squared_distance_to_vec(&player_pos) < self.stop_distance * self.stop_distance;
        if self.non_pathfinder {
            let mut move_control = mob_entity
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if close {
                move_control.set_wait();
            } else {
                let eye = player.get_entity().get_eye_pos();
                let scale: f64 = mob.get_random().random();
                move_control.set_wanted_position(
                    mob_pos.x + (eye.x - mob_pos.x) * scale,
                    mob_pos.y + (eye.y - mob_pos.y) * scale,
                    mob_pos.z + (eye.z - mob_pos.z) * scale,
                    self.speed,
                );
            }
            return;
        }
        let steered = mob_entity.steered();
        let mut navigator = steered.navigator();
        if close {
            navigator.stop();
        } else {
            navigator.set_progress(NavigatorGoal::new(mob_pos, player_pos, self.speed));
        }
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.target_player = None;
        if self.non_pathfinder {
            mob.get_mob_entity()
                .move_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_wait();
        } else {
            mob.get_mob_entity().steered().navigator().stop();
        }
        self.cooldown = to_goal_ticks(100);
        self.running = false;
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}
