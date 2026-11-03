//! Ender dragon, ported from `net.minecraft.world.entity.boss.enderdragon.EnderDragon`,
//! `EnderDragonPart` and the node pathfinding it uses.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use pumpkin_data::BlockStateId;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::GameMode;
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::math::wrap_degrees;
use pumpkin_world::chunk::ChunkHeightmapType;
use pumpkin_world::world::BlockFlags;

use crate::entity::experience_orb::ExperienceOrbEntity;
use crate::entity::mob::{Mob, MobEntity};
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::plugin::api::events::entity::ender_dragon_change_phase::EnderDragonChangePhaseEvent;
use crate::server::Server;
use crate::world::World;

pub mod flight_history;
pub mod phase;

use flight_history::DragonFlightHistory;
use phase::{EnderDragonPhase, PhaseManager};

pub const NODE_COUNT: usize = 24;

const NODE_ADJACENCY: [u32; NODE_COUNT] = [
    6146, 8197, 8202, 16404, 32808, 32848, 65696, 131392, 131712, 263424, 526848, 525313, 1581057,
    3166214, 2138120, 6373424, 4358208, 12910976, 9044480, 9706496, 15216640, 13688832, 11763712,
    8257536,
];

const DEATH_TICKS: i32 = 200;
const SITTING_ALLOWED_DAMAGE_PERCENTAGE: f32 = 0.25;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Vanilla `Node` of the dragon's flight graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Node {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Node {
    fn distance_to(self, other: Self) -> f32 {
        self.distance_to_sqr(other).sqrt()
    }

    fn distance_to_sqr(self, other: Self) -> f32 {
        let dx = (other.x - self.x) as f32;
        let dy = (other.y - self.y) as f32;
        let dz = (other.z - self.z) as f32;
        dx * dx + dy * dy + dz * dz
    }
}

/// Vanilla `Path`: nodes plus the index of the next one.
#[derive(Clone, Debug)]
pub struct DragonPath {
    nodes: Vec<Node>,
    next: usize,
}

impl DragonPath {
    #[must_use]
    pub const fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    pub const fn advance(&mut self) {
        self.next += 1;
    }

    /// Vanilla `getNextNodePos`, only valid while not done.
    #[must_use]
    pub fn next_node(&self) -> Node {
        self.nodes[self.next.min(self.nodes.len() - 1)]
    }
}

/// Vanilla `EnderDragon.findPath`, `min_index` is 12 while no crystal is alive.
#[must_use]
pub fn find_path_between(
    nodes: &[Node; NODE_COUNT],
    start: usize,
    end: usize,
    final_node: Option<Node>,
    min_index: usize,
) -> Option<DragonPath> {
    let mut g = [0.0f32; NODE_COUNT];
    let mut f = [0.0f32; NODE_COUNT];
    let mut closed = [false; NODE_COUNT];
    let mut in_open = [false; NODE_COUNT];
    let mut came_from: [Option<usize>; NODE_COUNT] = [None; NODE_COUNT];

    let target = nodes[end];
    f[start] = nodes[start].distance_to(target);
    let mut open = vec![start];
    in_open[start] = true;
    let mut closest = start;

    let reconstruct = |last: usize, came_from: &[Option<usize>; NODE_COUNT]| {
        let mut path = vec![nodes[last]];
        let mut node = last;
        while let Some(previous) = came_from[node] {
            node = previous;
            path.push(nodes[node]);
        }
        path.reverse();
        if let Some(final_node) = final_node {
            path.push(final_node);
        }
        DragonPath {
            nodes: path,
            next: 0,
        }
    };

    while !open.is_empty() {
        let (position, &current) = open
            .iter()
            .enumerate()
            .min_by(|a, b| f[*a.1].total_cmp(&f[*b.1]))
            .unwrap_or((0, &start));
        open.swap_remove(position);
        in_open[current] = false;

        if nodes[current] == target {
            return Some(reconstruct(current, &came_from));
        }
        if nodes[current].distance_to(target) < nodes[closest].distance_to(target) {
            closest = current;
        }
        closed[current] = true;

        for adjacent in min_index..NODE_COUNT {
            if NODE_ADJACENCY[current] & (1 << adjacent) == 0 || closed[adjacent] {
                continue;
            }
            let tentative = g[current] + nodes[current].distance_to(nodes[adjacent]);
            if !in_open[adjacent] || tentative < g[adjacent] {
                came_from[adjacent] = Some(current);
                g[adjacent] = tentative;
                f[adjacent] = tentative + nodes[adjacent].distance_to(target);
                if !in_open[adjacent] {
                    open.push(adjacent);
                    in_open[adjacent] = true;
                }
            }
        }
    }

    (closest != start).then(|| reconstruct(closest, &came_from))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Head,
    Neck,
    Body,
    Tail1,
    Tail2,
    Tail3,
    Wing1,
    Wing2,
}

impl PartKind {
    const ALL: [Self; 8] = [
        Self::Head,
        Self::Neck,
        Self::Body,
        Self::Tail1,
        Self::Tail2,
        Self::Tail3,
        Self::Wing1,
        Self::Wing2,
    ];

    const fn size(self) -> (f32, f32) {
        match self {
            Self::Head => (1.0, 1.0),
            Self::Neck => (3.0, 3.0),
            Self::Body => (5.0, 3.0),
            Self::Tail1 | Self::Tail2 | Self::Tail3 => (2.0, 2.0),
            Self::Wing1 | Self::Wing2 => (4.0, 2.0),
        }
    }
}

/// Vanilla `EnderDragonPart`: a hit box of the dragon that damages it.
pub struct EnderDragonPart {
    pub entity: Entity,
    pub kind: PartKind,
    dragon_uuid: uuid::Uuid,
    dragon: Weak<EnderDragonEntity>,
}

impl EntityBase for EnderDragonPart {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&crate::entity::living::LivingEntity> {
        None
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn tick(&self, _caller: &dyn EntityBase, _server: &Server) {}

    fn can_hit(&self) -> bool {
        true
    }

    fn is_sub_entity(&self) -> bool {
        true
    }

    fn parent_entity_uuid(&self) -> Option<uuid::Uuid> {
        Some(self.dragon_uuid)
    }

    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        self.dragon.upgrade().is_some_and(|dragon| {
            dragon.hurt_part(self.kind, amount, damage_type, position, source, cause)
        })
    }
}

struct DragonInner {
    y_rot_a: f32,
    in_wall: bool,
    flight_history: DragonFlightHistory,
    nodes: Option<[Node; NODE_COUNT]>,
    sitting_damage_received: f32,
    nearest_crystal: Option<Arc<dyn EntityBase>>,
}

pub struct EnderDragonEntity {
    pub mob_entity: MobEntity,
    pub parts: Vec<Arc<EnderDragonPart>>,
    inner: Mutex<DragonInner>,
    phases: PhaseManager,
    fight_origin: Mutex<BlockPos>,
    fight_linked: AtomicBool,
    alive_crystals: AtomicI32,
    dragon_death_time: AtomicI32,
    parts_registered: AtomicBool,
}

impl EnderDragonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        entity.no_physics.store(true, Ordering::Relaxed);
        let world = entity.world.load_full();
        let dragon_uuid = entity.entity_uuid;
        let pos = entity.pos.load();
        let dragon_id = entity.entity_id;
        // the client numbers the parts after the dragon, in the order of PartKind::ALL
        let contiguous = Entity::reserve_ids_after(dragon_id, PartKind::ALL.len() as i32);
        if !contiguous {
            tracing::warn!("ender dragon {dragon_id} could not reserve ids for its parts");
        }
        Arc::new_cyclic(|weak: &Weak<Self>| {
            let parts = PartKind::ALL
                .iter()
                .enumerate()
                .map(|(index, &kind)| {
                    let id = if contiguous {
                        dragon_id + 1 + index as i32
                    } else {
                        Entity::reserve_ids(1)
                    };
                    let part_entity = Entity::from_uuid_with_id(
                        id,
                        uuid::Uuid::new_v4(),
                        world.clone(),
                        pos,
                        &EntityType::ENDER_DRAGON,
                    );
                    let (width, height) = kind.size();
                    part_entity.set_default_dimensions(EntityDimensions::new(
                        width,
                        height,
                        height * 0.85,
                    ));
                    part_entity.no_physics.store(true, Ordering::Relaxed);
                    Arc::new(EnderDragonPart {
                        entity: part_entity,
                        kind,
                        dragon_uuid,
                        dragon: weak.clone(),
                    })
                })
                .collect();
            Self {
                mob_entity: MobEntity::new(entity),
                parts,
                inner: Mutex::new(DragonInner {
                    y_rot_a: 0.0,
                    in_wall: false,
                    flight_history: DragonFlightHistory::default(),
                    nodes: None,
                    sitting_damage_received: 0.0,
                    nearest_crystal: None,
                }),
                phases: PhaseManager::new(),
                fight_origin: Mutex::new(BlockPos::new(0, 0, 0)),
                fight_linked: AtomicBool::new(false),
                alive_crystals: AtomicI32::new(0),
                dragon_death_time: AtomicI32::new(0),
                parts_registered: AtomicBool::new(false),
            }
        })
    }

    fn part(&self, kind: PartKind) -> &Arc<EnderDragonPart> {
        &self.parts[kind as usize]
    }

    // ── accessors used by the phases ─────────────────────────────────────────

    #[must_use]
    pub fn world(&self) -> Arc<World> {
        self.get_entity().world.load_full()
    }

    #[must_use]
    pub fn position(&self) -> Vector3<f64> {
        self.get_entity().pos.load()
    }

    #[must_use]
    pub fn delta_movement(&self) -> Vector3<f64> {
        self.get_entity().velocity.load()
    }

    #[must_use]
    pub fn yaw(&self) -> f32 {
        self.get_entity().yaw.load()
    }

    pub fn set_yaw(&self, yaw: f32) {
        self.get_entity().yaw.store(yaw);
    }

    #[must_use]
    pub fn y_rot_a(&self) -> f32 {
        lock(&self.inner).y_rot_a
    }

    pub fn scale_y_rot_a(&self, factor: f32) {
        lock(&self.inner).y_rot_a *= factor;
    }

    pub fn add_y_rot_a(&self, amount: f32) {
        lock(&self.inner).y_rot_a += amount;
    }

    #[must_use]
    pub const fn phases(&self) -> &PhaseManager {
        &self.phases
    }

    #[must_use]
    pub fn fight_origin(&self) -> BlockPos {
        *lock(&self.fight_origin)
    }

    pub fn set_fight_origin(&self, origin: BlockPos) {
        *lock(&self.fight_origin) = origin;
    }

    /// Vanilla `dragonFight != null`.
    #[must_use]
    pub fn has_fight(&self) -> bool {
        self.fight_linked.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn alive_crystals(&self) -> i32 {
        self.alive_crystals.load(Ordering::Relaxed)
    }

    /// Position of the head part, `head.getX()/getY()/getZ()`.
    #[must_use]
    pub fn head_position(&self) -> Vector3<f64> {
        self.part(PartKind::Head).entity.pos.load()
    }

    #[must_use]
    pub fn view_vector(&self) -> Vector3<f64> {
        let entity = self.get_entity();
        Vector3::rotation_vector(f64::from(entity.pitch.load()), f64::from(entity.yaw.load()))
    }

    /// Vanilla `getHeadLookVector`.
    #[must_use]
    pub fn head_look_vector(&self) -> Vector3<f64> {
        let entity = self.get_entity();
        let phase = self.phases.current_type();
        let x_rot = if matches!(phase, EnderDragonPhase::Landing | EnderDragonPhase::Takeoff) {
            let world = self.world();
            let egg = self.podium_surface(&world);
            let center = Vector3::new(
                f64::from(egg.0.x) + 0.5,
                f64::from(egg.0.y) + 0.5,
                f64::from(egg.0.z) + 0.5,
            );
            let dist =
                ((center.squared_distance_to_vec(&self.position())).sqrt() as f32 / 4.0).max(1.0);
            let y_offset = 6.0 / dist;
            -y_offset * 1.5 * 5.0
        } else if self.phases.current().is_sitting() {
            -45.0
        } else {
            entity.pitch.load()
        };
        Vector3::rotation_vector(f64::from(x_rot), f64::from(entity.yaw.load()))
    }

    /// `getHeightmapPos(MOTION_BLOCKING_NO_LEAVES, podium)`.
    #[must_use]
    pub fn podium_surface(&self, world: &World) -> BlockPos {
        let origin = self.fight_origin();
        BlockPos::new(
            origin.0.x,
            world.get_heightmap_height(
                ChunkHeightmapType::MotionBlockingNoLeaves,
                origin.0.x,
                origin.0.z,
            ),
            origin.0.z,
        )
    }

    /// `getHeightmapPos(MOTION_BLOCKING, podium)`.
    #[must_use]
    pub fn podium_motion_blocking(&self, world: &World) -> BlockPos {
        let origin = self.fight_origin();
        BlockPos::new(
            origin.0.x,
            world.get_heightmap_height(ChunkHeightmapType::MotionBlocking, origin.0.x, origin.0.z),
            origin.0.z,
        )
    }

    pub fn set_health_direct(&self, health: f32) {
        self.mob_entity.living_entity.set_health(health);
    }

    #[must_use]
    pub fn has_line_of_sight_to(&self, target: &Player) -> bool {
        Mob::has_line_of_sight(self, target.get_entity())
    }

    /// Vanilla `canAttack`: `target.canBeSeenAsEnemy()`.
    #[must_use]
    pub fn can_attack(&self, target: &Player) -> bool {
        target.living_entity.health.load() > 0.0
            && matches!(
                target.gamemode.load(),
                GameMode::Survival | GameMode::Adventure
            )
    }

    /// Vanilla `Level.getNearestPlayer(TargetingConditions.forCombat()…, dragon, x, y, z)`,
    /// `range` overrides the follow range of the dragon.
    pub fn nearest_player(
        &self,
        x: f64,
        y: f64,
        z: f64,
        range: Option<f64>,
        accept: impl Fn(&Player) -> bool,
    ) -> Option<Arc<Player>> {
        let world = self.world();
        let me = self.position();
        let follow = range.unwrap_or_else(|| {
            self.mob_entity
                .living_entity
                .get_attribute_value(&Attributes::FOLLOW_RANGE)
        });
        let from = Vector3::new(x, y, z);
        world
            .players
            .load()
            .iter()
            .filter(|player| {
                let max_dist = (follow * Self::visibility(player)).max(2.0);
                self.can_attack(player)
                    && accept(player)
                    && me.squared_distance_to_vec(&player.get_entity().pos.load())
                        <= max_dist * max_dist
            })
            .min_by(|a, b| {
                from.squared_distance_to_vec(&a.get_entity().pos.load())
                    .total_cmp(&from.squared_distance_to_vec(&b.get_entity().pos.load()))
            })
            .cloned()
    }

    /// Vanilla `Player.getVisibilityPercent`, sneaking and invisibility (no mob heads).
    fn visibility(player: &Player) -> f64 {
        let mut visibility = 1.0;
        if player.get_entity().is_sneaking() {
            visibility *= 0.8;
        }
        if player
            .living_entity
            .has_effect(&pumpkin_data::effect::StatusEffect::INVISIBILITY)
        {
            let armor = (36..40)
                .filter(|slot| !player.inventory().get_slot(*slot).is_empty())
                .count();
            let cover = (armor as f64 / 4.0).max(0.1);
            visibility *= 0.7 * cover;
        }
        visibility
    }

    pub fn strafe_player(&self, player: &Arc<Player>) {
        self.set_phase(EnderDragonPhase::StrafePlayer);
        self.phases.strafe_player.set_target(self, player);
    }

    // ── phases ───────────────────────────────────────────────────────────────

    /// Vanilla `EnderDragonPhaseManager.setPhase`.
    pub fn set_phase(&self, target: EnderDragonPhase) {
        let current = self.phases.current_type();
        if current == target {
            return;
        }
        let world = self.world();
        if let Some(server) = world.server.upgrade() {
            let mut event = EnderDragonChangePhaseEvent {
                entity_id: self.get_entity().entity_id,
                current_phase: format!("{current:?}"),
                new_phase: format!("{target:?}"),
                cancelled: false,
            };
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return;
            }
        }
        self.phases.get(current).end(self);
        self.phases.store_current(target);
        self.get_entity()
            .set_synced_data(tracked_data::ender_dragon::DATA_PHASE, target.id());
        self.phases.get(target).begin(self);
    }

    // ── flight graph ─────────────────────────────────────────────────────────

    fn ensure_nodes(&self, world: &World) -> [Node; NODE_COUNT] {
        let mut inner = lock(&self.inner);
        if let Some(nodes) = inner.nodes {
            return nodes;
        }
        let mut nodes = [Node::default(); NODE_COUNT];
        for (i, node) in nodes.iter_mut().enumerate() {
            let mut y_adjustment = 5;
            let mut multiplier = i as f32;
            let (radius, step) = if i < 12 {
                (60.0f32, std::f32::consts::PI / 12.0)
            } else if i < 20 {
                multiplier -= 12.0;
                y_adjustment += 10;
                (40.0, std::f32::consts::PI / 8.0)
            } else {
                multiplier -= 20.0;
                (20.0, std::f32::consts::PI / 4.0)
            };
            let angle = 2.0 * (-std::f32::consts::PI + step * multiplier);
            let x = (radius * angle.cos()).floor() as i32;
            let z = (radius * angle.sin()).floor() as i32;
            let height =
                world.get_heightmap_height(ChunkHeightmapType::MotionBlockingNoLeaves, x, z);
            *node = Node {
                x,
                y: 73.max(height + y_adjustment),
                z,
            };
        }
        inner.nodes = Some(nodes);
        nodes
    }

    fn min_node_index(&self) -> usize {
        if self.has_fight() && self.alive_crystals() > 0 {
            0
        } else {
            12
        }
    }

    /// Vanilla `findClosestNode()`.
    #[must_use]
    pub fn find_closest_node(&self) -> usize {
        let pos = self.position();
        self.find_closest_node_to(pos.x, pos.y, pos.z)
    }

    /// Vanilla `findClosestNode(x, y, z)`.
    #[must_use]
    pub fn find_closest_node_to(&self, x: f64, y: f64, z: f64) -> usize {
        let nodes = self.ensure_nodes(&self.world());
        let current = Node {
            x: x.floor() as i32,
            y: y.floor() as i32,
            z: z.floor() as i32,
        };
        let mut closest_dist = 10000.0f32;
        let mut closest_index = 0;
        for (i, node) in nodes.iter().enumerate().skip(self.min_node_index()) {
            let dist = node.distance_to_sqr(current);
            if dist < closest_dist {
                closest_dist = dist;
                closest_index = i;
            }
        }
        closest_index
    }

    /// Vanilla `findPath`.
    #[must_use]
    pub fn find_path(
        &self,
        start: usize,
        end: usize,
        final_node: Option<Node>,
    ) -> Option<DragonPath> {
        let nodes = self.ensure_nodes(&self.world());
        find_path_between(&nodes, start, end, final_node, self.min_node_index())
    }

    // ── per tick ─────────────────────────────────────────────────────────────

    fn tick_part(&self, kind: PartKind, x: f64, y: f64, z: f64) {
        let pos = self.position();
        self.part(kind)
            .entity
            .set_pos(Vector3::new(pos.x + x, pos.y + y, pos.z + z));
    }

    fn register_parts(&self, world: &World) {
        if !self.parts_registered.swap(true, Ordering::AcqRel) {
            let parts: Vec<Arc<dyn EntityBase>> = self
                .parts
                .iter()
                .map(|part| part.clone() as Arc<dyn EntityBase>)
                .collect();
            world.add_sub_entities(&parts);
        }
    }

    fn sync_fight(&self, world: &World) {
        let uuid = self.get_entity().entity_uuid;
        if let Some(fight) = world.dragon_fight.as_ref()
            && let Ok(fight) = fight.try_lock()
        {
            self.fight_linked
                .store(fight.dragon_uuid() == Some(uuid), Ordering::Relaxed);
            self.alive_crystals
                .store(fight.alive_crystals(), Ordering::Relaxed);
        }
    }

    fn update_fight(&self, world: &Arc<World>) {
        if !self.has_fight() {
            return;
        }
        if let Some(fight) = world.dragon_fight.as_ref()
            && let Ok(mut fight) = fight.try_lock()
        {
            let living = &self.mob_entity.living_entity;
            let custom_name = living.entity.custom_name.load();
            fight.update_dragon(
                world,
                self.get_entity().entity_uuid,
                living.health.load(),
                living.get_max_health(),
                custom_name.as_ref().as_ref(),
            );
        }
    }

    /// Vanilla `checkCrystals`.
    fn check_crystals(&self, world: &World) {
        let tick_count = self.get_entity().tick_count.load(Ordering::Relaxed);
        let mut inner = lock(&self.inner);
        if let Some(crystal) = inner.nearest_crystal.clone() {
            if crystal.get_entity().is_removed() {
                inner.nearest_crystal = None;
            } else if tick_count % 10 == 0 {
                let living = &self.mob_entity.living_entity;
                if living.health.load() < living.get_max_health() {
                    living.set_health(living.health.load() + 1.0);
                }
            }
        }
        if rand::random_range(0..10) == 0 {
            drop(inner);
            let area = self
                .get_entity()
                .bounding_box
                .load()
                .expand(32.0, 32.0, 32.0);
            let me = self.position();
            let nearest = world
                .get_entities_at_box(&area)
                .into_iter()
                .filter(|entity| entity.get_entity().entity_type == &EntityType::END_CRYSTAL)
                .min_by(|a, b| {
                    a.get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&me)
                        .total_cmp(&b.get_entity().pos.load().squared_distance_to_vec(&me))
                });
            lock(&self.inner).nearest_crystal = nearest;
        }
    }

    fn push_entity(target: &dyn EntityBase, push: Vector3<f64>) {
        if let Some(player) = target.get_player() {
            let current = Entity::known_movement(target);
            player.set_velocity(current + push);
        } else {
            let entity = target.get_entity();
            entity.velocity.store(entity.velocity.load() + push);
            entity.velocity_dirty.store(true, Ordering::Relaxed);
        }
    }

    fn targets_in(&self, world: &World, area: &BoundingBox) -> Vec<Arc<dyn EntityBase>> {
        let me = self.get_entity().entity_id;
        world
            .get_all_at_box(area)
            .into_iter()
            .filter(|entity| {
                let base = entity.get_entity();
                base.entity_id != me
                    && !entity.is_sub_entity()
                    && base.entity_type != &EntityType::ENDER_DRAGON
                    && entity.get_living_entity().is_some()
                    && !entity.get_player().is_some_and(|player| {
                        matches!(
                            player.gamemode.load(),
                            GameMode::Creative | GameMode::Spectator
                        )
                    })
            })
            .collect()
    }

    /// Vanilla `knockBack`.
    fn knock_back(&self, targets: &[Arc<dyn EntityBase>]) {
        let body = self.part(PartKind::Body).entity.bounding_box.load();
        let xm = f64::midpoint(body.min.x, body.max.x);
        let zm = f64::midpoint(body.min.z, body.max.z);
        let sitting = self.phases.current().is_sitting();
        for target in targets {
            let pos = target.get_entity().pos.load();
            let xd = pos.x - xm;
            let zd = pos.z - zm;
            let dd = (xd * xd + zd * zd).max(0.1);
            Self::push_entity(
                target.as_ref(),
                Vector3::new(xd / dd * 4.0, f64::from(0.2f32), zd / dd * 4.0),
            );
            if !sitting {
                target.damage_with_context(
                    target.as_ref(),
                    5.0,
                    DamageType::MOB_ATTACK,
                    Some(self.position()),
                    Some(self),
                    Some(self),
                );
            }
        }
    }

    /// Vanilla `hurt(level, entities)`: head and neck bite.
    fn bite(&self, targets: &[Arc<dyn EntityBase>]) {
        for target in targets {
            target.damage_with_context(
                target.as_ref(),
                10.0,
                DamageType::MOB_ATTACK,
                Some(self.position()),
                Some(self),
                Some(self),
            );
        }
    }

    /// Vanilla `checkWalls`: the dragon eats through blocks.
    fn check_walls(world: &Arc<World>, area: BoundingBox) -> bool {
        let x0 = area.min.x.floor() as i32;
        let y0 = area.min.y.floor() as i32;
        let z0 = area.min.z.floor() as i32;
        let x1 = area.max.x.floor() as i32;
        let y1 = area.max.y.floor() as i32;
        let z1 = area.max.z.floor() as i32;
        let griefing = world.level_info.load().game_rules.mob_griefing;
        let mut hit_wall = false;
        let mut destroyed = false;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    let pos = BlockPos::new(x, y, z);
                    let (block, state) = world.get_block_and_state(&pos);
                    if state.is_air() || block.has_tag(&tag::Block::MINECRAFT_DRAGON_TRANSPARENT) {
                        continue;
                    }
                    if griefing && !block.has_tag(&tag::Block::MINECRAFT_DRAGON_IMMUNE) {
                        world.set_block_state(&pos, BlockStateId::AIR, BlockFlags::NOTIFY_ALL);
                        destroyed = true;
                    } else {
                        hit_wall = true;
                    }
                }
            }
        }
        if destroyed {
            let pos = BlockPos::new(
                x0 + rand::random_range(0..=(x1 - x0)),
                y0 + rand::random_range(0..=(y1 - y0)),
                z0 + rand::random_range(0..=(z1 - z0)),
            );
            world.sync_world_event(WorldEvent::ParticlesDragonBlockBreak, pos, 0);
        }
        hit_wall
    }

    /// Vanilla `EnderDragon.aiStep` (server side).
    #[expect(clippy::too_many_lines)]
    fn ai_step(&self) {
        let entity = self.get_entity();
        let world = self.world();
        self.register_parts(&world);
        self.sync_fight(&world);
        if self.mob_entity.living_entity.is_dead_or_dying() {
            return;
        }
        self.check_crystals(&world);

        entity.yaw.store(wrap_degrees(entity.yaw.load()));
        if self.mob_entity.is_no_ai() {
            return;
        }
        lock(&self.inner)
            .flight_history
            .record(entity.pos.load().y, entity.yaw.load());

        let mut phase = self.phases.current();
        phase.server_tick(self, &world);
        if self.phases.current_type() != phase.phase() {
            phase = self.phases.current();
            phase.server_tick(self, &world);
        }

        if let Some(target) = phase.fly_target_location() {
            let pos = entity.pos.load();
            let xdd = target.x - pos.x;
            let mut ydd = target.y - pos.y;
            let zdd = target.z - pos.z;
            let dist_to_target = xdd * xdd + ydd * ydd + zdd * zdd;
            let max = f64::from(phase.fly_speed());
            let horizontal_dist = xdd.hypot(zdd);
            if horizontal_dist > 0.0 {
                ydd = (ydd / horizontal_dist).clamp(-max, max);
            }
            let mut velocity = entity.velocity.load();
            velocity.y += ydd * 0.01;
            entity.velocity.store(velocity);
            entity.yaw.store(wrap_degrees(entity.yaw.load()));
            let aim = (target - pos).normalize();
            let yaw_rad = (entity.yaw.load() * (std::f32::consts::PI / 180.0)).sin();
            let yaw_cos = (entity.yaw.load() * (std::f32::consts::PI / 180.0)).cos();
            let dir = Vector3::new(f64::from(yaw_rad), velocity.y, -f64::from(yaw_cos)).normalize();
            let dot = ((dir.dot(&aim) as f32 + 0.5) / 1.5).max(0.0);
            if xdd.abs() > f64::from(1.0E-5f32) || zdd.abs() > f64::from(1.0E-5f32) {
                let y_rot_d =
                    wrap_degrees(180.0 - (xdd.atan2(zdd).to_degrees() as f32) - entity.yaw.load())
                        .clamp(-50.0, 50.0);
                let mut inner = lock(&self.inner);
                inner.y_rot_a *= 0.8;
                inner.y_rot_a += y_rot_d * phase.turn_speed(self);
                let y_rot_a = inner.y_rot_a;
                drop(inner);
                entity.yaw.store(entity.yaw.load() + y_rot_a * 0.1);
            }
            let span = (2.0 / (dist_to_target + 1.0)) as f32;
            self.move_relative(
                0.06 * (dot * span + (1.0 - span)),
                Vector3::new(0.0, 0.0, -1.0),
            );
            let in_wall = lock(&self.inner).in_wall;
            let velocity = entity.velocity.load();
            let step = if in_wall {
                velocity * f64::from(0.8f32)
            } else {
                velocity
            };
            entity.set_pos(entity.pos.load() + step);
            let actual = entity.velocity.load().normalize();
            let slide = 0.8 + 0.15 * (actual.dot(&dir) + 1.0) / 2.0;
            let velocity = entity.velocity.load();
            entity.velocity.store(Vector3::new(
                velocity.x * slide,
                velocity.y * f64::from(0.91f32),
                velocity.z * slide,
            ));
            entity.velocity_dirty.store(true, Ordering::Relaxed);
        }

        entity.body_yaw.store(entity.yaw.load());
        entity.head_yaw.store(entity.yaw.load());

        let (tilt, p1, history_5, history_0) = {
            let inner = lock(&self.inner);
            let h = &inner.flight_history;
            (
                (h.get(5).y - h.get(10).y) as f32 * 10.0 * (std::f32::consts::PI / 180.0),
                h.get(5),
                h.get(5).y,
                h.get(0).y,
            )
        };
        let cc_tilt = tilt.cos();
        let ss_tilt = tilt.sin();
        let rot1 = entity.yaw.load() * (std::f32::consts::PI / 180.0);
        let ss1 = rot1.sin();
        let cc1 = rot1.cos();
        self.tick_part(
            PartKind::Body,
            f64::from(ss1 * 0.5),
            0.0,
            f64::from(-cc1 * 0.5),
        );
        self.tick_part(
            PartKind::Wing1,
            f64::from(cc1 * 4.5),
            2.0,
            f64::from(ss1 * 4.5),
        );
        self.tick_part(
            PartKind::Wing2,
            f64::from(cc1 * -4.5),
            2.0,
            f64::from(ss1 * -4.5),
        );

        if self
            .mob_entity
            .living_entity
            .hurt_cooldown
            .load(Ordering::Relaxed)
            <= 10
        {
            for kind in [PartKind::Wing1, PartKind::Wing2] {
                let area = self
                    .part(kind)
                    .entity
                    .bounding_box
                    .load()
                    .expand(4.0, 2.0, 4.0)
                    .shift(Vector3::new(0.0, -2.0, 0.0));
                let targets = self.targets_in(&world, &area);
                self.knock_back(&targets);
            }
            for kind in [PartKind::Head, PartKind::Neck] {
                let area = self
                    .part(kind)
                    .entity
                    .bounding_box
                    .load()
                    .expand(1.0, 1.0, 1.0);
                let targets = self.targets_in(&world, &area);
                self.bite(&targets);
            }
        }

        let y_rot_a = self.y_rot_a();
        let rot2 = entity.yaw.load() * (std::f32::consts::PI / 180.0) - y_rot_a * 0.01;
        let ss2 = rot2.sin();
        let cc2 = rot2.cos();
        let y_offset = if self.phases.current().is_sitting() {
            -1.0
        } else {
            (history_5 - history_0) as f32
        };
        self.tick_part(
            PartKind::Head,
            f64::from(ss2 * 6.5 * cc_tilt),
            f64::from(y_offset + ss_tilt * 6.5),
            f64::from(-cc2 * 6.5 * cc_tilt),
        );
        self.tick_part(
            PartKind::Neck,
            f64::from(ss2 * 5.5 * cc_tilt),
            f64::from(y_offset + ss_tilt * 5.5),
            f64::from(-cc2 * 5.5 * cc_tilt),
        );
        for (i, kind) in [PartKind::Tail1, PartKind::Tail2, PartKind::Tail3]
            .into_iter()
            .enumerate()
        {
            let p0 = lock(&self.inner).flight_history.get(12 + i as i32 * 2);
            let rot = entity.yaw.load() * (std::f32::consts::PI / 180.0)
                + wrap_degrees(p0.y_rot - p1.y_rot) * (std::f32::consts::PI / 180.0);
            let ss = rot.sin();
            let cc = rot.cos();
            let dd = (i as f32 + 1.0) * 2.0;
            self.tick_part(
                kind,
                f64::from(-(ss1 * 1.5 + ss * dd) * cc_tilt),
                p0.y - p1.y - f64::from((dd + 1.5) * ss_tilt) + 1.5,
                f64::from((cc1 * 1.5 + cc * dd) * cc_tilt),
            );
        }

        let in_wall =
            Self::check_walls(&world, self.part(PartKind::Head).entity.bounding_box.load())
                | Self::check_walls(&world, self.part(PartKind::Neck).entity.bounding_box.load())
                | Self::check_walls(&world, self.part(PartKind::Body).entity.bounding_box.load());
        lock(&self.inner).in_wall = in_wall;
        self.update_fight(&world);
    }

    /// Vanilla `Entity.moveRelative`.
    fn move_relative(&self, speed: f32, input: Vector3<f64>) {
        let entity = self.get_entity();
        let len = input.length_squared();
        if len < 1.0E-7 {
            return;
        }
        let movement = if len > 1.0 { input.normalize() } else { input } * f64::from(speed);
        let yaw = entity.yaw.load() * (std::f32::consts::PI / 180.0);
        let sin = f64::from(yaw.sin());
        let cos = f64::from(yaw.cos());
        entity.velocity.store(
            entity.velocity.load()
                + Vector3::new(
                    movement.x * cos - movement.z * sin,
                    movement.y,
                    movement.z * cos + movement.x * sin,
                ),
        );
    }

    /// Vanilla `hurt(level, part, source, damage)`.
    pub fn hurt_part(
        &self,
        part: PartKind,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let phase = self.phases.current();
        if phase.phase() == EnderDragonPhase::Dying {
            return false;
        }
        let mut damage = phase.on_hurt(source, amount);
        if !matches!(part, PartKind::Head | PartKind::Neck) {
            damage = damage / 4.0 + damage.min(1.0);
        }
        if damage < 0.01 {
            return false;
        }
        let by_player = cause
            .or(source)
            .is_some_and(|attacker| attacker.get_player().is_some());
        if by_player || damage_type.has_tag(&tag::DamageType::MINECRAFT_ALWAYS_HURTS_ENDER_DRAGONS)
        {
            let living = &self.mob_entity.living_entity;
            let health_before = living.health.load();
            living.damage_with_context(self, damage, damage_type, position, source, cause);
            if self.phases.current().is_sitting() {
                let mut inner = lock(&self.inner);
                inner.sitting_damage_received += health_before - living.health.load();
                if inner.sitting_damage_received
                    > SITTING_ALLOWED_DAMAGE_PERCENTAGE * living.get_max_health()
                {
                    inner.sitting_damage_received = 0.0;
                    drop(inner);
                    self.set_phase(EnderDragonPhase::Takeoff);
                }
            }
        }
        true
    }

    /// Vanilla `onCrystalDestroyed`.
    pub fn on_crystal_destroyed(
        &self,
        crystal: &dyn EntityBase,
        crystal_pos: Vector3<f64>,
        attacker: Option<Arc<Player>>,
    ) {
        let world = self.world();
        let player = attacker.or_else(|| {
            world.get_nearest_player(crystal_pos, 64.0, |player| self.can_attack(player))
        });
        let is_nearest = lock(&self.inner)
            .nearest_crystal
            .as_ref()
            .is_some_and(|nearest| {
                nearest.get_entity().entity_id == crystal.get_entity().entity_id
            });
        if is_nearest {
            self.hurt_part(
                PartKind::Head,
                10.0,
                DamageType::EXPLOSION,
                Some(crystal_pos),
                Some(crystal),
                player.as_deref().map(|p| p as &dyn EntityBase),
            );
        }
        self.phases.current().on_crystal_destroyed(self, player);
    }

    /// Vanilla `tickDeath`, runs every tick while the health is zero.
    fn tick_death(&self) {
        let entity = self.get_entity();
        let world = self.world();
        self.update_fight(&world);
        let time = self.dragon_death_time.fetch_add(1, Ordering::Relaxed) + 1;
        let previously_killed = world
            .dragon_fight
            .as_ref()
            .and_then(|fight| fight.try_lock().ok())
            .is_some_and(|fight| fight.has_previously_killed_dragon());
        let xp_count: f32 = if self.has_fight() && !previously_killed {
            12000.0
        } else {
            500.0
        };
        let mob_drops = world.level_info.load().game_rules.mob_drops;
        let pos = entity.pos.load();
        if time > 150 && time % 5 == 0 && mob_drops {
            ExperienceOrbEntity::spawn(&world, pos, (xp_count * 0.08).floor() as u32);
        }
        if time == 1 && !entity.silent.load(Ordering::Relaxed) {
            world.sync_global_world_event(WorldEvent::SoundDragonDeath, entity.block_pos.load(), 0);
        }
        let lift = Vector3::new(0.0, f64::from(0.1f32), 0.0);
        entity.set_pos(pos + lift);
        for part in &self.parts {
            part.entity.set_pos(part.entity.pos.load() + lift);
        }
        if time >= DEATH_TICKS {
            if mob_drops {
                ExperienceOrbEntity::spawn(&world, pos, (xp_count * 0.2).floor() as u32);
            }
            if let Some(fight) = world.dragon_fight.as_ref() {
                let mut fight = lock(fight);
                if self.has_fight() {
                    fight.set_dragon_killed(&world, entity.entity_uuid);
                }
            }
            entity.remove();
        }
    }
}

impl Mob for EnderDragonEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.ai_step();
    }

    fn travel(&self, _caller: &dyn EntityBase) -> bool {
        true
    }

    fn get_mob_gravity(&self) -> f64 {
        0.0
    }

    fn requires_custom_persistence(&self) -> bool {
        true
    }

    fn get_base_experience_reward(&self) -> u32 {
        0
    }

    fn damage_override(
        &self,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> Option<bool> {
        Some(self.hurt_part(PartKind::Body, amount, damage_type, position, source, cause))
    }

    fn knockback_override(
        &self,
        _power: f64,
        _dx: f64,
        _dz: f64,
        _damage: f32,
        _attacker: Option<&dyn EntityBase>,
    ) -> bool {
        self.phases.current().is_sitting()
    }

    fn handle_killing_blow(&self) -> bool {
        if self.phases.current().is_sitting() {
            true
        } else {
            // the kill still counts for stats and advancements (LivingEntity.die awardKillScore)
            let living = &self.mob_entity.living_entity;
            let credit = living.get_kill_credit();
            living.update_death_stats(self, credit.as_deref());
            self.set_health_direct(1.0);
            self.set_phase(EnderDragonPhase::Dying);
            false
        }
    }

    fn custom_tick_death(&self) -> bool {
        self.tick_death();
        true
    }

    fn on_removed(&self) {
        let parts: Vec<Arc<dyn EntityBase>> = self
            .parts
            .iter()
            .map(|part| part.clone() as Arc<dyn EntityBase>)
            .collect();
        self.world().remove_sub_entities(&parts);
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int("DragonPhase", self.phases.current_type().id());
        nbt.put_int(
            "DragonDeathTime",
            self.dragon_death_time.load(Ordering::Relaxed),
        );
        nbt.put_float(
            "sitting_damage_received",
            lock(&self.inner).sitting_damage_received,
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(phase) = nbt.get_int("DragonPhase") {
            self.set_phase(EnderDragonPhase::from_id(phase));
        }
        self.dragon_death_time.store(
            nbt.get_int("DragonDeathTime").unwrap_or(0),
            Ordering::Relaxed,
        );
        lock(&self.inner).sitting_damage_received =
            nbt.get_float("sitting_damage_received").unwrap_or(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> [Node; NODE_COUNT] {
        let mut nodes = [Node::default(); NODE_COUNT];
        for (i, node) in nodes.iter_mut().enumerate() {
            *node = Node {
                x: i as i32 * 10,
                y: 80,
                z: 0,
            };
        }
        nodes
    }

    #[test]
    fn path_follows_the_adjacency_graph_to_the_end_node() {
        let nodes = ring();
        let path = find_path_between(&nodes, 0, 6, None, 0).expect("path");
        assert_eq!(path.nodes.first(), Some(&nodes[0]));
        assert_eq!(path.nodes.last(), Some(&nodes[6]));
        for pair in path.nodes.windows(2) {
            let a = nodes.iter().position(|n| n == &pair[0]).unwrap();
            let b = nodes.iter().position(|n| n == &pair[1]).unwrap();
            assert_ne!(NODE_ADJACENCY[a] & (1 << b), 0);
        }
    }

    #[test]
    fn final_node_is_appended_after_the_end_node() {
        let nodes = ring();
        let final_node = Node { x: 5, y: 70, z: 5 };
        let path = find_path_between(&nodes, 0, 6, Some(final_node), 0).expect("path");
        assert_eq!(path.nodes.last(), Some(&final_node));
        assert_eq!(path.nodes[path.nodes.len() - 2], nodes[6]);
    }

    #[test]
    fn inner_ring_only_when_no_crystal_is_alive() {
        let nodes = ring();
        let path = find_path_between(&nodes, 12, 15, None, 12).expect("path");
        assert!(path.nodes.iter().all(|node| node.x >= 120));
    }

    #[test]
    fn path_to_the_start_node_is_a_single_node() {
        let nodes = ring();
        let path = find_path_between(&nodes, 3, 3, None, 0).expect("path");
        assert_eq!(path.nodes.len(), 1);
    }
}
