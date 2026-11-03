//! Ender dragon phases, ported from `net.minecraft.world.entity.boss.enderdragon.phases`.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pumpkin_data::entity::EntityType;
use pumpkin_data::world::WorldEvent;
use pumpkin_util::math::vector3::Vector3;

use super::{DragonPath, EnderDragonEntity, Node};
use crate::entity::player::Player;
use crate::entity::projectile::dragon_fireball::DragonFireballEntity;
use crate::entity::{Entity, EntityBase};
use crate::world::World;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Vanilla `EnderDragonPhase`, ids are the ones synced to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EnderDragonPhase {
    HoldingPattern = 0,
    StrafePlayer = 1,
    LandingApproach = 2,
    Landing = 3,
    Takeoff = 4,
    SittingFlaming = 5,
    SittingScanning = 6,
    SittingAttacking = 7,
    ChargingPlayer = 8,
    Dying = 9,
    #[default]
    Hovering = 10,
}

impl EnderDragonPhase {
    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    /// Vanilla `EnderDragonPhase.getById`, unknown ids give the holding pattern.
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::StrafePlayer,
            2 => Self::LandingApproach,
            3 => Self::Landing,
            4 => Self::Takeoff,
            5 => Self::SittingFlaming,
            6 => Self::SittingScanning,
            7 => Self::SittingAttacking,
            8 => Self::ChargingPlayer,
            9 => Self::Dying,
            10 => Self::Hovering,
            _ => Self::HoldingPattern,
        }
    }
}

/// Vanilla `DragonPhaseInstance`.
pub trait DragonPhase: Send + Sync {
    fn phase(&self) -> EnderDragonPhase;

    fn is_sitting(&self) -> bool {
        false
    }

    fn server_tick(&self, _dragon: &EnderDragonEntity, _world: &Arc<World>) {}

    fn on_crystal_destroyed(&self, _dragon: &EnderDragonEntity, _player: Option<Arc<Player>>) {}

    fn begin(&self, _dragon: &EnderDragonEntity) {}

    fn end(&self, _dragon: &EnderDragonEntity) {}

    fn fly_speed(&self) -> f32 {
        0.6
    }

    fn turn_speed(&self, dragon: &EnderDragonEntity) -> f32 {
        let rot_speed = dragon.delta_movement().horizontal_length() as f32 + 1.0;
        let dist = rot_speed.min(40.0);
        0.7 / dist / rot_speed
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        None
    }

    /// `source` is the direct entity of the hit, the damage is changed in place.
    fn on_hurt(&self, _source: Option<&dyn EntityBase>, damage: f32) -> f32 {
        damage
    }
}

/// Vanilla `EnderDragonPhaseManager`.
pub struct PhaseManager {
    pub holding_pattern: Arc<HoldingPatternPhase>,
    pub strafe_player: Arc<StrafePlayerPhase>,
    pub landing_approach: Arc<LandingApproachPhase>,
    pub landing: Arc<LandingPhase>,
    pub takeoff: Arc<TakeoffPhase>,
    pub sitting_flaming: Arc<SittingFlamingPhase>,
    pub sitting_scanning: Arc<SittingScanningPhase>,
    pub sitting_attacking: Arc<SittingAttackingPhase>,
    pub charging_player: Arc<ChargePlayerPhase>,
    pub dying: Arc<DeathPhase>,
    pub hovering: Arc<HoverPhase>,
    current: AtomicU8,
}

impl Default for PhaseManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PhaseManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            holding_pattern: Arc::new(HoldingPatternPhase::default()),
            strafe_player: Arc::new(StrafePlayerPhase::default()),
            landing_approach: Arc::new(LandingApproachPhase::default()),
            landing: Arc::new(LandingPhase::default()),
            takeoff: Arc::new(TakeoffPhase::default()),
            sitting_flaming: Arc::new(SittingFlamingPhase::default()),
            sitting_scanning: Arc::new(SittingScanningPhase::default()),
            sitting_attacking: Arc::new(SittingAttackingPhase::default()),
            charging_player: Arc::new(ChargePlayerPhase::default()),
            dying: Arc::new(DeathPhase::default()),
            hovering: Arc::new(HoverPhase::default()),
            current: AtomicU8::new(EnderDragonPhase::Hovering as u8),
        }
    }

    #[must_use]
    pub fn current_type(&self) -> EnderDragonPhase {
        EnderDragonPhase::from_id(i32::from(self.current.load(Ordering::Acquire)))
    }

    pub(super) fn store_current(&self, phase: EnderDragonPhase) {
        self.current.store(phase as u8, Ordering::Release);
    }

    #[must_use]
    pub fn get(&self, phase: EnderDragonPhase) -> Arc<dyn DragonPhase> {
        match phase {
            EnderDragonPhase::HoldingPattern => self.holding_pattern.clone(),
            EnderDragonPhase::StrafePlayer => self.strafe_player.clone(),
            EnderDragonPhase::LandingApproach => self.landing_approach.clone(),
            EnderDragonPhase::Landing => self.landing.clone(),
            EnderDragonPhase::Takeoff => self.takeoff.clone(),
            EnderDragonPhase::SittingFlaming => self.sitting_flaming.clone(),
            EnderDragonPhase::SittingScanning => self.sitting_scanning.clone(),
            EnderDragonPhase::SittingAttacking => self.sitting_attacking.clone(),
            EnderDragonPhase::ChargingPlayer => self.charging_player.clone(),
            EnderDragonPhase::Dying => self.dying.clone(),
            EnderDragonPhase::Hovering => self.hovering.clone(),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<dyn DragonPhase> {
        self.get(self.current_type())
    }
}

/// `distToTarget < 100 || distToTarget > 22500` of the phases that pick a new target
/// (no target counts as distance 0).
fn target_reached_or_lost(target: Option<Vector3<f64>>, pos: Vector3<f64>) -> bool {
    let dist_sq = target.map_or(0.0, |target| target.squared_distance_to_vec(&pos));
    dist_sq < 100.0 || dist_sq > 22500.0
}

/// Next node of `path` as a target above the node (`navigateToNextPathNode`).
fn target_above(node: Node) -> Vector3<f64> {
    let height = f64::from(rand::random::<f32>() * 20.0);
    Vector3::new(
        f64::from(node.x),
        f64::from(node.y) + height,
        f64::from(node.z),
    )
}

/// Holding pattern and strafe pick the next circle node the same way.
fn next_circle_node(dragon: &EnderDragonEntity, clockwise: &mut bool, crystals_gate: bool) -> i32 {
    let current = dragon.find_closest_node() as i32;
    let mut target = current;
    if rand::random_range(0..8) == 0 {
        *clockwise = !*clockwise;
        target += 6;
    }
    if *clockwise {
        target += 1;
    } else {
        target -= 1;
    }
    if crystals_gate {
        target = target.rem_euclid(12);
    } else {
        target -= 12;
        target &= 7;
        target += 12;
    }
    target
}

// ───────────────────────────── holding pattern ─────────────────────────────

#[derive(Default)]
struct HoldingState {
    path: Option<DragonPath>,
    target: Option<Vector3<f64>>,
    clockwise: bool,
}

#[derive(Default)]
pub struct HoldingPatternPhase {
    state: Mutex<HoldingState>,
}

impl HoldingPatternPhase {
    fn navigate_to_next_node(state: &mut HoldingState) {
        if let Some(path) = state.path.as_mut()
            && !path.is_done()
        {
            let node = path.next_node();
            path.advance();
            state.target = Some(target_above(node));
        }
    }
}

enum HoldingAction {
    None,
    Land,
    Strafe(Arc<Player>),
}

impl DragonPhase for HoldingPatternPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::HoldingPattern
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let mut state = lock(&self.state);
        if !target_reached_or_lost(state.target, dragon.position()) {
            return;
        }
        let mut action = HoldingAction::None;
        if state.path.as_ref().is_some_and(DragonPath::is_done) {
            let egg = dragon.podium_surface(world);
            let crystals = dragon.alive_crystals();
            if rand::random_range(0..(crystals + 3).max(1)) == 0 {
                action = HoldingAction::Land;
            } else {
                let nearest = dragon.nearest_player(
                    f64::from(egg.0.x),
                    f64::from(egg.0.y),
                    f64::from(egg.0.z),
                    None,
                    |_| true,
                );
                let dist_sqr = nearest.as_ref().map_or(64.0, |player| {
                    let p = player.get_entity().pos.load();
                    let dx = p.x - (f64::from(egg.0.x) + 0.5);
                    let dy = p.y - (f64::from(egg.0.y) + 0.5);
                    let dz = p.z - (f64::from(egg.0.z) + 0.5);
                    (dx * dx + dy * dy + dz * dz) / 512.0
                });
                if let Some(player) = nearest
                    && (rand::random_range(0..((dist_sqr + 2.0) as i32).max(1)) == 0
                        || rand::random_range(0..(crystals + 2).max(1)) == 0)
                {
                    action = HoldingAction::Strafe(player);
                }
            }
        }
        match action {
            HoldingAction::Land => {
                drop(state);
                dragon.set_phase(EnderDragonPhase::LandingApproach);
                return;
            }
            HoldingAction::Strafe(player) => {
                drop(state);
                dragon.strafe_player(&player);
                return;
            }
            HoldingAction::None => {}
        }
        if state.path.as_ref().is_none_or(DragonPath::is_done) {
            let gate = dragon.has_fight() && dragon.alive_crystals() >= 0;
            let mut clockwise = state.clockwise;
            let target = next_circle_node(dragon, &mut clockwise, gate);
            state.clockwise = clockwise;
            state.path = dragon.find_path(
                dragon.find_closest_node(),
                target.rem_euclid(24) as usize,
                None,
            );
            if let Some(path) = state.path.as_mut() {
                path.advance();
            }
        }
        Self::navigate_to_next_node(&mut state);
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.path = None;
        state.target = None;
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        lock(&self.state).target
    }

    fn on_crystal_destroyed(&self, dragon: &EnderDragonEntity, player: Option<Arc<Player>>) {
        if let Some(player) = player
            && dragon.can_attack(&player)
        {
            dragon.strafe_player(&player);
        }
    }
}

// ───────────────────────────── strafe player ─────────────────────────────

#[derive(Default)]
struct StrafeState {
    fireball_charge: i32,
    path: Option<DragonPath>,
    target: Option<Vector3<f64>>,
    attack_target: Option<Arc<Player>>,
    clockwise: bool,
}

#[derive(Default)]
pub struct StrafePlayerPhase {
    state: Mutex<StrafeState>,
}

impl StrafePlayerPhase {
    fn navigate_to_next_node(state: &mut StrafeState) {
        if let Some(path) = state.path.as_mut()
            && !path.is_done()
        {
            let node = path.next_node();
            path.advance();
            state.target = Some(target_above(node));
        }
    }

    fn find_new_target(dragon: &EnderDragonEntity, state: &mut StrafeState) {
        if state.path.as_ref().is_none_or(DragonPath::is_done) {
            let gate = dragon.has_fight() && dragon.alive_crystals() > 0;
            let mut clockwise = state.clockwise;
            let target = next_circle_node(dragon, &mut clockwise, gate);
            state.clockwise = clockwise;
            state.path = dragon.find_path(
                dragon.find_closest_node(),
                target.rem_euclid(24) as usize,
                None,
            );
            if let Some(path) = state.path.as_mut() {
                path.advance();
            }
        }
        Self::navigate_to_next_node(state);
    }

    /// Vanilla `setTarget`.
    pub fn set_target(&self, dragon: &EnderDragonEntity, target: &Arc<Player>) {
        let mut state = lock(&self.state);
        state.attack_target = Some(target.clone());
        let pos = target.get_entity().pos.load();
        let current = dragon.find_closest_node();
        let target_index = dragon.find_closest_node_to(pos.x, pos.y, pos.z);
        let final_x = pos.x.floor() as i32;
        let final_z = pos.z.floor() as i32;
        let me = dragon.position();
        let xd = f64::from(final_x) - me.x;
        let zd = f64::from(final_z) - me.z;
        let sd = xd.hypot(zd);
        let height_offset = (f64::from(0.4f32) + sd / 80.0 - 1.0).min(10.0);
        let final_y = (pos.y + height_offset).floor() as i32;
        state.path = dragon.find_path(
            current,
            target_index,
            Some(Node {
                x: final_x,
                y: final_y,
                z: final_z,
            }),
        );
        if let Some(path) = state.path.as_mut() {
            path.advance();
        }
        Self::navigate_to_next_node(&mut state);
    }
}

impl DragonPhase for StrafePlayerPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::StrafePlayer
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let mut state = lock(&self.state);
        let Some(attack_target) = state.attack_target.clone() else {
            drop(state);
            dragon.set_phase(EnderDragonPhase::HoldingPattern);
            return;
        };
        let me = dragon.position();
        let target_pos = attack_target.get_entity().pos.load();
        if state.path.as_ref().is_some_and(DragonPath::is_done) {
            let dist = (target_pos.x - me.x).hypot(target_pos.z - me.z);
            let height_offset = (f64::from(0.4f32) + dist / 80.0 - 1.0).min(10.0);
            state.target = Some(Vector3::new(
                target_pos.x,
                target_pos.y + height_offset,
                target_pos.z,
            ));
        }
        if target_reached_or_lost(state.target, me) {
            Self::find_new_target(dragon, &mut state);
        }

        if target_pos.squared_distance_to_vec(&me) < 4096.0 {
            if dragon.has_line_of_sight_to(&attack_target) {
                state.fireball_charge += 1;
                let aim = Vector3::new(target_pos.x - me.x, 0.0, target_pos.z - me.z).normalize();
                let yaw_rad = f64::from(dragon.yaw()).to_radians();
                let dir = Vector3::new(yaw_rad.sin(), 0.0, -yaw_rad.cos()).normalize();
                let dot = dir.dot(&aim) as f32;
                let angle = dot.acos().to_degrees() + 0.5;
                if state.fireball_charge >= 5 && (0.0..10.0).contains(&angle) {
                    let view = dragon.view_vector();
                    let head = dragon.head_position();
                    let start = Vector3::new(head.x - view.x, head.y + 0.5 + 0.5, head.z - view.z);
                    let direction = Vector3::new(
                        target_pos.x - start.x,
                        target_pos.y + f64::from(attack_target.get_entity().height()) * 0.5
                            - start.y,
                        target_pos.z - start.z,
                    );
                    if !dragon.get_entity().silent.load(Ordering::Relaxed) {
                        world.sync_world_event(
                            WorldEvent::SoundDragonFireball,
                            dragon.get_entity().block_pos.load(),
                            0,
                        );
                    }
                    let fireball_entity =
                        Entity::new(world.clone(), start, &EntityType::DRAGON_FIREBALL);
                    let fireball = DragonFireballEntity::new_shot(
                        fireball_entity,
                        dragon.get_entity(),
                        direction,
                    );
                    world.spawn_entity(Arc::new(fireball));
                    state.fireball_charge = 0;
                    if let Some(path) = state.path.as_mut() {
                        while !path.is_done() {
                            path.advance();
                        }
                    }
                    drop(state);
                    dragon.set_phase(EnderDragonPhase::HoldingPattern);
                }
            } else if state.fireball_charge > 0 {
                state.fireball_charge -= 1;
            }
        } else if state.fireball_charge > 0 {
            state.fireball_charge -= 1;
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.fireball_charge = 0;
        state.target = None;
        state.path = None;
        state.attack_target = None;
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        lock(&self.state).target
    }
}

// ───────────────────────────── landing approach ─────────────────────────────

#[derive(Default)]
struct LandingApproachState {
    path: Option<DragonPath>,
    target: Option<Vector3<f64>>,
}

#[derive(Default)]
pub struct LandingApproachPhase {
    state: Mutex<LandingApproachState>,
}

impl DragonPhase for LandingApproachPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::LandingApproach
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.path = None;
        state.target = None;
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let mut state = lock(&self.state);
        if !target_reached_or_lost(state.target, dragon.position()) {
            return;
        }
        if state.path.as_ref().is_none_or(DragonPath::is_done) {
            let current = dragon.find_closest_node();
            let egg = dragon.podium_surface(world);
            let nearest = dragon.nearest_player(
                f64::from(egg.0.x),
                f64::from(egg.0.y),
                f64::from(egg.0.z),
                None,
                |_| true,
            );
            let target_index = nearest.map_or_else(
                || dragon.find_closest_node_to(40.0, f64::from(egg.0.y), 0.0),
                |player| {
                    let p = player.get_entity().pos.load();
                    let aim = Vector3::new(p.x, 0.0, p.z).normalize();
                    dragon.find_closest_node_to(-aim.x * 40.0, 105.0, -aim.z * 40.0)
                },
            );
            let final_node = Node {
                x: egg.0.x,
                y: egg.0.y,
                z: egg.0.z,
            };
            state.path = dragon.find_path(current, target_index, Some(final_node));
            if let Some(path) = state.path.as_mut() {
                path.advance();
            }
        }
        if let Some(path) = state.path.as_mut()
            && !path.is_done()
        {
            let node = path.next_node();
            path.advance();
            state.target = Some(target_above(node));
        }
        if state.path.as_ref().is_some_and(DragonPath::is_done) {
            drop(state);
            dragon.set_phase(EnderDragonPhase::Landing);
        }
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        lock(&self.state).target
    }
}

// ───────────────────────────── landing ─────────────────────────────

#[derive(Default)]
pub struct LandingPhase {
    target: Mutex<Option<Vector3<f64>>>,
}

impl DragonPhase for LandingPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::Landing
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let mut target = lock(&self.target);
        if target.is_none() {
            let egg = dragon.podium_surface(world);
            *target = Some(Vector3::new(
                f64::from(egg.0.x) + 0.5,
                f64::from(egg.0.y),
                f64::from(egg.0.z) + 0.5,
            ));
        }
        let reached =
            target.is_some_and(|target| target.squared_distance_to_vec(&dragon.position()) < 1.0);
        drop(target);
        if reached {
            dragon.phases().sitting_flaming.reset_flame_count();
            dragon.set_phase(EnderDragonPhase::SittingScanning);
        }
    }

    fn fly_speed(&self) -> f32 {
        1.5
    }

    fn turn_speed(&self, dragon: &EnderDragonEntity) -> f32 {
        let rot_speed = dragon.delta_movement().horizontal_length() as f32 + 1.0;
        let dist = rot_speed.min(40.0);
        dist / rot_speed
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        *lock(&self.target) = None;
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        *lock(&self.target)
    }
}

// ───────────────────────────── takeoff ─────────────────────────────

#[derive(Default)]
struct TakeoffState {
    first_tick: bool,
    path: Option<DragonPath>,
    target: Option<Vector3<f64>>,
}

#[derive(Default)]
pub struct TakeoffPhase {
    state: Mutex<TakeoffState>,
}

impl TakeoffPhase {
    fn find_new_target(dragon: &EnderDragonEntity, state: &mut TakeoffState) {
        let current = dragon.find_closest_node();
        let look = dragon.head_look_vector();
        let mut target = dragon.find_closest_node_to(-look.x * 40.0, 105.0, -look.z * 40.0) as i32;
        if dragon.has_fight() && dragon.alive_crystals() > 0 {
            target = target.rem_euclid(12);
        } else {
            target -= 12;
            target &= 7;
            target += 12;
        }
        state.path = dragon.find_path(current, target.rem_euclid(24) as usize, None);
        if let Some(path) = state.path.as_mut() {
            path.advance();
            if !path.is_done() {
                let node = path.next_node();
                path.advance();
                state.target = Some(target_above(node));
            }
        }
    }
}

impl DragonPhase for TakeoffPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::Takeoff
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let mut state = lock(&self.state);
        if !state.first_tick && state.path.is_some() {
            let egg = dragon.podium_surface(world);
            let center = Vector3::new(
                f64::from(egg.0.x) + 0.5,
                f64::from(egg.0.y) + 0.5,
                f64::from(egg.0.z) + 0.5,
            );
            if center.squared_distance_to_vec(&dragon.position()) >= 100.0 {
                drop(state);
                dragon.set_phase(EnderDragonPhase::HoldingPattern);
            }
        } else {
            state.first_tick = false;
            Self::find_new_target(dragon, &mut state);
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.first_tick = true;
        state.path = None;
        state.target = None;
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        lock(&self.state).target
    }
}

// ───────────────────────────── sitting ─────────────────────────────

/// Vanilla `AbstractDragonSittingPhase.onHurt`: arrows and wind charges bounce off.
fn sitting_on_hurt(source: Option<&dyn EntityBase>, damage: f32) -> f32 {
    if let Some(direct) = source {
        let entity_type = direct.get_entity().entity_type;
        if entity_type == &EntityType::ARROW
            || entity_type == &EntityType::SPECTRAL_ARROW
            || entity_type == &EntityType::TRIDENT
            || entity_type == &EntityType::WIND_CHARGE
        {
            direct.get_entity().set_on_fire_for(1.0);
            return 0.0;
        }
    }
    damage
}

#[derive(Default)]
pub struct SittingScanningPhase {
    scanning_time: Mutex<i32>,
}

impl DragonPhase for SittingScanningPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::SittingScanning
    }

    fn is_sitting(&self) -> bool {
        true
    }

    fn on_hurt(&self, source: Option<&dyn EntityBase>, damage: f32) -> f32 {
        sitting_on_hurt(source, damage)
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, _world: &Arc<World>) {
        let scanning_time = {
            let mut time = lock(&self.scanning_time);
            *time += 1;
            *time
        };
        let me = dragon.position();
        let attack_target = dragon.nearest_player(me.x, me.y, me.z, Some(20.0), |player| {
            (player.get_entity().pos.load().y - me.y).abs() <= 10.0
        });
        if let Some(target) = attack_target {
            if scanning_time > 25 {
                dragon.set_phase(EnderDragonPhase::SittingAttacking);
            } else {
                let target_pos = target.get_entity().pos.load();
                let aim = Vector3::new(target_pos.x - me.x, 0.0, target_pos.z - me.z).normalize();
                let yaw_rad = f64::from(dragon.yaw()).to_radians();
                let dir = Vector3::new(yaw_rad.sin(), 0.0, -yaw_rad.cos()).normalize();
                let dot = dir.dot(&aim) as f32;
                let angle = dot.acos().to_degrees() + 0.5;
                if angle < 0.0 || angle > 10.0 {
                    let head = dragon.head_position();
                    let x_attack_dist = target_pos.x - head.x;
                    let z_attack_dist = target_pos.z - head.z;
                    let y_rot_delta = pumpkin_util::math::wrap_degrees_f64(
                        180.0
                            - x_attack_dist.atan2(z_attack_dist).to_degrees()
                            - f64::from(dragon.yaw()),
                    )
                    .clamp(-100.0, 100.0);
                    dragon.scale_y_rot_a(0.8);
                    let dist = (x_attack_dist.hypot(z_attack_dist) as f32) + 1.0;
                    let rot_speed = dist;
                    let dist = dist.min(40.0);
                    dragon.add_y_rot_a(y_rot_delta as f32 * (0.7 / dist / rot_speed));
                    dragon.set_yaw(dragon.yaw() + dragon.y_rot_a());
                }
            }
        } else if scanning_time >= 100 {
            let charge_target = dragon.nearest_player(me.x, me.y, me.z, Some(150.0), |_| true);
            dragon.set_phase(EnderDragonPhase::Takeoff);
            if let Some(target) = charge_target {
                dragon.set_phase(EnderDragonPhase::ChargingPlayer);
                let pos = target.get_entity().pos.load();
                dragon.phases().charging_player.set_target(pos);
            }
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        *lock(&self.scanning_time) = 0;
    }
}

#[derive(Default)]
struct FlamingState {
    flame_ticks: i32,
    flame_count: i32,
    flame: Option<Arc<dyn EntityBase>>,
}

#[derive(Default)]
pub struct SittingFlamingPhase {
    state: Mutex<FlamingState>,
}

impl SittingFlamingPhase {
    pub fn reset_flame_count(&self) {
        lock(&self.state).flame_count = 0;
    }
}

impl DragonPhase for SittingFlamingPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::SittingFlaming
    }

    fn is_sitting(&self) -> bool {
        true
    }

    fn on_hurt(&self, source: Option<&dyn EntityBase>, damage: f32) -> f32 {
        sitting_on_hurt(source, damage)
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let (ticks, count) = {
            let mut state = lock(&self.state);
            state.flame_ticks += 1;
            (state.flame_ticks, state.flame_count)
        };
        if ticks >= 200 {
            if count >= 4 {
                dragon.set_phase(EnderDragonPhase::Takeoff);
            } else {
                dragon.set_phase(EnderDragonPhase::SittingScanning);
            }
        } else if ticks == 10 {
            let head = dragon.head_position();
            let me = dragon.position();
            let look = Vector3::new(head.x - me.x, 0.0, head.z - me.z).normalize();
            let x = head.x + look.x * 5.0 / 2.0;
            let z = head.z + look.z * 5.0 / 2.0;
            let initial_y = head.y + 0.5;
            let mut y = initial_y;
            let min_y = f64::from(world.min_y);
            while world
                .get_block(&pumpkin_util::math::position::BlockPos::new(
                    x.floor() as i32,
                    y.floor() as i32,
                    z.floor() as i32,
                ))
                .is_air()
            {
                y -= 1.0;
                if y < 0.0 || y < min_y {
                    y = initial_y;
                    break;
                }
            }
            y = y.floor() + 1.0;
            let cloud_entity = Entity::new(
                world.clone(),
                Vector3::new(x, y, z),
                &EntityType::AREA_EFFECT_CLOUD,
            );
            let cloud =
                crate::entity::area_effect_cloud::AreaEffectCloudEntity::create_dragon_breath(
                    cloud_entity,
                    Some(dragon.get_entity().entity_uuid),
                    200,
                    5.0,
                    0.0,
                    0,
                    1,
                );
            lock(&self.state).flame = Some(cloud.clone());
            world.spawn_entity(cloud);
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.flame_ticks = 0;
        state.flame_count += 1;
    }

    fn end(&self, _dragon: &EnderDragonEntity) {
        let flame = lock(&self.state).flame.take();
        if let Some(flame) = flame {
            flame.get_entity().remove();
        }
    }
}

#[derive(Default)]
pub struct SittingAttackingPhase {
    attacking_ticks: Mutex<i32>,
}

impl DragonPhase for SittingAttackingPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::SittingAttacking
    }

    fn is_sitting(&self) -> bool {
        true
    }

    fn on_hurt(&self, source: Option<&dyn EntityBase>, damage: f32) -> f32 {
        sitting_on_hurt(source, damage)
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, _world: &Arc<World>) {
        let ticks = {
            let mut ticks = lock(&self.attacking_ticks);
            let previous = *ticks;
            *ticks += 1;
            previous
        };
        if ticks >= 40 {
            dragon.set_phase(EnderDragonPhase::SittingFlaming);
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        *lock(&self.attacking_ticks) = 0;
    }
}

// ───────────────────────────── charge player ─────────────────────────────

#[derive(Default)]
struct ChargeState {
    target: Option<Vector3<f64>>,
    time_since_charge: i32,
}

#[derive(Default)]
pub struct ChargePlayerPhase {
    state: Mutex<ChargeState>,
}

impl ChargePlayerPhase {
    pub fn set_target(&self, target: Vector3<f64>) {
        lock(&self.state).target = Some(target);
    }
}

impl DragonPhase for ChargePlayerPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::ChargingPlayer
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, _world: &Arc<World>) {
        let mut state = lock(&self.state);
        let Some(target) = state.target else {
            drop(state);
            dragon.set_phase(EnderDragonPhase::HoldingPattern);
            return;
        };
        if state.time_since_charge > 0 {
            let previous = state.time_since_charge;
            state.time_since_charge += 1;
            if previous >= 10 {
                drop(state);
                dragon.set_phase(EnderDragonPhase::HoldingPattern);
                return;
            }
        }
        if target_reached_or_lost(Some(target), dragon.position()) {
            state.time_since_charge += 1;
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        let mut state = lock(&self.state);
        state.target = None;
        state.time_since_charge = 0;
    }

    fn fly_speed(&self) -> f32 {
        3.0
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        lock(&self.state).target
    }
}

// ───────────────────────────── death ─────────────────────────────

#[derive(Default)]
pub struct DeathPhase {
    target: Mutex<Option<Vector3<f64>>>,
}

impl DragonPhase for DeathPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::Dying
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, world: &Arc<World>) {
        let target = {
            let mut target = lock(&self.target);
            if target.is_none() {
                let egg = dragon.podium_motion_blocking(world);
                *target = Some(Vector3::new(
                    f64::from(egg.0.x) + 0.5,
                    f64::from(egg.0.y),
                    f64::from(egg.0.z) + 0.5,
                ));
            }
            *target
        };
        if target_reached_or_lost(target, dragon.position()) {
            dragon.set_health_direct(0.0);
        } else {
            dragon.set_health_direct(1.0);
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        *lock(&self.target) = None;
    }

    fn fly_speed(&self) -> f32 {
        3.0
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        *lock(&self.target)
    }
}

// ───────────────────────────── hover ─────────────────────────────

#[derive(Default)]
pub struct HoverPhase {
    target: Mutex<Option<Vector3<f64>>>,
}

impl DragonPhase for HoverPhase {
    fn phase(&self) -> EnderDragonPhase {
        EnderDragonPhase::Hovering
    }

    fn is_sitting(&self) -> bool {
        true
    }

    fn server_tick(&self, dragon: &EnderDragonEntity, _world: &Arc<World>) {
        let mut target = lock(&self.target);
        if target.is_none() {
            *target = Some(dragon.position());
        }
    }

    fn begin(&self, _dragon: &EnderDragonEntity) {
        *lock(&self.target) = None;
    }

    fn fly_speed(&self) -> f32 {
        1.0
    }

    fn fly_target_location(&self) -> Option<Vector3<f64>> {
        *lock(&self.target)
    }
}
