use crate::entity::projectile::entity_ray_intersection;
use crate::entity::projectile::owner::ProjectileOwner;
use crate::world::World;
use pumpkin_data::Block;
use pumpkin_data::item::Item;
use pumpkin_data::particle::Particle;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use crate::entity::experience_orb::ExperienceOrbEntity;
use crate::entity::item::ItemEntity;
use crate::entity::projectile::{ProjectileHit, is_projectile};
use crate::world::loot::{LootContextParameters, LootTableHandle, generate_loot_from_handle};
use crate::{
    entity::{Entity, EntityBase, living::LivingEntity, player::Player},
    server::Server,
};
use pumpkin_data::attributes::Attributes;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_protocol::bedrock::server::ActorEventID;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

pub struct FishingBobberEntity {
    pub entity: Entity,
    pub owner: ProjectileOwner,
    pub hooked_entity_id: AtomicI32,
    state: Mutex<FishingState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookState {
    Flying,
    HookedInEntity,
    Bobbing,
}

// fishing hook: per-hook timers (FishingHook fields)
struct FishingState {
    hook_state: HookState,
    life: i32,
    out_of_water_time: i32,
    nibble: i32,
    time_until_lured: i32,
    time_until_hooked: i32,
    fish_angle: f32,
    biting: bool,
    open_water: bool,
    lure_speed: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenWaterType {
    AboveWater,
    InsideWater,
    Invalid,
}

const NO_HOOKED_ENTITY: i32 = -1;

impl FishingBobberEntity {
    const GRAVITY: f64 = 0.03;
    const INERTIA: f64 = 0.92;
    const MAX_LIFE: i32 = 1200;
    const MAX_OUT_OF_WATER_TIME: i32 = 10;
    const MAX_RANGE_SQ: f64 = 1024.0;

    pub fn new(entity: Entity, owner: &Player) -> Self {
        Self::new_with_rod(entity, owner, 0)
    }

    pub fn new_with_rod(entity: Entity, owner: &Player, lure_speed: i32) -> Self {
        let mut owner_pos = owner.living_entity.entity.pos.load();
        owner_pos.y += owner.living_entity.entity.get_eye_height() - 0.1;
        entity.pos.store(owner_pos);

        Self {
            entity,
            owner: ProjectileOwner::new(Some(&owner.living_entity.entity)),
            hooked_entity_id: AtomicI32::new(NO_HOOKED_ENTITY),
            state: Mutex::new(FishingState {
                hook_state: HookState::Flying,
                life: 0,
                out_of_water_time: 0,
                nibble: 0,
                time_until_lured: 0,
                time_until_hooked: 0,
                fish_angle: 0.0,
                biting: false,
                open_water: true,
                lure_speed: lure_speed.max(0),
            }),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, FishingState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // fishing hook: loot predicate in_open_water (FishingHook.isOpenWaterFishing)
    pub fn is_open_water_fishing(&self) -> bool {
        self.lock_state().open_water
    }

    fn owner_player(&self, world: &World) -> Option<Arc<Player>> {
        let owner = self.owner.get(world)?;
        world.get_player_by_id(owner.get_entity().entity_id)
    }

    fn hooked_entity(&self, world: &World) -> Option<Arc<dyn EntityBase>> {
        let id = self.hooked_entity_id.load(Ordering::Relaxed);
        if id == NO_HOOKED_ENTITY {
            return None;
        }
        world.get_entity_by_id(id).or_else(|| {
            world
                .get_player_by_id(id)
                .map(|player| player as Arc<dyn EntityBase>)
        })
    }

    fn set_hooked_entity(&self, hooked: Option<&Entity>) {
        let id = hooked.map_or(NO_HOOKED_ENTITY, |hooked| hooked.entity_id);
        self.hooked_entity_id.store(id, Ordering::Relaxed);
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::fishing_bobber::DATA_HOOKED_ENTITY,
            VarInt(hooked.map_or(0, |hooked| hooked.entity_id + 1)),
        );
    }

    fn set_biting(&self, biting: bool) {
        self.entity.set_synced_data(
            pumpkin_data::tracked_data::fishing_bobber::DATA_BITING,
            biting,
        );
    }

    // fishing hook: discard and clear the owner's hook (FishingHook.onRemoval)
    fn discard(&self, owner: Option<&Player>) {
        if let Some(owner) = owner {
            let _ = owner.fishing_bobber.compare_exchange(
                self.entity.entity_id,
                -1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
        self.entity.remove();
    }

    // fishing hook: line breaks without a rod in hand or beyond 32 blocks (FishingHook.shouldStopFishing)
    fn should_stop_fishing(&self, owner: &Player) -> bool {
        if owner.living_entity.health.load() > 0.0 && !owner.living_entity.entity.is_removed() {
            let inventory = owner.inventory();
            let holds_rod = inventory.get_stack_in_hand(Hand::Right).item == &Item::FISHING_ROD
                || inventory.get_stack_in_hand(Hand::Left).item == &Item::FISHING_ROD;
            if holds_rod
                && self
                    .entity
                    .pos
                    .load()
                    .squared_distance_to_vec(&owner.living_entity.entity.pos.load())
                    <= Self::MAX_RANGE_SQ
            {
                return false;
            }
        }
        self.discard(Some(owner));
        true
    }

    // fishing hook: reel in, pull hooked entity or roll fishing loot (FishingHook.retrieve)
    pub fn reel_in(&self, player: &Player, rod: &ItemStack) -> i32 {
        if self.should_stop_fishing(player) {
            return 0;
        }
        let world = self.entity.world.load();
        let mut dmg = 0;
        let bobber_pos = self.entity.pos.load();
        let player_pos = player.get_entity().pos.load();
        let nibble = self.lock_state().nibble;

        if let Some(hooked) = self.hooked_entity(&world) {
            // fishing hook: pull towards owner (FishingHook.pullEntity)
            let delta = (player_pos - bobber_pos).multiply(0.1, 0.1, 0.1);
            hooked.get_entity().add_velocity(delta);
            world.send_entity_status(
                &self.entity,
                EntityStatus::FishingRodReelIn,
                ActorEventID::FishhookHookTime,
            );
            dmg = if hooked.get_entity().entity_type == &EntityType::ITEM {
                3
            } else {
                5
            };
        } else if nibble > 0 {
            let luck =
                crate::enchantment::helper::EnchantmentHelper::modify_fishing_luck_bonus(rod, 0.0)
                    + player.living_entity.get_attribute_value(&Attributes::LUCK) as f32;
            let params = LootContextParameters {
                position: Some(bobber_pos),
                tool: Some(rod.clone()),
                this_entity: Some(self),
                world: Some(&world),
                luck,
                ..Default::default()
            };
            let handle = world
                .server
                .upgrade()
                .and_then(|s| {
                    s.datapack_manager
                        .get_loot_table("minecraft:gameplay/fishing")
                })
                .unwrap_or(LootTableHandle::Static(
                    &pumpkin_data::loot_table::GAMEPLAY_FISHING,
                ));
            let items = generate_loot_from_handle(&handle, 0, &params);
            let delta = player_pos - bobber_pos;
            let vy = delta.y * 0.1 + delta.length().sqrt() * 0.08;

            for item_stack in items {
                if item_stack.is_empty() {
                    continue;
                }
                let is_fish = item_stack
                    .item
                    .has_tag(&pumpkin_data::tag::Item::MINECRAFT_FISHES);
                player.trigger_advancement(
                    crate::entity::player::advancement::trigger::AdvancementTrigger::FishedItem {
                        item_id: format!("minecraft:{}", item_stack.item.registry_key),
                    },
                );
                let item_entity = ItemEntity::new_with_velocity(
                    Entity::new(world.clone(), bobber_pos, &EntityType::ITEM),
                    item_stack,
                    Vector3::new(delta.x * 0.1, vy, delta.z * 0.1),
                    0,
                );
                world.spawn_entity(Arc::new(item_entity));
                let xp_amount = rand::rng().random_range(1..=6);
                ExperienceOrbEntity::spawn(&world, player_pos.add_raw(0.0, 0.5, 0.5), xp_amount);
                if is_fish {
                    player.increment_stat(
                        pumpkin_data::statistic::StatisticCategory::Custom,
                        pumpkin_data::statistic::CustomStatistic::FishCaught as i32,
                        1,
                    );
                }
            }
            dmg = 1;
        }

        if self.entity.on_ground.load(Ordering::Relaxed) {
            dmg = 2;
        }

        self.discard(Some(player));
        dmg
    }

    // fishing hook: state machine, bobbing and catching (FishingHook.tick)
    #[expect(clippy::too_many_lines, reason = "mirrors FishingHook.tick")]
    pub fn process_tick(&self, caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let world = entity.world.load();
        self.owner.check_left_owner(entity);

        let Some(owner) = self.owner_player(&world) else {
            self.discard(None);
            return;
        };
        if self.should_stop_fishing(&owner) {
            return;
        }

        let mut state = self.lock_state();
        if entity.on_ground.load(Ordering::Relaxed) {
            state.life += 1;
            if state.life >= Self::MAX_LIFE {
                drop(state);
                self.discard(Some(&owner));
                return;
            }
        } else {
            state.life = 0;
        }

        let block_pos = entity.block_pos.load();
        let (fluid, fluid_state) = world.get_fluid_and_fluid_state(&block_pos);
        let is_water_fluid = fluid.has_tag(&pumpkin_data::tag::Fluid::MINECRAFT_WATER);
        let liquid_height = if is_water_fluid {
            world.get_fluid_height(&block_pos, fluid, &fluid_state)
        } else {
            0.0
        };
        let in_water = liquid_height > 0.0;
        let hooked = self.hooked_entity(&world);

        match state.hook_state {
            HookState::Flying => {
                if hooked.is_some() {
                    entity.velocity.store(Vector3::default());
                    state.hook_state = HookState::HookedInEntity;
                    return;
                }
                if in_water {
                    entity
                        .velocity
                        .store(entity.velocity.load().multiply(0.3, 0.2, 0.3));
                    state.hook_state = HookState::Bobbing;
                    return;
                }
                self.check_collision(&world, entity, caller);
            }
            HookState::HookedInEntity => {
                if self.hooked_entity_id.load(Ordering::Relaxed) != NO_HOOKED_ENTITY {
                    if let Some(hooked) = hooked.filter(|hooked| hooked.get_entity().is_alive()) {
                        let hooked_entity = hooked.get_entity();
                        let mut pos = hooked_entity.pos.load();
                        pos.y += f64::from(hooked_entity.height()) * 0.8;
                        entity.set_pos(pos);
                    } else {
                        self.set_hooked_entity(None);
                        state.hook_state = HookState::Flying;
                    }
                }
                return;
            }
            HookState::Bobbing => {
                let velocity = entity.velocity.load();
                let mut force = entity.pos.load().y + velocity.y
                    - f64::from(block_pos.0.y)
                    - f64::from(liquid_height);
                if force.abs() < 0.01 {
                    force += force.signum() * 0.1;
                }
                entity.velocity.store(Vector3::new(
                    velocity.x * 0.9,
                    velocity.y - force * f64::from(rand::random::<f32>()) * 0.2,
                    velocity.z * 0.9,
                ));
                state.open_water = if state.nibble <= 0 && state.time_until_hooked <= 0 {
                    true
                } else {
                    state.open_water
                        && state.out_of_water_time < Self::MAX_OUT_OF_WATER_TIME
                        && Self::calculate_open_water(&world, &block_pos)
                };
                if in_water {
                    state.out_of_water_time = (state.out_of_water_time - 1).max(0);
                    if state.biting {
                        let dip = -0.1 * f64::from(rand::random::<f32>() * rand::random::<f32>());
                        entity
                            .velocity
                            .store(entity.velocity.load().add_raw(0.0, dip, 0.0));
                    }
                    self.catching_fish(&mut state, &world, &block_pos);
                } else {
                    state.out_of_water_time =
                        (state.out_of_water_time + 1).min(Self::MAX_OUT_OF_WATER_TIME);
                }
            }
        }
        let hook_state = state.hook_state;
        drop(state);

        if !is_water_fluid
            && !entity.on_ground.load(Ordering::Relaxed)
            && self.hooked_entity_id.load(Ordering::Relaxed) == NO_HOOKED_ENTITY
        {
            entity
                .velocity
                .store(entity.velocity.load().add_raw(0.0, -Self::GRAVITY, 0.0));
        }

        entity.move_entity(caller, entity.velocity.load());
        if hook_state == HookState::Flying
            && (entity.on_ground.load(Ordering::Relaxed)
                || entity.horizontal_collision.load(Ordering::Relaxed))
        {
            entity.velocity.store(Vector3::default());
        }
        entity.velocity.store(entity.velocity.load().multiply(
            Self::INERTIA,
            Self::INERTIA,
            Self::INERTIA,
        ));
    }

    // fishing hook: hit entity or shorten the move to the hit block (FishingHook.checkCollision)
    fn check_collision(&self, world: &Arc<World>, entity: &Entity, caller: &dyn EntityBase) {
        let start = entity.pos.load();
        let velocity = entity.velocity.load();
        let end = start.add(&velocity);
        let block_hit = world.clip(&pumpkin_world::clip::ClipContext::new(
            start,
            end,
            pumpkin_world::clip::ClipBlock::Collider,
            pumpkin_world::clip::ClipFluid::None,
        ));
        let reach = if block_hit.is_miss {
            velocity
        } else {
            block_hit.location - start
        };

        let search_box = entity
            .bounding_box
            .load()
            .expand_towards(reach.x, reach.y, reach.z)
            .expand(1.0, 1.0, 1.0);
        let mut closest: Option<(f64, Arc<dyn EntityBase>)> = None;
        for candidate in world.get_all_at_box(&search_box) {
            let candidate_entity = candidate.get_entity();
            if candidate_entity.entity_id == entity.entity_id
                || candidate.is_spectator()
                || !candidate_entity.is_alive()
                || is_projectile(candidate_entity.entity_type)
                || !(self.owner.can_hit(world, candidate_entity)
                    || candidate_entity.entity_type == &EntityType::ITEM)
            {
                continue;
            }
            let hitbox = candidate_entity.bounding_box.load().expand(0.3, 0.3, 0.3);
            if let Some(t) = entity_ray_intersection(&start, &reach, &hitbox, candidate.as_ref())
                && closest.as_ref().is_none_or(|(best, _)| t < *best)
            {
                closest = Some((t, candidate));
            }
        }

        if let Some((_, hooked)) = closest {
            // projectile: Projectile.hitTargetOrDeflectSelf
            if crate::entity::projectile_deflection::try_deflect(caller, hooked.as_ref()) {
                return;
            }
            self.set_hooked_entity(Some(hooked.get_entity()));
        } else if !block_hit.is_miss {
            entity.velocity.store(reach);
        }
    }

    // fishing hook: lure, approach and bite timers (FishingHook.catchingFish)
    #[expect(clippy::too_many_lines, reason = "mirrors FishingHook.catchingFish")]
    fn catching_fish(&self, state: &mut FishingState, world: &Arc<World>, block_pos: &BlockPos) {
        let entity = &self.entity;
        let mut rng = rand::rng();
        let mut fishing_speed = 1;
        let above = block_pos.up();
        if rng.random::<f32>() < 0.25 && world.is_raining_at(&above) {
            fishing_speed += 1;
        }
        if rng.random::<f32>() < 0.5 && !world.can_see_sky(&above) {
            fishing_speed -= 1;
        }

        let pos = entity.pos.load();
        if state.nibble > 0 {
            state.nibble -= 1;
            if state.nibble <= 0 {
                state.time_until_lured = 0;
                state.time_until_hooked = 0;
                state.biting = false;
                self.set_biting(false);
            }
        } else if state.time_until_hooked > 0 {
            state.time_until_hooked -= fishing_speed;
            if state.time_until_hooked > 0 {
                state.fish_angle += (9.188 * (rng.random::<f64>() - rng.random::<f64>())) as f32;
                let angle = state.fish_angle.to_radians();
                let (angle_sin, angle_cos) = (angle.sin(), angle.cos());
                let fish_x = pos.x + f64::from(angle_sin * state.time_until_hooked as f32 * 0.1);
                let fish_y = pos.y.floor() + 1.0;
                let fish_z = pos.z + f64::from(angle_cos * state.time_until_hooked as f32 * 0.1);
                let splash_pos = BlockPos::containing(fish_x, fish_y - 1.0, fish_z);
                if world.get_block(&splash_pos) == &Block::WATER {
                    if rng.random::<f32>() < 0.15 {
                        world.spawn_particle(
                            Vector3::new(fish_x, fish_y - 0.1, fish_z),
                            Vector3::new(angle_sin, 0.1, angle_cos),
                            0.0,
                            1,
                            Particle::Bubble,
                        );
                    }
                    let particle_x = angle_sin * 0.04;
                    let particle_z = angle_cos * 0.04;
                    let fish_pos = Vector3::new(fish_x, fish_y, fish_z);
                    world.spawn_particle(
                        fish_pos,
                        Vector3::new(particle_z, 0.01, -particle_x),
                        1.0,
                        0,
                        Particle::Fishing,
                    );
                    world.spawn_particle(
                        fish_pos,
                        Vector3::new(-particle_z, 0.01, particle_x),
                        1.0,
                        0,
                        Particle::Fishing,
                    );
                }
            } else {
                let pitch = 1.0 + (rng.random::<f32>() - rng.random::<f32>()) * 0.4;
                world.play_sound_fine(
                    Sound::EntityFishingBobberSplash,
                    SoundCategory::Neutral,
                    &pos,
                    0.25,
                    pitch,
                );
                let width = entity.width();
                let count = (1.0 + width * 20.0) as i32;
                let splash = pos.add_raw(0.0, 0.5, 0.0);
                let spread = Vector3::new(width, 0.0, width);
                world.spawn_particle(splash, spread, 0.2, count, Particle::Bubble);
                world.spawn_particle(splash, spread, 0.2, count, Particle::Fishing);
                state.nibble = rng.random_range(20..=40);
                state.biting = true;
                self.set_biting(true);
            }
        } else if state.time_until_lured > 0 {
            state.time_until_lured -= fishing_speed;
            let mut tease_chance = 0.15;
            if state.time_until_lured < 20 {
                tease_chance += (20 - state.time_until_lured) as f32 * 0.05;
            } else if state.time_until_lured < 40 {
                tease_chance += (40 - state.time_until_lured) as f32 * 0.02;
            } else if state.time_until_lured < 60 {
                tease_chance += (60 - state.time_until_lured) as f32 * 0.01;
            }
            if rng.random::<f32>() < tease_chance {
                let angle = rng.random_range(0.0f32..360.0).to_radians();
                let dist = rng.random_range(25.0f32..60.0);
                let fish_x = pos.x + f64::from(angle.sin() * dist) * 0.1;
                let fish_y = pos.y.floor() + 1.0;
                let fish_z = pos.z + f64::from(angle.cos() * dist) * 0.1;
                let splash_pos = BlockPos::containing(fish_x, fish_y - 1.0, fish_z);
                if world.get_block(&splash_pos) == &Block::WATER {
                    world.spawn_particle(
                        Vector3::new(fish_x, fish_y, fish_z),
                        Vector3::new(0.1, 0.0, 0.1),
                        0.0,
                        2 + rng.random_range(0..2),
                        Particle::Splash,
                    );
                }
            }
            if state.time_until_lured <= 0 {
                state.fish_angle = rng.random_range(0.0f32..360.0);
                state.time_until_hooked = rng.random_range(20..=80);
            }
        } else {
            state.time_until_lured = rng.random_range(100..=600) - state.lure_speed;
        }
    }

    // fishing hook: 5x4x5 layers of source water under air (FishingHook.calculateOpenWater)
    fn calculate_open_water(world: &World, block_pos: &BlockPos) -> bool {
        let mut previous = OpenWaterType::Invalid;
        for y in -1..=2 {
            let layer = Self::open_water_type_for_layer(world, block_pos, y);
            match layer {
                OpenWaterType::AboveWater if previous == OpenWaterType::Invalid => return false,
                OpenWaterType::InsideWater if previous == OpenWaterType::AboveWater => {
                    return false;
                }
                OpenWaterType::Invalid => return false,
                _ => {}
            }
            previous = layer;
        }
        true
    }

    fn open_water_type_for_layer(world: &World, center: &BlockPos, y: i32) -> OpenWaterType {
        let mut layer = None;
        for x in -2..=2 {
            for z in -2..=2 {
                let block_type =
                    Self::open_water_type_for_block(world, &center.offset(Vector3::new(x, y, z)));
                match layer {
                    None => layer = Some(block_type),
                    Some(current) if current != block_type => return OpenWaterType::Invalid,
                    _ => {}
                }
            }
        }
        layer.unwrap_or(OpenWaterType::Invalid)
    }

    fn open_water_type_for_block(world: &World, pos: &BlockPos) -> OpenWaterType {
        let state = world.get_block_state(pos);
        let block = Block::from_state_id(state.id);
        if state.is_air() || block == &Block::LILY_PAD {
            return OpenWaterType::AboveWater;
        }
        let (fluid, fluid_state) = world.get_fluid_and_fluid_state(pos);
        if fluid.has_tag(&pumpkin_data::tag::Fluid::MINECRAFT_WATER)
            && fluid_state.is_source
            && state.get_block_collision_shapes().next().is_none()
        {
            OpenWaterType::InsideWater
        } else {
            OpenWaterType::Invalid
        }
    }
}

impl EntityBase for FishingBobberEntity {
    fn get_projectile_owner(&self) -> Option<&ProjectileOwner> {
        Some(&self.owner)
    }

    fn entity_bedrock_spawn_metadata(
        &self,
    ) -> Option<pumpkin_protocol::bedrock::client::set_actor_data::SyncedActorDataList> {
        let mut meta =
            pumpkin_protocol::bedrock::client::set_actor_data::SyncedActorDataList::new();
        meta.set(
            pumpkin_protocol::bedrock::client::set_actor_data::entity_data_key::OWNER,
            pumpkin_protocol::bedrock::client::set_actor_data::MetadataValue::Int64(i64::from(
                self.owner
                    .get(&self.entity.world.load())
                    .map_or(0, |owner| owner.get_entity().entity_id),
            )),
        );
        Some(meta)
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
    fn on_hit(&self, _hit: ProjectileHit) {}

    fn tick(&self, caller: &dyn EntityBase, _server: &Server) {
        self.process_tick(caller);
    }
}
