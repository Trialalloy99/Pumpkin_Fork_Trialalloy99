use std::sync::{
    Arc, Mutex, PoisonError, Weak,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::{EntityStatus, EntityType};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::{Block, tracked_data};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase,
    ai::control::flying_move_control::FlyingMoveControl,
    ai::goal::{
        Controls, Goal, escape_danger::EscapeDangerGoal, follow_mob::FollowMobGoal,
        follow_owner::FollowOwnerGoal, look_at_entity::LookAtEntityGoal,
        sit_when_ordered_to::SitWhenOrderedToGoal, swim::SwimGoal,
        water_avoiding_random_flying::WaterAvoidingRandomFlyingGoal,
    },
    ai::pathfinder::{Navigator, node::PathType},
    ai::util::land_random_pos,
    mob::{Mob, MobEntity},
    passive::{
        animal::Animal,
        tamable::{TamableAnimal, TamableData},
    },
    player::Player,
};
use crate::world::World;

// parrot: constants (Parrot, ShoulderRidingEntity)
const COOKIE_POISON_DURATION: i32 = 900;
const TAME_CHANCE: i32 = 10;
const RIDE_COOLDOWN: i32 = 100;
const IMITATION_RANGE: f64 = 20.0;
const IMITATION_CHANCE: i32 = 400;
const JUKEBOX_PARTY_DISTANCE: f64 = 3.46;
const SHOULDER_SIT_TICKS: i32 = RIDE_COOLDOWN;
const VARIANT_COUNT: u8 = 5;

// parrot: MOB_SOUND_MAP (Parrot)
const IMITATED_SOUNDS: &[(&EntityType, Sound)] = &[
    (&EntityType::BLAZE, Sound::EntityParrotImitateBlaze),
    (&EntityType::BOGGED, Sound::EntityParrotImitateBogged),
    (&EntityType::BREEZE, Sound::EntityParrotImitateBreeze),
    (&EntityType::CAVE_SPIDER, Sound::EntityParrotImitateSpider),
    (&EntityType::CREAKING, Sound::EntityParrotImitateCreaking),
    (&EntityType::CREEPER, Sound::EntityParrotImitateCreeper),
    (&EntityType::DROWNED, Sound::EntityParrotImitateDrowned),
    (
        &EntityType::ELDER_GUARDIAN,
        Sound::EntityParrotImitateElderGuardian,
    ),
    (
        &EntityType::ENDER_DRAGON,
        Sound::EntityParrotImitateEnderDragon,
    ),
    (&EntityType::ENDERMITE, Sound::EntityParrotImitateEndermite),
    (&EntityType::EVOKER, Sound::EntityParrotImitateEvoker),
    (&EntityType::GHAST, Sound::EntityParrotImitateGhast),
    (&EntityType::GUARDIAN, Sound::EntityParrotImitateGuardian),
    (&EntityType::HOGLIN, Sound::EntityParrotImitateHoglin),
    (&EntityType::HUSK, Sound::EntityParrotImitateHusk),
    (
        &EntityType::ILLUSIONER,
        Sound::EntityParrotImitateIllusioner,
    ),
    (&EntityType::MAGMA_CUBE, Sound::EntityParrotImitateMagmaCube),
    (&EntityType::PARCHED, Sound::EntityParrotImitateParched),
    (&EntityType::PHANTOM, Sound::EntityParrotImitatePhantom),
    (&EntityType::PIGLIN, Sound::EntityParrotImitatePiglin),
    (
        &EntityType::PIGLIN_BRUTE,
        Sound::EntityParrotImitatePiglinBrute,
    ),
    (&EntityType::PILLAGER, Sound::EntityParrotImitatePillager),
    (&EntityType::RAVAGER, Sound::EntityParrotImitateRavager),
    (&EntityType::SHULKER, Sound::EntityParrotImitateShulker),
    (
        &EntityType::SILVERFISH,
        Sound::EntityParrotImitateSilverfish,
    ),
    (&EntityType::SKELETON, Sound::EntityParrotImitateSkeleton),
    (&EntityType::SLIME, Sound::EntityParrotImitateSlime),
    (&EntityType::SPIDER, Sound::EntityParrotImitateSpider),
    (&EntityType::STRAY, Sound::EntityParrotImitateStray),
    (&EntityType::VEX, Sound::EntityParrotImitateVex),
    (
        &EntityType::VINDICATOR,
        Sound::EntityParrotImitateVindicator,
    ),
    (&EntityType::WARDEN, Sound::EntityParrotImitateWarden),
    (&EntityType::WITCH, Sound::EntityParrotImitateWitch),
    (&EntityType::WITHER, Sound::EntityParrotImitateWither),
    (
        &EntityType::WITHER_SKELETON,
        Sound::EntityParrotImitateWitherSkeleton,
    ),
    (&EntityType::ZOGLIN, Sound::EntityParrotImitateZoglin),
    (&EntityType::ZOMBIE, Sound::EntityParrotImitateZombie),
    (
        &EntityType::ZOMBIE_HORSE,
        Sound::EntityParrotImitateZombieHorse,
    ),
    (
        &EntityType::ZOMBIE_VILLAGER,
        Sound::EntityParrotImitateZombieVillager,
    ),
];

// parrot: Parrot.getImitatedSound
fn imitated_sound(entity_type: &EntityType) -> Sound {
    IMITATED_SOUNDS
        .iter()
        .find(|(candidate, _)| candidate.id == entity_type.id)
        .map_or(Sound::EntityParrotAmbient, |(_, sound)| *sound)
}

/// Represents a Parrot, a tameable flying mob that can mimic nearby mob sounds.
///
/// Wiki: <https://minecraft.wiki/w/Parrot>
pub struct ParrotEntity {
    pub mob_entity: MobEntity,
    tamable_data: TamableData,
    variant: AtomicU8,
    flap_speed: AtomicCell<f32>,
    flapping: AtomicCell<f32>,
    next_flap: AtomicCell<f32>,
    party_parrot: AtomicBool,
    jukebox: Mutex<Option<BlockPos>>,
    ride_cooldown_counter: AtomicI32,
}

impl ParrotEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mut mob_entity = MobEntity::new(entity);
        *mob_entity
            .move_control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Box::new(FlyingMoveControl::new(10, false));
        let navigator = mob_entity
            .navigator
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        *navigator = Navigator::flying();
        navigator.set_can_open_doors(false);
        navigator.set_can_float(true);
        // parrot: pathfinding malus (Parrot constructor)
        navigator.set_pathfinding_malus(PathType::DangerFire, -1.0);
        navigator.set_pathfinding_malus(PathType::DamageFire, -1.0);
        navigator.set_pathfinding_malus(PathType::Cocoa, -1.0);
        let parrot = Self {
            mob_entity,
            tamable_data: TamableData::default(),
            variant: AtomicU8::new(0),
            flap_speed: AtomicCell::new(0.0),
            flapping: AtomicCell::new(1.0),
            next_flap: AtomicCell::new(1.0),
            party_parrot: AtomicBool::new(false),
            jukebox: Mutex::new(None),
            ride_cooldown_counter: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(parrot);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, EscapeDangerGoal::new(1.25));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(
                1,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(2, Box::new(SitWhenOrderedToGoal::new()));
            goal_selector.add_goal(2, FollowOwnerGoal::new(1.0, 5.0, 1.0));
            goal_selector.add_goal(2, Box::new(ParrotWanderGoal::new()));
            goal_selector.add_goal(3, Box::new(LandOnOwnersShoulderGoal::new()));
            goal_selector.add_goal(3, Box::new(FollowMobGoal::new(1.0, 3.0, 7.0)));
        };

        mob_arc
    }

    #[must_use]
    pub fn variant(&self) -> u8 {
        self.variant.load(Ordering::Relaxed)
    }

    pub fn set_variant(&self, variant: u8) {
        let variant = variant % VARIANT_COUNT;
        self.variant.store(variant, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(tracked_data::parrot::DATA_VARIANT_ID, i32::from(variant));
    }

    #[must_use]
    pub fn is_party_parrot(&self) -> bool {
        self.party_parrot.load(Ordering::Relaxed)
    }

    // parrot: Parrot.isFlying
    #[must_use]
    pub fn is_flying(&self) -> bool {
        !self.get_entity().on_ground.load(Ordering::Relaxed)
    }

    // shoulder: ShoulderRidingEntity.canSitOnShoulder
    #[must_use]
    pub fn can_sit_on_shoulder(&self) -> bool {
        self.ride_cooldown_counter.load(Ordering::Relaxed) > SHOULDER_SIT_TICKS
    }

    // parrot: Parrot.getPitch
    #[must_use]
    pub fn get_pitch() -> f32 {
        (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0
    }

    // parrot: Parrot.getAmbient, rarely imitates a random mob
    #[must_use]
    pub fn get_ambient_sound(world: &World) -> Sound {
        if world.level_info.load().difficulty != pumpkin_util::Difficulty::Peaceful
            && rand::random_range(0..1000) == 0
        {
            IMITATED_SOUNDS[rand::random_range(0..IMITATED_SOUNDS.len())].1
        } else {
            Sound::EntityParrotAmbient
        }
    }

    // parrot: Parrot.imitateNearbyMobs
    pub fn imitate_nearby_mobs(world: &Arc<World>, entity: &Entity) -> bool {
        if !entity.is_alive() || entity.silent.load(Ordering::Relaxed) || rand::random::<bool>() {
            return false;
        }
        let pos = entity.pos.load();
        let area = BoundingBox::new(
            Vector3::new(
                pos.x - IMITATION_RANGE,
                pos.y - IMITATION_RANGE,
                pos.z - IMITATION_RANGE,
            ),
            Vector3::new(
                pos.x + IMITATION_RANGE,
                pos.y + IMITATION_RANGE,
                pos.z + IMITATION_RANGE,
            ),
        );
        let candidates: Vec<_> = world
            .get_entities_at_box(&area)
            .into_iter()
            .filter(|candidate| {
                candidate.get_mob().is_some()
                    && IMITATED_SOUNDS
                        .iter()
                        .any(|(kind, _)| kind.id == candidate.get_entity().entity_type.id)
            })
            .collect();
        if candidates.is_empty() {
            return false;
        }
        let mob = &candidates[rand::random_range(0..candidates.len())];
        if mob.get_entity().silent.load(Ordering::Relaxed) {
            return false;
        }
        world.play_sound_fine(
            imitated_sound(mob.get_entity().entity_type),
            SoundCategory::Neutral,
            &pos,
            0.7,
            Self::get_pitch(),
        );
        true
    }

    /// Feeds the parrot a cookie: it is poisoned and then killed, as in vanilla
    /// `Parrot.mobInteract`.
    fn eat_cookie(&self, player: &Arc<Player>, item_stack: &mut ItemStack) {
        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        self.mob_entity
            .living_entity
            .add_effect(pumpkin_data::potion::Effect {
                effect_type: &StatusEffect::POISON,
                duration: COOKIE_POISON_DURATION,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: true,
            });
        // Vanilla guards this call with `player.isCreative() || !this.isInvulnerable()`,
        // but `hurt` re-checks invulnerability itself and `player_attack` doesn't bypass
        // it, so the guard only skips a call that would do nothing anyway.
        self.damage_with_context(
            self,
            f32::MAX,
            DamageType::PLAYER_ATTACK,
            None,
            Some(player.as_ref()),
            Some(player.as_ref()),
        );
    }

    // parrot: seeds tame the parrot (Parrot.mobInteract)
    fn feed_to_tame(&self, player: &Arc<Player>, item_stack: &mut ItemStack) {
        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
        let entity = self.get_entity();
        let world = entity.world.load();
        if !entity.silent.load(Ordering::Relaxed) {
            world.play_sound_fine(
                Sound::EntityParrotEat,
                SoundCategory::Neutral,
                &entity.pos.load(),
                1.0,
                1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2,
            );
        }
        if rand::random_range(0..TAME_CHANCE) == 0 {
            self.tame(player.gameprofile.id);
            world.send_entity_status(
                entity,
                EntityStatus::TamingSucceeded,
                ActorEventID::TamingSucceeded,
            );
        } else {
            world.send_entity_status(
                entity,
                EntityStatus::TamingFailed,
                ActorEventID::TamingFailed,
            );
        }
    }

    // shoulder: ShoulderRidingEntity.setEntityOnShoulder
    fn set_entity_on_shoulder(&self, player: &Player) -> bool {
        let mut nbt = NbtCompound::new();
        self.write_nbt(&mut nbt);
        nbt.put_string("id", "minecraft:parrot".to_string());
        if player.set_entity_on_shoulder(nbt) {
            self.get_entity().remove();
            true
        } else {
            false
        }
    }
}

impl Animal for ParrotEntity {
    // parrot: Parrot.isFood
    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }
}

impl TamableAnimal for ParrotEntity {
    fn get_tamable_data(&self) -> &TamableData {
        &self.tamable_data
    }
}

impl Mob for ParrotEntity {
    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_tamable(&self) -> Option<&dyn TamableAnimal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int("Variant", i32::from(self.variant()));
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_variant(nbt.get_int("Variant").unwrap_or(0) as u8);
    }

    fn mob_init_data_tracker(&self) {
        self.get_entity().set_synced_data(
            tracked_data::parrot::DATA_VARIANT_ID,
            i32::from(self.variant()),
        );
    }

    // parrot: random color, never a baby (Parrot.finalizeSpawn, canBeABaby)
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        group_data: Option<crate::entity::mob::spawn::SpawnGroupData>,
    ) -> Option<crate::entity::mob::spawn::SpawnGroupData> {
        self.set_variant(rand::random_range(0..VARIANT_COUNT));
        self.finalize_spawn_super(group_data.or(Some(
            crate::entity::mob::spawn::SpawnGroupData::Ageable {
                group_size: 0,
                should_spawn_baby: false,
                baby_spawn_chance: 0.0,
            },
        )))
    }

    // parrot: Parrot.aiStep jukebox party, imitation, calculateFlapping
    fn mob_ai_step(&self) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        {
            let mut jukebox = self.jukebox.lock().unwrap_or_else(PoisonError::into_inner);
            let keep = jukebox.is_some_and(|jukebox_pos| {
                jukebox_pos.to_centered_f64().squared_distance_to_vec(&pos)
                    < JUKEBOX_PARTY_DISTANCE * JUKEBOX_PARTY_DISTANCE
                    && world.get_block(&jukebox_pos) == &Block::JUKEBOX
            });
            if !keep {
                self.party_parrot.store(false, Ordering::Relaxed);
                *jukebox = None;
            }
        }
        if rand::random_range(0..IMITATION_CHANCE) == 0 {
            Self::imitate_nearby_mobs(&world, entity);
        }
        // Vanilla `calculateFlapping`.
        let on_ground = entity.on_ground.load(Ordering::Relaxed);
        let flap_change: f32 = if on_ground || entity.has_vehicle() {
            -1.0
        } else {
            4.0
        };
        self.flap_speed.store(
            flap_change
                .mul_add(0.3, self.flap_speed.load())
                .clamp(0.0, 1.0),
        );
        if !on_ground && self.flapping.load() < 1.0 {
            self.flapping.store(1.0);
        }
        self.flapping.store(self.flapping.load() * 0.9);
        let velocity = entity.velocity.load();
        if !on_ground && velocity.y < 0.0 {
            entity.set_velocity(velocity.multiply(1.0, 0.6, 1.0));
        }
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        self.ride_cooldown_counter.fetch_add(1, Ordering::Relaxed);
    }

    fn set_record_playing_nearby(&self, jukebox: BlockPos, playing: bool) {
        *self.jukebox.lock().unwrap_or_else(PoisonError::into_inner) = Some(jukebox);
        self.party_parrot.store(playing, Ordering::Relaxed);
    }

    fn mob_is_flapping(&self) -> bool {
        self.get_entity().fly_dist.load() > self.next_flap.load()
    }

    fn mob_on_flap(&self) {
        self.play_entity_sound(Sound::EntityParrotFly, 0.15, 1.0);
        self.next_flap
            .store(self.get_entity().fly_dist.load() + self.flap_speed.load() / 2.0);
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityParrotStep, 0.15, 1.0);
    }

    fn omnidirectional_air_mover(&self) -> bool {
        true
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Self::get_ambient_sound(&self.get_entity().world.load()))
    }

    fn mob_sound_source(&self) -> SoundCategory {
        SoundCategory::Neutral
    }

    // parrot: Parrot.hurtServer stands up
    fn pre_damage(&self, _damage_type: DamageType, _source: Option<&dyn EntityBase>) -> bool {
        self.set_ordered_to_sit(false);
        true
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    // parrot: Parrot.mobInteract
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();
        if !self.is_tame() && item.has_tag(&tag::Item::MINECRAFT_PARROT_FOOD) {
            self.feed_to_tame(player, item_stack);
            return true;
        }
        if item.has_tag(&tag::Item::MINECRAFT_PARROT_POISONOUS_FOOD) {
            self.eat_cookie(player, item_stack);
            return true;
        }
        if !self.is_flying() && self.is_tame() && self.is_owned_by(&player.gameprofile.id) {
            self.set_ordered_to_sit(!self.is_ordered_to_sit());
            return true;
        }
        self.mob_entity
            .mob_interact(player, item_stack, self.can_be_leashed())
    }
}

// parrot: Parrot.ParrotWanderGoal, prefers tree tops (getTreePos)
struct ParrotWanderGoal {
    inner: WaterAvoidingRandomFlyingGoal,
}

impl ParrotWanderGoal {
    const fn new() -> Self {
        Self {
            inner: WaterAvoidingRandomFlyingGoal::new(1.0),
        }
    }
}

impl Goal for ParrotWanderGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        self.inner.can_start_with(mob, Self::tree_or_default)
    }

    fn should_continue(&mut self, mob: &dyn Mob) -> bool {
        self.inner.should_continue(mob)
    }

    fn start(&mut self, mob: &dyn Mob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn Mob) {
        self.inner.stop(mob);
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

impl ParrotWanderGoal {
    // parrot: getPosition, water exit, then tree top with 1 - probability, else default
    fn tree_or_default(mob: &dyn Mob, default: Option<Vector3<f64>>) -> Option<Vector3<f64>> {
        let mut pos = None;
        if mob.get_entity().is_in_water() {
            pos = land_random_pos::get_pos(mob, 15, 15);
        }
        if mob.get_random().random::<f32>() >= 0.001 {
            pos = Self::tree_pos(mob);
        }
        pos.or(default)
    }

    // parrot: getTreePos, leaves or logs around with open air above
    fn tree_pos(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = mob.get_entity();
        let world = entity.world.load();
        let mob_pos = entity.block_pos.load();
        let mut best: Option<BlockPos> = None;
        for dx in -3..=3 {
            for dy in -3..=3 {
                for dz in -3..=3 {
                    let pos = BlockPos::new(mob_pos.0.x + dx, mob_pos.0.y + dy, mob_pos.0.z + dz);
                    let block = world.get_block(&pos);
                    if !(block.has_tag(&tag::Block::MINECRAFT_LEAVES)
                        || block.has_tag(&tag::Block::MINECRAFT_LOGS))
                    {
                        continue;
                    }
                    let above = pos.up();
                    if world.get_block_state(&above).is_air()
                        && best.is_none_or(|current| {
                            let distance = |p: &BlockPos| mob_pos.0.squared_distance_to_vec(&p.0);
                            distance(&above) < distance(&current)
                        })
                    {
                        best = Some(above);
                    }
                }
            }
        }
        best.map(|pos| {
            Vector3::new(
                f64::from(pos.0.x) + 0.5,
                f64::from(pos.0.y),
                f64::from(pos.0.z) + 0.5,
            )
        })
    }
}

// parrot: LandOnOwnersShoulderGoal
struct LandOnOwnersShoulderGoal {
    sitting_on_shoulder: bool,
}

impl LandOnOwnersShoulderGoal {
    const fn new() -> Self {
        Self {
            sitting_on_shoulder: false,
        }
    }

    fn owner(mob: &dyn Mob) -> Option<Arc<Player>> {
        let owner = mob.get_owner_uuid()?;
        mob.get_entity().world.load().get_player_by_uuid(owner)
    }
}

impl Goal for LandOnOwnersShoulderGoal {
    fn can_start(&mut self, mob: &dyn Mob) -> bool {
        let Some(owner) = Self::owner(mob) else {
            return false;
        };
        let Some(parrot) = mob.cast_any().downcast_ref::<ParrotEntity>() else {
            return false;
        };
        let flying = owner
            .abilities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flying;
        let owner_can_be_sat_on =
            !owner.is_spectator() && !flying && !owner.get_entity().is_in_water();
        !parrot.is_ordered_to_sit() && owner_can_be_sat_on && parrot.can_sit_on_shoulder()
    }

    fn start(&mut self, _mob: &dyn Mob) {
        self.sitting_on_shoulder = false;
    }

    fn can_stop(&self) -> bool {
        !self.sitting_on_shoulder
    }

    fn tick(&mut self, mob: &dyn Mob) {
        let Some(parrot) = mob.cast_any().downcast_ref::<ParrotEntity>() else {
            return;
        };
        if self.sitting_on_shoulder || parrot.is_in_sitting_pose() {
            return;
        }
        let Some(owner) = Self::owner(mob) else {
            return;
        };
        let touching = parrot
            .get_entity()
            .bounding_box
            .load()
            .intersects(&owner.get_entity().bounding_box.load());
        if touching {
            self.sitting_on_shoulder = parrot.set_entity_on_shoulder(&owner);
        }
    }
}
