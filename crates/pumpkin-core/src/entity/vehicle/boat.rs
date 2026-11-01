use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use crate::entity::living::is_allowed_by_team_rules;
use crate::entity::player::Player;
use crate::entity::vehicle::minecart::container::{self, MinecartInventory};
use crate::entity::vehicle::vehicle::VehicleEntity;
use crate::entity::{Entity, EntityBase, living::LivingEntity};
use crate::server::Server;
use crate::world::World;
use pumpkin_data::Block;
use pumpkin_data::damage::DamageType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;

const MAX_OUT_OF_CONTROL_TICKS: f32 = 60.0;
const BUBBLE_TIME: i32 = 60;
const GRAVITY: f64 = 0.04;
const PADDLE_STEP: f32 = std::f32::consts::PI / 8.0;
const CHEST_SIZE: usize = 27;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    InAir,
}

// boat: per-tick physics state (AbstractBoat fields)
struct BoatState {
    status: Status,
    old_status: Status,
    water_level: f64,
    land_friction: f32,
    last_yd: f64,
    out_of_control_ticks: f32,
    paddle_positions: [f32; 2],
    above_bubble_column: bool,
    bubble_column_down: bool,
}

pub struct BoatEntity {
    pub vehicle: VehicleEntity,
    state: Mutex<BoatState>,
    left_paddle_moving: AtomicBool,
    right_paddle_moving: AtomicBool,
    bubble_time: AtomicI32,
    chest: Option<Arc<MinecartInventory>>,
}

impl BoatEntity {
    pub fn new(mut entity: Entity) -> Self {
        entity.blocks_building = true;
        // chest boat: 27 slot container (AbstractChestBoat.CONTAINER_SIZE)
        let chest = entity
            .entity_type
            .resource_name
            .contains("chest")
            .then(|| Arc::new(MinecartInventory::new(CHEST_SIZE)));
        Self {
            vehicle: VehicleEntity::new(entity),
            state: Mutex::new(BoatState {
                status: Status::InAir,
                old_status: Status::InAir,
                water_level: 0.0,
                land_friction: 0.0,
                last_yd: 0.0,
                out_of_control_ticks: 0.0,
                paddle_positions: [0.0; 2],
                above_bubble_column: false,
                bubble_column_down: false,
            }),
            left_paddle_moving: AtomicBool::new(false),
            right_paddle_moving: AtomicBool::new(false),
            bubble_time: AtomicI32::new(0),
            chest,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, BoatState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_paddles(&self, left: bool, right: bool) {
        self.left_paddle_moving.store(left, Ordering::Relaxed);
        self.right_paddle_moving.store(right, Ordering::Relaxed);
        self.vehicle.entity.send_meta_data(
            &[
                Metadata::new(pumpkin_data::tracked_data::boat::ID_PADDLE_LEFT, left),
                Metadata::new(pumpkin_data::tracked_data::boat::ID_PADDLE_RIGHT, right),
            ],
            None,
        );
    }

    fn set_bubble_time(&self, time: i32) {
        if self.bubble_time.swap(time, Ordering::Relaxed) != time {
            self.vehicle.entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::boat::ID_BUBBLE_TIME,
                    pumpkin_protocol::codec::var_int::VarInt(time),
                )],
                None,
            );
        }
    }

    fn send_wobble_metadata(&self) {
        self.vehicle.send_wobble_metadata();
    }

    const fn max_passengers(&self) -> usize {
        if self.chest.is_some() { 1 } else { 2 }
    }

    fn passenger_count(&self) -> usize {
        self.vehicle
            .entity
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn first_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        self.vehicle
            .entity
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .cloned()
    }

    fn eject_passengers(&self) {
        let ids: Vec<i32> = self
            .vehicle
            .entity
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|passenger| passenger.get_entity().entity_id)
            .collect();
        for id in ids {
            self.vehicle.entity.remove_passenger(id);
        }
    }

    // boat: status from water and ground below (AbstractBoat.getStatus)
    fn get_status(&self, world: &World, state: &mut BoatState) -> Status {
        let bb = self.vehicle.entity.bounding_box.load();
        if let Some(status) = Self::is_underwater(world, &bb) {
            state.water_level = bb.max.y;
            return status;
        }
        if let Some(level) = Self::check_in_water(world, &bb) {
            state.water_level = level;
            return Status::InWater;
        }
        let friction = Self::ground_friction(world, &bb);
        if friction > 0.0 {
            state.land_friction = friction;
            Status::OnLand
        } else {
            Status::InAir
        }
    }

    fn water_height(world: &World, pos: &BlockPos) -> Option<(f32, bool)> {
        let (fluid, fluid_state) = world.get_fluid_and_fluid_state(pos);
        if fluid_state.is_empty || !fluid.has_tag(&tag::Fluid::MINECRAFT_WATER) {
            return None;
        }
        Some((
            world.get_fluid_height(pos, fluid, &fluid_state),
            fluid_state.is_source,
        ))
    }

    // boat: water surface the hull sits in (AbstractBoat.checkInWater)
    fn check_in_water(world: &World, bb: &BoundingBox) -> Option<f64> {
        let mut in_water = false;
        let mut water_level = -f64::MAX;
        for x in bb.min.x.floor() as i32..bb.max.x.ceil() as i32 {
            for y in bb.min.y.floor() as i32..(bb.min.y + 0.001).ceil() as i32 {
                for z in bb.min.z.floor() as i32..bb.max.z.ceil() as i32 {
                    if let Some((height, _)) = Self::water_height(world, &BlockPos::new(x, y, z)) {
                        let height = f64::from(y) + f64::from(height);
                        water_level = water_level.max(height);
                        in_water |= bb.min.y < height;
                    }
                }
            }
        }
        in_water.then_some(water_level)
    }

    // boat: top covered by water (AbstractBoat.isUnderwater)
    fn is_underwater(world: &World, bb: &BoundingBox) -> Option<Status> {
        let max_y = bb.max.y + 0.001;
        let mut under_water = false;
        for x in bb.min.x.floor() as i32..bb.max.x.ceil() as i32 {
            for y in bb.max.y.floor() as i32..max_y.ceil() as i32 {
                for z in bb.min.z.floor() as i32..bb.max.z.ceil() as i32 {
                    if let Some((height, source)) =
                        Self::water_height(world, &BlockPos::new(x, y, z))
                        && max_y < f64::from(y) + f64::from(height)
                    {
                        if !source {
                            return Some(Status::UnderFlowingWater);
                        }
                        under_water = true;
                    }
                }
            }
        }
        under_water.then_some(Status::UnderWater)
    }

    // boat: first non-full water layer above the hull (AbstractBoat.getWaterLevelAbove)
    fn water_level_above(world: &World, bb: &BoundingBox, last_yd: f64) -> f32 {
        let min_y = bb.max.y.floor() as i32;
        let max_y = (bb.max.y - last_yd).ceil() as i32;
        'layers: for y in min_y..max_y {
            let mut block_height = 0.0f32;
            for x in bb.min.x.floor() as i32..bb.max.x.ceil() as i32 {
                for z in bb.min.z.floor() as i32..bb.max.z.ceil() as i32 {
                    if let Some((height, _)) = Self::water_height(world, &BlockPos::new(x, y, z)) {
                        block_height = block_height.max(height);
                    }
                    if block_height >= 1.0 {
                        continue 'layers;
                    }
                }
            }
            if block_height < 1.0 {
                return y as f32 + block_height;
            }
        }
        (max_y + 1) as f32
    }

    // boat: mean friction of blocks under the hull, ice slides (AbstractBoat.getGroundFriction)
    fn ground_friction(world: &World, bb: &BoundingBox) -> f32 {
        let boat_box = BoundingBox::new(
            Vector3::new(bb.min.x, bb.min.y - 0.001, bb.min.z),
            Vector3::new(bb.max.x, bb.min.y, bb.max.z),
        );
        let x0 = boat_box.min.x.floor() as i32 - 1;
        let x1 = boat_box.max.x.ceil() as i32 + 1;
        let y0 = boat_box.min.y.floor() as i32 - 1;
        let y1 = boat_box.max.y.ceil() as i32 + 1;
        let z0 = boat_box.min.z.floor() as i32 - 1;
        let z1 = boat_box.max.z.ceil() as i32 + 1;
        let mut friction = 0.0f32;
        let mut count = 0u32;
        for x in x0..x1 {
            for z in z0..z1 {
                let edges = i32::from(x == x0 || x == x1 - 1) + i32::from(z == z0 || z == z1 - 1);
                if edges == 2 {
                    continue;
                }
                for y in y0..y1 {
                    if edges > 0 && (y == y0 || y == y1 - 1) {
                        continue;
                    }
                    let pos = BlockPos::new(x, y, z);
                    let (block, state) = world.get_block_and_state(&pos);
                    if block == &Block::LILY_PAD {
                        continue;
                    }
                    let offset = pos.0.to_f64();
                    if state
                        .get_block_collision_shapes()
                        .any(|shape| shape.shift(offset).intersects(&boat_box))
                    {
                        friction += block.slipperiness;
                        count += 1;
                    }
                }
            }
        }
        if count == 0 {
            0.0
        } else {
            friction / count as f32
        }
    }

    // boat: buoyancy and drag per status (AbstractBoat.floatBoat)
    fn float_boat(&self, world: &World, state: &mut BoatState, player_controlled: bool) {
        let entity = &self.vehicle.entity;
        let gravity = if entity.has_no_gravity() {
            0.0
        } else {
            GRAVITY
        };
        if state.old_status == Status::InAir
            && state.status != Status::InAir
            && state.status != Status::OnLand
        {
            let pos = entity.pos.load();
            state.water_level = pos.y + f64::from(entity.height());
            let bb = entity.bounding_box.load();
            let target_y = f64::from(Self::water_level_above(world, &bb, state.last_yd))
                - f64::from(entity.height())
                + 0.101;
            if world.is_space_empty(bb.shift(Vector3::new(0.0, target_y - pos.y, 0.0))) {
                entity.set_pos(Vector3::new(pos.x, target_y, pos.z));
                let velocity = entity.velocity.load();
                entity
                    .velocity
                    .store(Vector3::new(velocity.x, 0.0, velocity.z));
                state.last_yd = 0.0;
            }
            state.status = Status::InWater;
            return;
        }
        let (inv_friction, buoyancy, vspeed) = match state.status {
            Status::InWater => (
                0.9f32,
                (state.water_level - entity.pos.load().y) / f64::from(entity.height()),
                -gravity,
            ),
            Status::UnderFlowingWater => (0.9, 0.0, -7.0E-4),
            Status::UnderWater => (0.45, 0.01, -gravity),
            Status::InAir => (0.9, 0.0, -gravity),
            Status::OnLand => {
                let friction = state.land_friction;
                if player_controlled {
                    state.land_friction /= 2.0;
                }
                (friction, 0.0, -gravity)
            }
        };
        let velocity = entity.velocity.load();
        let mut velocity = Vector3::new(
            velocity.x * f64::from(inv_friction),
            velocity.y + vspeed,
            velocity.z * f64::from(inv_friction),
        );
        if buoyancy > 0.0 {
            velocity.y = (velocity.y + buoyancy * (GRAVITY / 0.65)) * 0.75;
        }
        entity.velocity.store(velocity);
    }

    // boat: paddle splash or knock on land per half turn (AbstractBoat.tick paddle sound)
    fn tick_paddles(&self, world: &World, state: &mut BoatState, has_controller: bool) {
        let entity = &self.vehicle.entity;
        let sound = match state.status {
            Status::InWater | Status::UnderWater | Status::UnderFlowingWater => {
                Some(Sound::EntityBoatPaddleWater)
            }
            Status::OnLand => Some(Sound::EntityBoatPaddleLand),
            Status::InAir => None,
        };
        let paddles = [
            self.left_paddle_moving.load(Ordering::Relaxed),
            self.right_paddle_moving.load(Ordering::Relaxed),
        ];
        for (side, moving) in paddles.into_iter().enumerate() {
            if !(moving && has_controller) {
                state.paddle_positions[side] = 0.0;
                continue;
            }
            let position = state.paddle_positions[side];
            let quarter = std::f32::consts::FRAC_PI_4;
            if !entity.is_silent()
                && position % std::f32::consts::TAU <= quarter
                && (position + PADDLE_STEP) % std::f32::consts::TAU >= quarter
                && let Some(sound) = sound
            {
                let view = entity.rotation();
                let (dx, dz) = if side == 1 {
                    (-view.z, view.x)
                } else {
                    (view.z, -view.x)
                };
                let pos = entity.pos.load();
                world.play_sound_fine(
                    sound,
                    SoundCategory::Neutral,
                    &Vector3::new(pos.x + f64::from(dx), pos.y, pos.z + f64::from(dz)),
                    1.0,
                    0.4f32.mul_add(rand::random::<f32>(), 0.8),
                );
            }
            state.paddle_positions[side] = position + PADDLE_STEP;
        }
    }

    // boat: delayed bubble column launch or pull under (AbstractBoat.tickBubbleColumn)
    fn tick_bubble_column(&self, state: &mut BoatState) {
        if !state.above_bubble_column {
            self.set_bubble_time(0);
        }
        let bubble_time = self.bubble_time.load(Ordering::Relaxed);
        if bubble_time <= 0 {
            return;
        }
        let bubble_time = bubble_time - 1;
        self.set_bubble_time(bubble_time);
        let diff = BUBBLE_TIME - bubble_time - 1;
        if diff > 0 && bubble_time == 0 {
            let entity = &self.vehicle.entity;
            let world = entity.world.load();
            let velocity = entity.velocity.load();
            if state.bubble_column_down {
                self.eject_passengers();
                world.send_entity_status(
                    entity,
                    pumpkin_data::entity::EntityStatus::BoatSink,
                    pumpkin_protocol::bedrock::server::ActorEventID::None,
                );
                entity.velocity.store(velocity.add_raw(0.0, -0.7, 0.0));
            } else {
                world.send_entity_status(
                    entity,
                    pumpkin_data::entity::EntityStatus::BoatLaunch,
                    pumpkin_protocol::bedrock::server::ActorEventID::None,
                );
                entity
                    .velocity
                    .store(Vector3::new(velocity.x, 0.6, velocity.z));
            }
        }
        state.above_bubble_column = false;
    }

    // boat: small living entities hop in, others get pushed (AbstractBoat.tick)
    fn pick_up_or_push(&self, world: &Arc<World>, player_controlled: bool) {
        let entity = &self.vehicle.entity;
        let search = entity.bounding_box.load().expand(0.2, -0.01, 0.2);
        let own_team = (self as &dyn EntityBase).get_team();
        for other in world.get_all_at_box(&search) {
            let other_entity = other.get_entity();
            if other_entity.entity_id == entity.entity_id
                || other.is_spectator()
                || !other.is_pushable()
                || !is_allowed_by_team_rules(own_team.as_ref(), other.get_team().as_ref())
                || other_entity.has_passenger(entity.entity_id)
            {
                continue;
            }
            if !player_controlled
                && self.passenger_count() < self.max_passengers()
                && !other.is_passenger()
                && other_entity.width() < entity.width()
                && other.get_living_entity().is_some()
                && !other_entity
                    .entity_type
                    .has_tag(&tag::EntityType::MINECRAFT_CANNOT_BE_PUSHED_ONTO_BOATS)
                && self.can_add_passenger()
            {
                if let Some(vehicle) = world.get_entity_by_id(entity.entity_id) {
                    entity.add_passenger(vehicle, other);
                }
            } else {
                self.push(other.as_ref());
            }
        }
    }

    // boat: room left and not sunk (AbstractBoat.canAddPassenger)
    fn can_add_passenger(&self) -> bool {
        self.passenger_count() < self.max_passengers()
            && !self.vehicle.entity.is_submerged_in_water()
    }

    // boat: mobs face the boat, animals sideways when full (AbstractBoat.clampRotation)
    fn clamp_mob_passengers(&self) {
        let entity = &self.vehicle.entity;
        let passengers: Vec<Arc<dyn EntityBase>> = {
            let passengers = entity
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if passengers.iter().all(|p| p.get_mob().is_none()) {
                return;
            }
            passengers.clone()
        };
        let full = passengers.len() == self.max_passengers();
        let yaw = entity.yaw.load();
        for passenger in &passengers {
            let passenger_entity = passenger.get_entity();
            if passenger.get_mob().is_none()
                || passenger_entity
                    .entity_type
                    .has_tag(&tag::EntityType::MINECRAFT_CAN_TURN_IN_BOATS)
            {
                continue;
            }
            let is_animal = passenger
                .get_mob()
                .is_some_and(|mob| mob.as_ageable().is_some())
                && passenger_entity.entity_type.category
                    == &pumpkin_data::entity::MobCategory::CREATURE;
            let body_yaw = if is_animal && full {
                yaw + if passenger_entity.entity_id % 2 == 0 {
                    90.0
                } else {
                    270.0
                }
            } else {
                yaw
            };
            passenger_entity.set_rotation(body_yaw, passenger_entity.pitch.load());
            passenger_entity.head_yaw.store(body_yaw);
            passenger_entity.body_yaw.store(body_yaw);
        }
    }

    fn drop_item(&self) -> Option<&'static pumpkin_data::item::Item> {
        pumpkin_data::item::Item::from_registry_key(self.vehicle.entity.entity_type.resource_name)
    }

    // chest boat: open inventory (AbstractChestBoat.interactWithContainerVehicle)
    pub fn open_chest(&self, player: &Arc<Player>) -> bool {
        let Some(chest) = &self.chest else {
            return false;
        };
        let entity = &self.vehicle.entity;
        container::open(
            entity,
            player,
            chest,
            TextComponent::translate(
                format!("entity.minecraft.{}", entity.entity_type.resource_name),
                [],
            ),
            false,
        )
    }

    // vehicle: break into its item, chest contents spill (VehicleEntity.destroy, chestVehicleDestroyed)
    fn destroy(&self, source: Option<&dyn EntityBase>) {
        let entity = &self.vehicle.entity;
        let world = entity.world.load();
        entity.remove();
        if !world.level_info.load().game_rules.entity_drops {
            return;
        }
        if let Some(item) = self.drop_item() {
            let mut stack = ItemStack::new(1, item);
            if let Some(name) = entity.custom_name.load().as_ref().clone() {
                stack
                    .set_data_component(pumpkin_data::data_component_impl::CustomNameImpl { name });
            }
            entity.spawn_at_location(stack);
        }
        if let Some(chest) = &self.chest
            && chest.claim_drops()
        {
            chest.unpack_loot(&crate::world::loot::LootContextParameters {
                world: Some(&world),
                position: Some(entity.pos.load()),
                ..Default::default()
            });
            let inventory: Arc<dyn pumpkin_inventory::Inventory> = chest.clone();
            world.scatter_inventory(&entity.block_pos.load(), &inventory);
            container::anger_piglins_on_destroy(source);
        }
    }
}

impl EntityBase for BoatEntity {
    fn get_entity(&self) -> &Entity {
        &self.vehicle.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    // boat: status, drift, pick up riders (AbstractBoat.tick)
    fn tick(&self, caller: &dyn EntityBase, server: &Server) {
        let entity = &self.vehicle.entity;
        let world = entity.world.load_full();
        let out_of_control = {
            let mut state = self.lock_state();
            state.old_status = state.status;
            state.status = self.get_status(&world, &mut state);
            if matches!(state.status, Status::UnderWater | Status::UnderFlowingWater) {
                state.out_of_control_ticks += 1.0;
            } else {
                state.out_of_control_ticks = 0.0;
            }
            state.out_of_control_ticks
        };
        if out_of_control >= MAX_OUT_OF_CONTROL_TICKS {
            self.eject_passengers();
        }

        self.vehicle.tick();
        entity.tick(caller, server);

        let player_controlled = self
            .first_passenger()
            .is_some_and(|passenger| passenger.get_player().is_some());
        let has_controller = self.first_passenger().is_some();
        let mut state = self.lock_state();
        if player_controlled {
            entity.velocity.store(Vector3::default());
        } else {
            if self.left_paddle_moving.load(Ordering::Relaxed)
                || self.right_paddle_moving.load(Ordering::Relaxed)
            {
                self.set_paddles(false, false);
            }
            self.float_boat(&world, &mut state, player_controlled);
            let velocity = entity.velocity.load();
            state.last_yd = velocity.y;
            entity.move_entity(caller, velocity);
        }
        self.tick_bubble_column(&mut state);
        self.tick_paddles(&world, &mut state, has_controller);
        drop(state);

        self.pick_up_or_push(&world, player_controlled);
        self.clamp_mob_passengers();
    }

    fn init_data_tracker(&self) {
        self.send_wobble_metadata();
    }

    fn can_hit(&self) -> bool {
        self.vehicle.entity.is_alive()
    }

    fn is_collidable(&self, _entity: Option<Box<dyn EntityBase>>) -> bool {
        true
    }

    // boat: bubble column effect waits 60 ticks (AbstractBoat.onAboveBubbleColumn)
    fn on_above_bubble_column(&self, drag_down: bool) -> bool {
        let mut state = self.lock_state();
        state.above_bubble_column = true;
        state.bubble_column_down = drag_down;
        if self.bubble_time.load(Ordering::Relaxed) == 0 {
            self.set_bubble_time(BUBBLE_TIME);
        }
        let under_water = matches!(state.status, Status::UnderWater | Status::UnderFlowingWater);
        drop(state);
        if !under_water && rand::random_range(0..100) == 0 {
            let entity = &self.vehicle.entity;
            let world = entity.world.load();
            let pos = entity.pos.load();
            world.play_sound_fine(
                Sound::EntityGenericSplash,
                SoundCategory::Neutral,
                &pos,
                1.0,
                0.4f32.mul_add(rand::random::<f32>(), 0.8),
            );
            world.emit_game_event(pumpkin_data::game_event::GameEvent::Splash.name(), pos);
        }
        true
    }

    // boat: other boats only from below their top, others from below the hull (AbstractBoat.push)
    fn push(&self, entity: &dyn EntityBase) {
        let own_box = self.vehicle.entity.bounding_box.load();
        let other_box = entity.get_entity().bounding_box.load();
        let is_boat = (entity.cast_any()).is::<Self>();
        if (is_boat && other_box.min.y < own_box.max.y)
            || (!is_boat && other_box.min.y <= own_box.min.y)
        {
            crate::entity::default_push(self, entity);
        }
    }

    // vehicle: wobble, break past 40 damage, creative removes (VehicleEntity.hurtServer)
    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let entity = &self.vehicle.entity;
        if entity.is_removed() {
            return true;
        }
        if entity.is_invulnerable_to(&damage_type, cause.or(source)) {
            return false;
        }
        let attacker = cause.or(source);
        let attacker_id = attacker.map(|s| s.get_entity().entity_id);
        let world = entity.world.load();
        if let Some(server) = world.server.upgrade() {
            let mut event =
                crate::plugin::api::events::vehicle::vehicle_damage::VehicleDamageEvent::new(
                    entity.entity_id,
                    amount,
                    attacker_id,
                );
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return false;
            }
        }
        let damage = self.vehicle.apply_damage_wobble(amount);
        world.emit_game_event(
            pumpkin_data::game_event::GameEvent::EntityDamage.name(),
            entity.pos.load(),
        );
        let creative = attacker
            .and_then(EntityBase::get_player)
            .is_some_and(Player::is_creative);
        if creative || damage > 40.0 {
            if let Some(server) = world.server.upgrade() {
                let mut event =
                    crate::plugin::api::events::vehicle::vehicle_destroy::VehicleDestroyEvent::new(
                        entity.entity_id,
                        attacker_id,
                    );
                server.plugin_manager.fire_blocking(&server, &mut event);
                if event.cancelled {
                    return true;
                }
            }
            if creative {
                entity.remove();
            } else {
                self.destroy(source);
            }
        }
        true
    }

    // boat: board unless sneaking or sunk, chest boats open when full or sneaking (AbstractBoat.interact)
    fn interact(&self, player: &Arc<Player>, _item_stack: &mut ItemStack) -> bool {
        let entity = &self.vehicle.entity;
        let sneaking = player.get_entity().is_sneaking();
        let out_of_control = self.lock_state().out_of_control_ticks;
        if !sneaking
            && out_of_control < MAX_OUT_OF_CONTROL_TICKS
            && self.can_add_passenger()
            && !player.get_entity().has_vehicle()
        {
            let world = entity.world.load();
            if let (Some(vehicle), Some(passenger)) = (
                world.get_entity_by_id(entity.entity_id),
                world.get_player_by_id(player.entity_id()),
            ) {
                entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
                return true;
            }
        }
        if self.chest.is_some() && (sneaking || !self.can_add_passenger()) {
            return self.open_chest(player);
        }
        false
    }

    fn set_paddle_state(&self, left: bool, right: bool) {
        self.set_paddles(left, right);
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        if let Some(chest) = &self.chest {
            chest.write_nbt(nbt);
        }
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(chest) = &self.chest {
            chest.read_nbt(nbt);
        }
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_pushable(&self) -> bool {
        true
    }
}
