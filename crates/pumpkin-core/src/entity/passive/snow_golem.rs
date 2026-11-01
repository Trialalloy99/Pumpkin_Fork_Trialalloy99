use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::Block;
use pumpkin_data::damage::DamageType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::world::BlockFlags;
use rand::RngExt;

use crate::world::loot::{LootContextParameters, generate_loot_from_handle};

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::ai::target_match::TargetMatch;
use crate::entity::{
    Entity, EntityBase,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        ranged_attack::RangedAttackGoal, wander_around::WanderAroundGoal,
    },
    item::ItemEntity,
    mob::{Mob, MobEntity, RangedAttackMob},
    player::Player,
    projectile::snowball::SnowballEntity,
};

const SNOW_GOLEM_HUNTS: &[Hunt] = &[Hunt::new(1, TargetMatch::Enemy, true)];

pub struct SnowGolemEntity {
    pub mob_entity: MobEntity,
    pub has_pumpkin: AtomicBool,
}

impl SnowGolemEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let snow_golem = Self {
            mob_entity,
            has_pumpkin: AtomicBool::new(true),
        };
        let mob_arc = Arc::new(snow_golem);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };
        let ranged_weak: Weak<dyn RangedAttackMob> = {
            let ranged_arc: Arc<dyn RangedAttackMob> = mob_arc.clone();
            Arc::downgrade(&ranged_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            goal_selector.add_goal(
                1,
                Box::new(RangedAttackGoal::new(ranged_weak, 1.25, 20, 10.0)),
            );
            // snow golem: goals (SnowGolem.registerGoals)
            goal_selector.add_goal(
                2,
                Box::new(WanderAroundGoal::water_avoiding_with_probability(
                    1.0, 1.000_000_1E-5,
                )),
            );
            goal_selector.add_goal(
                3,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(4, Box::new(RandomLookAroundGoal::default()));

            hunting::apply(target_selector, &mob_arc.mob_entity, SNOW_GOLEM_HUNTS);
        };

        mob_arc
    }

    #[must_use]
    pub fn has_pumpkin(&self) -> bool {
        self.has_pumpkin.load(Ordering::Relaxed)
    }

    pub fn set_has_pumpkin(&self, has_pumpkin: bool) {
        self.has_pumpkin.store(has_pumpkin, Ordering::Relaxed);
        let entity = self.get_entity();
        let flags = if has_pumpkin { 16u8 } else { 0u8 };
        entity.set_synced_data(
            pumpkin_data::tracked_data::snow_golem::DATA_PUMPKIN_ID,
            flags as i8,
        );
    }

    // snow golem: aimed snowball (SnowGolem.performRangedAttack)
    pub fn throw_snowball(&self, target: &Arc<dyn EntityBase>) {
        let entity = self.get_entity();
        let world = entity.world.load_full();

        let snowball_entity = Entity::new(world.clone(), entity.pos.load(), &EntityType::SNOWBALL);
        let snowball = SnowballEntity::new_shot(snowball_entity, entity);

        let mob_pos = entity.pos.load();
        let target_entity = target.get_entity();
        let target_pos = target_entity.pos.load();

        let dx = target_pos.x - mob_pos.x;
        let dy = target_entity.get_eye_y() - f64::from(1.1f32);
        let dz = target_pos.z - mob_pos.z;
        let yo = dx.hypot(dz) * f64::from(0.2f32);

        snowball.thrown.set_velocity(
            dx,
            dy + yo - snowball.get_entity().pos.load().y,
            dz,
            1.6,
            12.0,
        );
        world.spawn_entity(Arc::new(snowball));

        let pitch = 0.4 / self.get_random().random::<f32>().mul_add(0.4, 0.8);
        self.play_entity_sound(Sound::EntitySnowGolemShoot, 1.0, pitch);
    }

    // snow golem: pumpkin off, loot at eye height (SnowGolem.shear)
    pub fn shear(&self, category: SoundCategory, tool: &ItemStack) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        world.play_sound(Sound::EntitySnowGolemShear, category, &pos);
        self.set_has_pumpkin(false);
        let Some(table) = world.get_loot_table("minecraft:shearing/snow_golem") else {
            return;
        };
        let params = LootContextParameters {
            world: Some(&world),
            position: Some(pos),
            this_entity: Some(self),
            tool: Some(tool.clone()),
            ..Default::default()
        };
        let drop_pos = pos.add_raw(0.0, entity.get_eye_height(), 0.0);
        for stack in generate_loot_from_handle(&table, self.get_random().random(), &params) {
            let item_entity = ItemEntity::new(
                Entity::new(world.clone(), drop_pos, &EntityType::ITEM),
                stack,
            );
            world.spawn_entity(Arc::new(item_entity));
        }
    }
}

impl Mob for SnowGolemEntity {
    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("Pumpkin", self.has_pumpkin());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(pumpkin) = nbt.get_bool("Pumpkin") {
            self.set_has_pumpkin(pumpkin);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntitySnowGolemAmbient)
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        120
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        false
    }

    // snow golem: water hurts, hot biomes melt, snow trail (LivingEntity.aiStep, SnowGolem.aiStep)
    fn mob_ai_step(&self) {
        let entity = self.get_entity();
        if !entity.is_alive() {
            return;
        }
        if entity.is_in_water_or_rain() {
            self.damage(self, 1.0, DamageType::DROWN);
        }
        let world = entity.world.load();
        let block_pos = entity.block_pos.load();
        // snow golem: dimension default, hot biome override (EnvironmentAttributes.SNOW_GOLEM_MELTS)
        if world.dimension.snow_golem_melts || world.get_biome(&block_pos).snow_golem_melts {
            self.damage(self, 1.0, DamageType::ON_FIRE);
        }
        if !world.level_info.load().game_rules.mob_griefing {
            return;
        }
        let pos = entity.pos.load();
        for i in 0..4 {
            let x = (pos.x + f64::from((i % 2 * 2 - 1) as f32 * 0.25)).floor() as i32;
            let z = (pos.z + f64::from((i / 2 % 2 * 2 - 1) as f32 * 0.25)).floor() as i32;
            let snow_pos = BlockPos::new(x, pos.y.floor() as i32, z);
            if world.get_block_state(&snow_pos).is_air()
                && crate::block::blocks::snow::can_place_at(world.as_ref(), &snow_pos)
            {
                world.set_block_state(
                    &snow_pos,
                    Block::SNOW.default_state.id,
                    BlockFlags::NOTIFY_ALL,
                );
                world.emit_game_event(
                    GameEvent::BlockPlace.name(),
                    Vector3::new(
                        f64::from(snow_pos.0.x) + 0.5,
                        f64::from(snow_pos.0.y) + 0.5,
                        f64::from(snow_pos.0.z) + 0.5,
                    ),
                );
            }
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let flags = if self.has_pumpkin() { 16u8 } else { 0u8 };
        entity.set_synced_data(
            pumpkin_data::tracked_data::snow_golem::DATA_PUMPKIN_ID,
            flags as i8,
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.get_item() != &Item::SHEARS || !self.has_pumpkin() {
            return false;
        }
        let entity = self.get_entity();
        let world = entity.world.load();
        if let Some(server) = world.server.upgrade() {
            let mut event = crate::plugin::api::events::player::player_shear_entity::PlayerShearEntityEvent {
                player: player.clone(),
                entity_id: entity.entity_id,
                hand: 0,
                cancelled: false,
            };
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return false;
            }
        }
        self.shear(SoundCategory::Players, item_stack);
        world.emit_game_event(GameEvent::Shear.name(), entity.pos.load());
        let _ = player.damage_item_stack(item_stack, 1);
        true
    }
}

impl RangedAttackMob for SnowGolemEntity {
    fn perform_ranged_attack(&self, target: &Arc<dyn EntityBase>, _power: f32) {
        self.throw_snowball(target);
    }
}
