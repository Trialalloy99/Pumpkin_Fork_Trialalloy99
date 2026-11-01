use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
};

use pumpkin_data::BlockState;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::pig_sound_variant::PigSoundVariant;
use pumpkin_data::pig_variant::PigVariant;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::{entity::EntityType, item::Item};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::Difficulty;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;

use crate::entity::EntityBase;
use crate::entity::custom_sound::CustomSound;
use crate::entity::item_steerable::{ItemBasedSteering, ItemSteerable};
use crate::entity::mob::conversion;
use crate::entity::mob::equipment::RegionalDifficulty;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity,
    ageable::AgeableMob,
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
};
use crate::world::World;

const PIG_FOOD: &[&Item] = &[&Item::CARROT, &Item::POTATO, &Item::BEETROOT];
const PIG_STICK: &[&Item] = &[&Item::CARROT_ON_A_STICK];

/// Represents a Pig, a common passive mob that provides porkchops.
///
/// Wiki: <https://minecraft.wiki/w/Pig>
pub struct PigEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: crate::entity::ageable::AgeableData,
    pub steering: ItemBasedSteering,
    pub variant: AtomicU8,
    pub sound_variant: AtomicU8,
}

impl PigEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let world = entity.world.load();
        let biome = world.get_biome(&entity.block_pos.load());
        let variant = PigVariant::select_for_biome(biome.registry_id);
        let mob_entity = MobEntity::new(entity);
        let pig = Self {
            mob_entity,
            ageable_data: crate::entity::ageable::AgeableData::default(),
            steering: ItemBasedSteering::default(),
            variant: AtomicU8::new(variant.id()),
            sound_variant: AtomicU8::new(PigSoundVariant::Classic as u8),
        };
        let mob_arc = Arc::new(pig);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // pig: goals (Pig.registerGoals)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.25));
            goal_selector.add_goal(3, BreedGoal::new(1.0));
            goal_selector.add_goal(4, Box::new(TemptGoal::new(1.2, PIG_STICK, false)));
            goal_selector.add_goal(4, Box::new(TemptGoal::new(1.2, PIG_FOOD, false)));
            goal_selector.add_goal(5, Box::new(FollowParentGoal::new(1.1)));
            goal_selector.add_goal(6, Box::new(WanderAroundGoal::water_avoiding(1.0)));
            goal_selector.add_goal(
                7,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(8, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    pub fn set_variant(&self, variant: PigVariant) {
        self.set_variant_id(variant.id());
    }

    pub fn set_variant_id(&self, raw_id: u8) {
        self.variant.store(raw_id, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::pig::DATA_VARIANT_ID,
            VarInt(i32::from(raw_id)),
        );
    }

    pub fn set_sound_variant(&self, sound_variant: PigSoundVariant) {
        self.sound_variant
            .store(sound_variant as u8, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::pig::DATA_SOUND_VARIANT_ID,
            VarInt(sound_variant as u8 as i32),
        );
    }
}

impl CustomSound for PigEntity {
    fn death_sound(&self) -> Option<Sound> {
        let is_baby = self.is_baby();
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.death_sound(is_baby))
    }

    fn hurt_sound(&self) -> Option<Sound> {
        let is_baby = self.is_baby();
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.hurt_sound(is_baby))
    }
}

impl AgeableMob for PigEntity {
    fn get_ageable_data(&self) -> &crate::entity::ageable::AgeableData {
        &self.ageable_data
    }
}

impl Animal for PigEntity {
    // pig: variant eat sound (Pig.playEatingSound)
    fn eating_sound(&self) -> Option<Sound> {
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.eat_sound(self.is_baby()))
    }

    // pig: baby takes a parent variant (Pig.getBreedOffspring)
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let baby = Self::new(Entity::new(
            entity.world.load_full(),
            entity.pos.load(),
            &EntityType::PIG,
        ));
        if let Some(partner) = partner.cast_any().downcast_ref::<Self>() {
            let variant = if self.get_random().random::<bool>() {
                self.variant.load(Ordering::Relaxed)
            } else {
                partner.variant.load(Ordering::Relaxed)
            };
            baby.variant.store(variant, Ordering::Relaxed);
        }
        baby
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        use pumpkin_data::tag::Taggable;
        item_stack
            .item
            .has_tag(&pumpkin_data::tag::Item::MINECRAFT_PIG_FOOD)
    }
}

impl Mob for PigEntity {
    // pig: own baby hitbox (Pig.BABY_DIMENSIONS)
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.45,
            height: 0.45,
            eye_height: 0.406_25,
        })
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        self.play_entity_sound(sound_variant.step_sound(self.is_baby()), 0.15, 1.0);
    }

    fn has_audited_tick(&self) -> bool {
        true
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = self.is_baby();
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::pig::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::pig::DATA_VARIANT_ID,
            VarInt(self.variant.load(Ordering::Relaxed) as i32),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::pig::DATA_SOUND_VARIANT_ID,
            VarInt(self.sound_variant.load(Ordering::Relaxed) as i32),
        );
    }

    fn mob_set_variant_name(&self, name: &str) {
        if let Some(id) = self.resolve_dynamic_raw_id("minecraft:pig_variant", name, |n| {
            PigVariant::from_name(n).map(|v| u32::from(v.id()))
        }) && let Ok(variant_id) = u8::try_from(id)
        {
            self.set_variant_id(variant_id);
        }
    }

    fn mob_set_sound_variant_name(&self, name: &str) {
        if let Some(v) = PigSoundVariant::from_name(name) {
            self.set_sound_variant(v);
        }
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let variant_id = u32::from(self.variant.load(Ordering::Relaxed));
        let variant_str =
            self.resolve_dynamic_entry_id("minecraft:pig_variant", variant_id, |id| {
                let variant = PigVariant::from_id(id as u8).unwrap_or_default();
                format!("minecraft:{}", variant.to_name())
            });
        nbt.put_string("variant", variant_str);
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        nbt.put_string(
            "sound_variant",
            format!("minecraft:{}", sound_variant.to_name()),
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        // pig: legacy Saddle flag from older saves
        if nbt.get_byte("Saddle") == Some(1) && !self.is_saddled() {
            self.set_saddled(true);
        }
        if let Some(variant_str) = nbt.get_string("variant")
            && let Some(id) =
                self.resolve_dynamic_raw_id("minecraft:pig_variant", variant_str, |n| {
                    PigVariant::from_name(n).map(|v| u32::from(v.id()))
                })
            && let Ok(variant_id) = u8::try_from(id)
        {
            self.variant.store(variant_id, Ordering::Relaxed);
        }
        if let Some(sound_str) = nbt.get_string("sound_variant")
            && let Some(sound_variant) = PigSoundVariant::from_name(sound_str)
        {
            self.sound_variant
                .store(sound_variant as u8, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn get_item_steerable(&self) -> Option<&dyn ItemSteerable> {
        Some(self)
    }

    // pig: saddle lives in the SADDLE equipment slot (Pig.isSaddled)
    fn is_saddled(&self) -> bool {
        let equipment = self
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        equipment
            .equipment
            .get(&EquipmentSlot::SADDLE)
            .is_some_and(|stack| !stack.is_empty())
    }

    fn can_be_saddled(&self) -> bool {
        self.mob_entity.living_entity.entity.is_alive() && !self.is_baby()
    }

    fn set_saddled(&self, saddled: bool) {
        if saddled {
            self.mob_entity.set_item_slot_and_drop_when_killed(
                &EquipmentSlot::SADDLE,
                ItemStack::new(1, &Item::SADDLE),
            );
        } else {
            self.mob_entity
                .set_item_slot(&EquipmentSlot::SADDLE, ItemStack::EMPTY.clone());
        }
    }

    // pig: saddled + player holding carrot on a stick steers (Pig.getControllingPassenger)
    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        let first = self
            .get_entity()
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .cloned()?;
        if let Some(player) = first.get_player() {
            let holds_stick = |stack: ItemStack| stack.item == &Item::CARROT_ON_A_STICK;
            return (self.is_saddled()
                && (holds_stick(player.living_entity.held_item(player))
                    || holds_stick(player.living_entity.off_hand_item(player))))
            .then_some(first);
        }
        (!self.mob_entity.is_no_ai() && first.get_mob().is_some() && first.can_control_vehicle())
            .then_some(first)
    }

    // pig: face rider, tick boost (Pig.tickRidden)
    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        let rider = controller.get_entity();
        let yaw = rider.yaw.load();
        let entity = self.get_entity();
        entity.set_rotation(yaw, rider.pitch.load() * 0.5);
        entity.head_yaw.store(yaw);
        entity.body_yaw.store(yaw);
        self.steering.tick_boost();
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        let sound_variant = PigSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.ambient_sound(self.is_baby()))
    }

    // pig: biome variant + random sound variant (Pig.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let biome = world.get_biome(&self.get_entity().block_pos.load());
        self.set_variant(PigVariant::select_for_biome(biome.registry_id));
        let sound_variants = PigSoundVariant::ALL;
        self.set_sound_variant(
            sound_variants[self.get_random().random_range(0..sound_variants.len())],
        );
        self.finalize_spawn_super(group_data)
    }

    // pig: zombified piglin unless peaceful (Pig.thunderHit)
    fn mob_on_lightning_strike(
        &self,
        caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        let world = self.get_entity().world.load_full();
        if world.level_info.load().difficulty != Difficulty::Peaceful {
            let difficulty = RegionalDifficulty::at(&world, self.get_entity().pos.load());
            let converted = conversion::convert_single(
                self,
                &EntityType::ZOMBIFIED_PIGLIN,
                false,
                true,
                |zombified| {
                    if let Some(mob) = zombified.get_mob() {
                        mob.populate_default_equipment_slots(&world, &difficulty);
                        mob.get_mob_entity()
                            .persistence_required
                            .store(true, Ordering::Relaxed);
                    }
                },
            );
            if converted.is_some() {
                return;
            }
        }
        self.get_mob_entity()
            .living_entity
            .on_lightning_strike(caller, lightning);
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        // pig: ride when saddled (Pig.mobInteract)
        let has_food = self.is_food(item_stack);
        let entity = self.get_entity();
        if !has_food
            && self.is_saddled()
            && !entity.has_passengers()
            && !player.get_entity().is_sneaking()
        {
            let world = player.world();
            if let Some(vehicle) = world.get_entity_by_id(entity.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                entity.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
            }
            return true;
        }
        if self.animal_interact(player, item_stack, Sound::EntityPigAmbient) {
            return true;
        }
        // pig: equip saddle (Equippable.equipOnInteract)
        if item_stack.item == &Item::SADDLE && self.can_be_saddled() && !self.is_saddled() {
            let saddle = item_stack.copy_with_count(1);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            self.mob_entity
                .set_item_slot_and_drop_when_killed(&EquipmentSlot::SADDLE, saddle);
            let world = entity.world.load();
            let pos = entity.pos.load();
            world.play_sound(Sound::EntityPigSaddle, SoundCategory::Neutral, &pos);
            world.emit_game_event(GameEvent::Equip.name(), pos);
            return true;
        }
        false
    }
}

impl ItemSteerable for PigEntity {
    // pig: sync boost time so the client speeds up (ItemBasedSteering.boost)
    fn boost(&self) -> bool {
        if !self.steering.boost() {
            return false;
        }
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::pig::DATA_BOOST_TIME,
            VarInt(self.steering.boost_time_total.load(Ordering::Relaxed)),
        );
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
