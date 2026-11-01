use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_util::Hand;
use rand::RngExt;
use std::sync::Arc;

use crate::entity::ai::goal::{Controls, Goal};
use crate::entity::ai::pathfinder::NavigatorGoal;
use crate::entity::mob::Mob;
use crate::entity::projectile::arrow::{ArrowEntity, ArrowPickup};
use crate::entity::{Entity, EntityBase};

/// Ranged bow attack used by skeletons and their variants.
/// Mirrors vanilla `RangedBowAttackGoal`: the mob keeps its distance, draws the bow
/// and releases an arrow once it has been drawn long enough.
pub struct BowAttackGoal {
    goal_control: Controls,
    speed: f64,
    attack_interval: i32,
    squared_range: f64,
    attack_time: i32,
    see_time: i32,
    strafing_clockwise: bool,
    strafing_backwards: bool,
    strafing_time: i32,
    draw_ticks: i32,
    drawing: bool,
    /// Interval comes from the mob by difficulty (AbstractSkeleton.reassessWeaponGoal).
    per_difficulty: bool,
    last_path_target: Option<pumpkin_util::math::position::BlockPos>,
}

impl BowAttackGoal {
    /// Ticks the bow has to be drawn before the arrow is released.
    const DRAW_TIME: i32 = 20;
    /// Vanilla arrow speed for mob shots.
    const ARROW_SPEED: f64 = 1.6;

    #[must_use]
    pub fn new(speed: f64, attack_interval: i32, range: f32) -> Self {
        Self {
            goal_control: Controls::MOVE | Controls::LOOK,
            speed,
            attack_interval,
            squared_range: f64::from(range * range),
            attack_time: -1,
            see_time: 0,
            strafing_clockwise: false,
            strafing_backwards: false,
            strafing_time: -1,
            draw_ticks: 0,
            drawing: false,
            per_difficulty: false,
            last_path_target: None,
        }
    }

    // skeleton bow: interval by type + difficulty (AbstractSkeleton.reassessWeaponGoal)
    #[must_use]
    pub const fn with_difficulty_interval(mut self) -> Self {
        self.per_difficulty = true;
        self
    }

    fn min_attack_interval(&self, mob: &dyn Mob) -> i32 {
        if !self.per_difficulty {
            return self.attack_interval;
        }
        let (hard, normal) = mob.bow_attack_intervals();
        if mob.get_entity().world.load().level_info.load().difficulty
            == pumpkin_util::Difficulty::Hard
        {
            hard
        } else {
            normal
        }
    }

    // bow: hand holding the bow, main first (ProjectileUtil.getWeaponHoldingHand)
    fn bow_hand(mob: &dyn Mob) -> Option<Hand> {
        let equipment = mob
            .get_mob_entity()
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if equipment
            .equipment
            .get(&EquipmentSlot::MAIN_HAND)
            .is_some_and(|stack| stack.item.id == Item::BOW.id)
        {
            Some(Hand::Right)
        } else if equipment
            .equipment
            .get(&EquipmentSlot::OFF_HAND)
            .is_some_and(|stack| stack.item.id == Item::BOW.id)
        {
            Some(Hand::Left)
        } else {
            None
        }
    }

    fn bow_item(mob: &dyn Mob) -> ItemStack {
        let slot = if Self::bow_hand(mob) == Some(Hand::Left) {
            EquipmentSlot::OFF_HAND
        } else {
            EquipmentSlot::MAIN_HAND
        };
        mob.get_mob_entity()
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&slot)
    }

    // bow: either hand (Mob.isHolding(Items.BOW))
    #[must_use]
    pub fn is_holding_bow(mob: &dyn Mob) -> bool {
        Self::bow_hand(mob).is_some()
    }

    fn stop_drawing(&mut self, mob: &dyn Mob) {
        if self.drawing {
            mob.get_mob_entity().living_entity.clear_active_hand();
            self.drawing = false;
            self.draw_ticks = 0;
        }
    }

    /// Spawns the arrow, matching vanilla `AbstractSkeleton::performRangedAttack`.
    fn shoot(mob: &dyn Mob, target: &Arc<dyn EntityBase>) {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let world_full = entity.world.load_full();

        let arrow_entity = Entity::new(world.clone(), entity.pos.load(), &EntityType::ARROW);
        let projectile = ItemStack::new(1, &Item::ARROW);
        let bow_item = Self::bow_item(mob);
        let arrow = ArrowEntity::new_shot_with_weapon(
            arrow_entity,
            entity,
            &projectile,
            &bow_item,
            ArrowPickup::Disallowed,
        );

        // Vanilla scales base damage with power and world difficulty
        let difficulty = world.level_info.load().difficulty as i32;
        arrow.set_base_damage_from_mob(Self::ARROW_SPEED, difficulty);
        let arrow = mob.get_arrow(arrow);

        arrow.set_base_damage(crate::enchantment::EnchantmentHelper::modify_damage(
            &bow_item,
            arrow.get_base_damage(),
        ));
        arrow.punch_level.store(
            crate::enchantment::EnchantmentHelper::modify_knockback(&bow_item, 0.0) as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        arrow.apply_on_projectile_spawned(&projectile);
        if entity.is_on_fire() {
            arrow.set_flame(true);
        }

        let mob_pos = entity.pos.load();
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();

        let dx = target_pos.x - mob_pos.x;
        let dy = (target_pos.y + f64::from(target_entity.entity_dimension.load().height) / 3.0)
            - arrow.entity.pos.load().y;
        let dz = target_pos.z - mob_pos.z;
        let horizontal_distance = dx.hypot(dz);

        // Vanilla scales the spread with the world difficulty: 14 - difficulty * 4.
        let divergence = f64::from(14 - difficulty * 4);

        arrow.set_velocity(
            dx,
            horizontal_distance.mul_add(0.2, dy),
            dz,
            Self::ARROW_SPEED,
            divergence,
        );

        // bow: skeleton shoot sound on the mob (AbstractSkeleton.performRangedAttack)
        let pitch = 1.0 / mob.get_random().random::<f32>().mul_add(0.4, 0.8);
        mob.play_entity_sound(Sound::EntitySkeletonShoot, 1.0, pitch);

        let arrow: Arc<dyn EntityBase> = Arc::new(arrow);
        let entity_id = entity.entity_id;
        if let Some(server) = world_full.server.upgrade() {
            let mut event =
                crate::plugin::api::events::entity::entity_shoot_bow::EntityShootBowEvent::new(
                    entity_id,
                    "minecraft:bow".to_string(),
                    1.0,
                );
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return;
            }
        }
        world_full.spawn_entity(arrow);
    }
}

impl Goal for BowAttackGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        mob.get_mob_entity().get_target().is_some() && Self::is_holding_bow(mob)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        let navigating = !mob.is_navigator_idle();
        (self.can_start(mob) || navigating) && Self::is_holding_bow(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().set_attacking(true);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity().set_attacking(false);
        self.see_time = 0;
        self.attack_time = -1;
        self.stop_drawing(mob);
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let target = mob.get_mob_entity().get_target().clone();
        let Some(target) = target else {
            return;
        };

        let mob_pos = mob.get_entity().pos.load();
        let target_pos = target.get_entity().pos.load();
        let distance_sq = mob_pos.squared_distance_to_vec(&target_pos);

        let has_line_of_sight = mob.has_line_of_sight(target.get_entity());
        if has_line_of_sight != (self.see_time > 0) {
            self.see_time = 0;
        }
        if has_line_of_sight {
            self.see_time += 1;
        } else {
            self.see_time -= 1;
        }

        {
            let steered = mob.get_mob_entity().steered();
            let mut navigator = steered.navigator();
            if distance_sq <= self.squared_range && self.see_time >= 20 {
                navigator.stop();
                self.last_path_target = None;
                self.strafing_time += 1;
            } else {
                // bow: repath only on a new target block (PathNavigation.createPath cache)
                let target_block = target.get_entity().block_pos.load();
                if self.last_path_target != Some(target_block) || navigator.is_idle() {
                    self.last_path_target = Some(target_block);
                    navigator.set_progress(NavigatorGoal {
                        current_progress: mob_pos,
                        destination: target_pos,
                        speed: self.speed,
                    });
                } else {
                    navigator.set_speed(self.speed);
                }
                self.strafing_time = -1;
            }
        }

        if self.strafing_time >= 20 {
            if mob.get_random().random::<f32>() < 0.3 {
                self.strafing_clockwise = !self.strafing_clockwise;
            }
            if mob.get_random().random::<f32>() < 0.3 {
                self.strafing_backwards = !self.strafing_backwards;
            }
            self.strafing_time = 0;
        }

        if self.strafing_time > -1 {
            if distance_sq > self.squared_range * 0.75 {
                self.strafing_backwards = false;
            } else if distance_sq < self.squared_range * 0.25 {
                self.strafing_backwards = true;
            }
            mob.get_mob_entity().steered().move_control().strafe(
                if self.strafing_backwards { -0.5 } else { 0.5 },
                if self.strafing_clockwise { 0.5 } else { -0.5 },
            );
            // bow: strafing turns the body (and a ridden mob) straight to the target (Mob.lookAt)
            if let Some(vehicle) = mob.get_entity().get_vehicle()
                && let Some(vehicle_mob) = vehicle.get_mob()
            {
                vehicle_mob
                    .get_mob_entity()
                    .look_at_entity_body(target.as_ref(), 30.0, 30.0);
            }
            mob.get_mob_entity()
                .look_at_entity_body(target.as_ref(), 30.0, 30.0);
        } else {
            mob.get_mob_entity()
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at_entity_with_range(&target, 30.0, 30.0);
        }

        if self.drawing {
            if !has_line_of_sight && self.see_time < -60 {
                self.stop_drawing(mob);
            } else if has_line_of_sight {
                self.draw_ticks += 1;
                if self.draw_ticks >= Self::DRAW_TIME {
                    self.stop_drawing(mob);
                    Self::shoot(mob, &target);
                    self.attack_time = self.min_attack_interval(mob);
                }
            }
        } else {
            self.attack_time -= 1;
            if self.attack_time <= 0
                && self.see_time >= -60
                && let Some(hand) = Self::bow_hand(mob)
            {
                let stack = Self::bow_item(mob);
                mob.get_mob_entity()
                    .living_entity
                    .set_active_hand(hand, stack, i32::MAX);
                self.drawing = true;
                self.draw_ticks = 0;
            }
        }
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}
