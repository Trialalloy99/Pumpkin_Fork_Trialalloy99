use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::Taggable;
use pumpkin_data::{
    entity::EntityType,
    item::Item,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;

use crate::entity::ai::behavior::hunting::{self, Hunt};
use crate::entity::{
    Entity, EntityBase,
    ai::behavior::fleeing::{self, Flee},
    ai::goal::{
        creeper_ignite::CreeperIgniteGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, revenge::RevengeGoal,
        swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    ai::target_match::TargetMatch,
    mob::{Mob, MobEntity},
    player::Player,
};

const DEFAULT_FUSE_TIME: i32 = 30;
const DEFAULT_EXPLOSION_RADIUS: i32 = 3;

const CREEPER_FLEES: &[Flee] = &[
    Flee::new(3, TargetMatch::Type(&EntityType::OCELOT), 6.0, 1.0, 1.2),
    Flee::new(3, TargetMatch::Type(&EntityType::CAT), 6.0, 1.0, 1.2),
];

const CREEPER_HUNTS: &[Hunt] = &[Hunt::new(1, TargetMatch::Type(&EntityType::PLAYER), true)];

pub struct CreeperEntity {
    pub mob_entity: MobEntity,
    pub fuse_speed: AtomicI32,
    pub current_fuse_time: AtomicI32,
    pub last_fuse_time: AtomicI32,
    pub fuse_time: AtomicI32,
    pub explosion_radius: AtomicI32,
    pub ignited: AtomicBool,
    pub charged: AtomicBool,
    dropped_skulls: AtomicBool,
}

impl CreeperEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let entity = Self {
            mob_entity,
            fuse_speed: AtomicI32::new(-1),
            current_fuse_time: AtomicI32::new(0),
            last_fuse_time: AtomicI32::new(0),
            fuse_time: AtomicI32::new(DEFAULT_FUSE_TIME),
            explosion_radius: AtomicI32::new(DEFAULT_EXPLOSION_RADIUS),
            ignited: AtomicBool::new(false),
            charged: AtomicBool::new(false),
            dropped_skulls: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(entity);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;
            let target_selector = &mob_arc.mob_entity.target_selector;

            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                2,
                Box::new(CreeperIgniteGoal::new(Arc::downgrade(&mob_arc))),
            );
            fleeing::apply(goal_selector, CREEPER_FLEES);
            // creeper: melee swings but never hurts (Creeper.doHurtTarget returns true)
            goal_selector.add_goal(
                4,
                Box::new(MeleeAttackGoal::new(1.0, false).performing(|mob, _target| {
                    mob.get_mob_entity().living_entity.swing_hand();
                })),
            );
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::water_avoiding(0.8)));

            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(6, Box::new(RandomLookAroundGoal::default()));

            hunting::apply(target_selector, &mob_arc.mob_entity, CREEPER_HUNTS);
            target_selector.add_goal(2, Box::new(RevengeGoal::new(true)));
        };

        mob_arc
    }

    pub fn set_fuse_speed(&self, speed: i32) {
        self.fuse_speed.store(speed, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::creeper::FUSE_ID, VarInt(speed));
    }

    // creeper: explode as the source, lingering cloud, death effects, discard (Creeper.explodeCreeper)
    pub fn explode(&self) {
        let entity = &self.mob_entity.living_entity.entity;
        let radius = self.explosion_radius.load(Ordering::Relaxed) as f32;
        let multiplier = if self.charged.load(Ordering::Relaxed) {
            2.0
        } else {
            1.0
        };
        self.mob_entity
            .living_entity
            .dead
            .store(true, Ordering::Relaxed);
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        match world.get_entity_by_id(entity.entity_id) {
            Some(source) => world.explode_from_entity(
                source,
                pos,
                radius * multiplier,
                crate::world::ExplosionInteraction::Mob,
            ),
            None => world.explode(
                pos,
                radius * multiplier,
                crate::world::ExplosionInteraction::Mob,
            ),
        }
        self.spawn_lingering_cloud();
        self.mob_entity
            .living_entity
            .trigger_on_death_mob_effects(&pumpkin_data::damage::DamageType::GENERIC_KILL);
        entity.remove();
    }

    // creeper: area effect cloud with its active effects (Creeper.spawnLingeringCloud)
    fn spawn_lingering_cloud(&self) {
        let living = &self.mob_entity.living_entity;
        let effects: Vec<_> = living
            .active_effects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|effect| {
                (
                    effect.effect_type,
                    // vanilla potionDurationScale 0.25
                    (effect.duration as f32 * 0.25).floor() as i32,
                    effect.amplifier,
                    effect.ambient,
                    effect.show_particles,
                    effect.show_icon,
                )
            })
            .collect();
        if effects.is_empty() {
            return;
        }
        let entity = &living.entity;
        let world = entity.world.load_full();
        let cloud_entity = Entity::new(
            world.clone(),
            entity.pos.load(),
            &EntityType::AREA_EFFECT_CLOUD,
        );
        let cloud = crate::entity::area_effect_cloud::AreaEffectCloudEntity::create(
            cloud_entity,
            ItemStack::EMPTY.clone(),
            effects,
            300,
            2.5,
            20,
            10,
            -0.5,
            0,
        );
        world.spawn_entity(cloud);
    }
}

impl Mob for CreeperEntity {
    // creeper: jumps further with a target, health - 1 (Creeper.getMaxFallDistance)
    fn max_fall_distance(&self) -> i32 {
        if self.mob_entity.get_target().is_none() {
            3
        } else {
            (self.mob_entity.living_entity.health.load() - 1.0 + 3.0).floor() as i32
        }
    }

    // creeper: landing shortens the fuse (Creeper.causeFallDamage)
    fn on_cause_fall_damage(&self, fall_distance: f32) {
        let fuse = self.fuse_time.load(Ordering::Relaxed);
        let swell = (self.current_fuse_time.load(Ordering::Relaxed) + (fall_distance * 1.5) as i32)
            .min(fuse - 5);
        self.current_fuse_time.store(swell, Ordering::Relaxed);
    }

    // creeper: goats are never targeted (Creeper.setTarget)
    fn apply_mob_target(&self, target: Option<Arc<dyn EntityBase>>) {
        if target
            .as_ref()
            .is_some_and(|target| target.get_entity().entity_type == &EntityType::GOAT)
        {
            return;
        }
        *self
            .mob_entity
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }

    // creeper: charged kill drops the victim's skull once (Creeper.killedEntity)
    fn killed_entity(&self, victim: &dyn EntityBase) -> bool {
        if !self.is_charged()
            || self.dropped_skulls.load(Ordering::Relaxed)
            || !Mob::should_drop_loot(self)
        {
            return true;
        }
        let victim_entity = victim.get_entity();
        let world = victim_entity.world.load();
        let key = format!(
            "minecraft:charged_creeper/{}",
            victim_entity.entity_type.resource_name
        );
        if let Some(table) = world.get_loot_table(&key) {
            let params = crate::world::loot::LootContextParameters {
                world: Some(&world),
                position: Some(victim_entity.pos.load()),
                this_entity: Some(victim),
                ..Default::default()
            };
            for stack in
                crate::world::loot::generate_loot_from_handle(&table, rand::random(), &params)
            {
                victim_entity.spawn_at_location(stack);
                self.dropped_skulls.store(true, Ordering::Relaxed);
            }
        }
        true
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("powered", self.charged.load(Ordering::Relaxed));
        nbt.put_short("Fuse", self.fuse_time.load(Ordering::Relaxed) as i16);
        nbt.put_byte(
            "ExplosionRadius",
            self.explosion_radius.load(Ordering::Relaxed) as i8,
        );
        nbt.put_bool("ignited", self.ignited.load(Ordering::Relaxed));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(powered) = nbt.get_bool("powered") {
            self.set_charged(powered);
        }
        if let Some(fuse) = nbt.get_short("Fuse") {
            self.fuse_time.store(i32::from(fuse), Ordering::Relaxed);
        }
        if let Some(radius) = nbt.get_byte("ExplosionRadius") {
            self.explosion_radius
                .store(i32::from(radius), Ordering::Relaxed);
        }
        if nbt.get_bool("ignited").unwrap_or(false) {
            self.set_ignited(true);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_metadata(&self) -> Option<crate::entity::mob::MobMetadata> {
        let mut meta = crate::entity::mob::MobMetadata::new();
        if self.is_charged() {
            meta = meta.charged(true);
        }
        if self.is_ignited() {
            meta = meta.ignited(true);
        }
        let fuse = self.fuse_speed.load(Ordering::Relaxed);
        if fuse != -1 {
            meta = meta.fuse_speed(fuse);
        }
        Some(meta)
    }

    fn mob_on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        self.charged.store(true, Ordering::Relaxed);
        self.mob_entity
            .living_entity
            .entity
            .set_synced_data(pumpkin_data::tracked_data::creeper::CHARGED, true);
        self.mob_entity
            .living_entity
            .on_lightning_strike(caller, lightning);
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = &self.mob_entity.living_entity.entity;
        if !entity.is_alive() {
            return;
        }

        self.last_fuse_time.store(
            self.current_fuse_time.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );

        if self.ignited.load(Ordering::Relaxed) {
            self.set_fuse_speed(1);
        }

        let fuse_speed = self.fuse_speed.load(Ordering::Relaxed);
        let current = self.current_fuse_time.load(Ordering::Relaxed);

        if fuse_speed > 0 && current == 0 {
            self.play_entity_sound(Sound::EntityCreeperPrimed, 1.0, 0.5);
            entity.world.load().emit_game_event_with_context(
                pumpkin_data::game_event::GameEvent::PrimeFuse.name(),
                entity.pos.load(),
                &crate::world::game_event::GameEventContext {
                    source_entity: Some(self),
                },
            );
        }

        let fuse_time = self.fuse_time.load(Ordering::Relaxed);
        let new_fuse = (current + fuse_speed).max(0);
        self.current_fuse_time.store(new_fuse, Ordering::Relaxed);

        if new_fuse >= fuse_time {
            self.current_fuse_time.store(fuse_time, Ordering::Relaxed);
            self.explode();
        }
    }

    // creeper: flint and steel / fire charge ignite (Creeper.mobInteract, #creeper_igniters)
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if !item_stack
            .item
            .has_tag(&pumpkin_data::tag::Item::MINECRAFT_CREEPER_IGNITERS)
        {
            return self
                .mob_entity
                .mob_interact(player, item_stack, self.can_be_leashed());
        }

        let entity = &self.mob_entity.living_entity.entity;
        let sound = if item_stack.item.id == Item::FIRE_CHARGE.id {
            Sound::ItemFirechargeUse
        } else {
            Sound::ItemFlintandsteelUse
        };
        entity.world.load().play_sound_fine(
            sound,
            SoundCategory::Hostile,
            &entity.pos.load(),
            1.0,
            rand::random::<f32>() * 0.4 + 0.8,
        );
        self.set_ignited(true);
        if item_stack.is_damageable() {
            if player.gamemode.load() != pumpkin_util::GameMode::Creative {
                let _ = player.damage_item_stack(item_stack, 1);
            }
        } else {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        }
        true
    }
}

impl CreeperEntity {
    pub fn is_charged(&self) -> bool {
        self.charged.load(Ordering::Relaxed)
    }

    pub fn set_charged(&self, charged: bool) {
        self.charged.store(charged, Ordering::Relaxed);
        let entity = &self.mob_entity.living_entity.entity;
        entity.set_synced_data(pumpkin_data::tracked_data::creeper::CHARGED, charged);
        if let Some(meta) = self.mob_metadata() {
            entity.send_bedrock_actor_data(&meta.to_bedrock_actor_data());
        }
    }

    pub fn is_ignited(&self) -> bool {
        self.ignited.load(Ordering::Relaxed)
    }

    pub fn set_ignited(&self, ignited: bool) {
        self.ignited.store(ignited, Ordering::Relaxed);
        let entity = &self.mob_entity.living_entity.entity;
        entity.set_synced_data(pumpkin_data::tracked_data::creeper::IS_IGNITED, ignited);
        if let Some(meta) = self.mob_metadata() {
            entity.send_bedrock_actor_data(&meta.to_bedrock_actor_data());
        }
    }

    pub fn get_fuse(&self) -> i32 {
        self.fuse_time.load(Ordering::Relaxed)
    }

    pub fn set_fuse(&self, fuse: i32) {
        self.fuse_time.store(fuse, Ordering::Relaxed);
    }

    pub fn get_explosion_radius(&self) -> i32 {
        self.explosion_radius.load(Ordering::Relaxed)
    }

    pub fn set_explosion_radius(&self, radius: i32) {
        self.explosion_radius.store(radius, Ordering::Relaxed);
    }
}
