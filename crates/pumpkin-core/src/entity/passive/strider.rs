use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    item_steerable::{ItemBasedSteering, ItemSteerable},
    mob::{Mob, MobEntity},
    passive::animal::Animal,
    player::Player,
    r#type::from_type,
};
use crate::world::World;

const TEMPT_ITEMS: &[&Item] = &[&Item::WARPED_FUNGUS, &Item::WARPED_FUNGUS_ON_A_STICK];

pub struct StriderEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub steering: ItemBasedSteering,
    pub saddled: AtomicBool,
    pub suffocating: AtomicBool,
}

impl StriderEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);

        // strider: attributes (Strider.createAttributes)
        mob_entity.living_entity.set_max_health(20.0);
        mob_entity.living_entity.health.store(20.0);
        mob_entity
            .living_entity
            .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.175);

        let strider = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            steering: ItemBasedSteering::default(),
            saddled: AtomicBool::new(false),
            suffocating: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(strider);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // strider: vanilla goal priorities (Strider.registerGoals)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(1.65));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.4, TEMPT_ITEMS, false)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.1)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn is_suffocating(&self) -> bool {
        self.suffocating.load(Ordering::Relaxed)
    }

    pub fn set_suffocating(&self, suffocating: bool) {
        self.suffocating.store(suffocating, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_SUFFOCATING,
            suffocating,
        );
    }
}

impl AgeableMob for StriderEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }

    fn default_baby_spawn_chance(&self) -> f32 {
        crate::entity::ageable::STRIDER_BABY_SPAWN_CHANCE
    }
}

impl Animal for StriderEntity {
    fn eating_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderEat)
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_STRIDER_FOOD)
            || item_stack.item == &Item::WARPED_FUNGUS
    }

    fn get_breed_offspring(&self, _partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let world = self.get_entity().world.load();
        let baby_base = from_type(
            &EntityType::STRIDER,
            self.get_entity().pos.load(),
            &world,
            Uuid::new_v4(),
        );
        baby_base.get_entity().age.store(-24000, Ordering::Relaxed);
        baby_base
    }
}

impl CustomSound for StriderEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderDeath)
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderHurt)
    }
}

impl Mob for StriderEntity {
    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityStriderAmbient)
    }

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        let sound = if self.get_entity().touching_lava.load(Ordering::Relaxed) {
            Sound::EntityStriderStepLava
        } else {
            Sound::EntityStriderStep
        };
        self.play_entity_sound(sound, 1.0, 1.0);
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    // strider: player holding warped fungus on a stick steers (Strider.getControllingPassenger)
    fn mob_controlling_passenger(&self) -> Option<Arc<dyn EntityBase>> {
        let first = self.get_entity().passengers.lock().ok()?.first().cloned()?;
        if let Some(player) = first.get_player() {
            let holds_stick = |stack: ItemStack| stack.item == &Item::WARPED_FUNGUS_ON_A_STICK;
            return (self.is_saddled()
                && (holds_stick(player.living_entity.held_item(player))
                    || holds_stick(player.living_entity.off_hand_item(player))))
            .then_some(first);
        }
        (!self.mob_entity.is_no_ai() && first.get_mob().is_some() && first.can_control_vehicle())
            .then_some(first)
    }

    // strider: tick boost and face rider (Strider.tickRidden)
    fn tick_ridden(&self, controller: &Arc<dyn EntityBase>) {
        let rider = controller.get_entity();
        let yaw = rider.yaw.load();
        let entity = self.get_entity();
        entity.set_rotation(yaw, rider.pitch.load() * 0.5);
        entity.head_yaw.store(yaw);
        entity.body_yaw.store(yaw);
        self.steering.tick_boost();
    }

    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();

        // strider: water sensitivity (Strider.isSensitiveToWater)
        if entity.touching_water.load(Ordering::Relaxed) {
            let _ = entity.damage(entity, 1.0, pumpkin_data::damage::DamageType::DROWN);
        }

        // strider: suffocating outside lava (Strider.tick, floatStrider)
        let in_lava = entity.touching_lava.load(Ordering::Relaxed);
        if in_lava {
            if self.is_suffocating() {
                self.set_suffocating(false);
                self.mob_entity
                    .living_entity
                    .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.175);
            }
            let mut vel = entity.velocity.load();
            if vel.y < 0.04 {
                vel.y = 0.04;
                entity.velocity.store(vel);
            }
        } else if !self.is_suffocating() {
            self.set_suffocating(true);
            self.mob_entity
                .living_entity
                .set_attribute_base(&Attributes::MOVEMENT_SPEED, 0.035);
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let result = self.finalize_spawn_super(group_data);
        let mut rng = rand::rng();

        // strider: baby strider jockey (1/10) or zombified piglin jockey (1/30) (Strider.finalizeSpawn)
        if rng.random_range(0.0..1.0) < 0.1 {
            let baby = from_type(
                &EntityType::STRIDER,
                self.get_entity().pos.load(),
                world,
                Uuid::new_v4(),
            );
            baby.get_entity().age.store(-24000, Ordering::Relaxed);
            if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
                self.get_entity().add_passenger(this_base, baby.clone());
            }
            world.spawn_entity(baby);
        } else if rng.random_range(0.0..1.0) < (1.0 / 30.0) {
            self.set_saddled(true);
            let jockey = from_type(
                &EntityType::ZOMBIFIED_PIGLIN,
                self.get_entity().pos.load(),
                world,
                Uuid::new_v4(),
            );
            if let Some(living) = jockey.get_living_entity() {
                if let Ok(mut eq) = living.entity_equipment.lock() {
                    eq.put(
                        &EquipmentSlot::MAIN_HAND,
                        ItemStack::new(1, &Item::WARPED_FUNGUS_ON_A_STICK),
                    );
                }
                living.send_equipment_changes(&[(
                    EquipmentSlot::MAIN_HAND,
                    ItemStack::new(1, &Item::WARPED_FUNGUS_ON_A_STICK),
                )]);
            }
            if let Some(this_base) = world.get_entity_by_id(self.get_entity().entity_id) {
                self.get_entity().add_passenger(this_base, jockey.clone());
            }
            world.spawn_entity(jockey);
        }
        result
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        self.write_ageable_nbt(nbt);
        nbt.put_bool("Saddle", self.is_saddled());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.read_ageable_nbt(nbt);
        if let Some(saddle) = nbt.get_bool("Saddle") {
            self.set_saddled(saddle);
        } else if let Some(saddle_byte) = nbt.get_byte("Saddle") {
            self.set_saddled(saddle_byte == 1);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn get_item_steerable(&self) -> Option<&dyn ItemSteerable> {
        Some(self)
    }

    fn is_saddled(&self) -> bool {
        self.saddled.load(Ordering::Relaxed)
    }

    fn can_be_saddled(&self) -> bool {
        self.mob_entity.living_entity.entity.is_alive() && !self.is_baby()
    }

    fn set_saddled(&self, saddled: bool) {
        self.saddled.store(saddled, Ordering::Relaxed);
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::strider::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_SUFFOCATING,
            self.is_suffocating(),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();

        // strider: saddling with saddle item (Strider.mobInteract)
        if item == &Item::SADDLE && self.can_be_saddled() && !self.is_saddled() {
            self.set_saddled(true);
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            let entity = self.get_entity();
            let world = entity.world.load();
            let pos = entity.pos.load();
            world.play_sound(Sound::EntityStriderSaddle, SoundCategory::Neutral, &pos);
            return true;
        }

        // Boost when player is riding and holding stick
        if self.is_saddled() && item == &Item::WARPED_FUNGUS_ON_A_STICK && self.boost() {
            if player.gamemode.load() != pumpkin_util::GameMode::Creative {
                let _ = item_stack.damage_item(1);
            }
            return true;
        }

        // Riding
        if self.is_saddled() && !self.is_food(item_stack) {
            let world = player.world();
            let ent = &self.mob_entity.living_entity.entity;
            if let Some(vehicle) = world.get_entity_by_id(ent.entity_id)
                && let Some(passenger) = world.get_player_by_id(player.entity_id())
            {
                ent.add_passenger(vehicle, passenger as Arc<dyn EntityBase>);
                return true;
            }
        }

        self.animal_interact(player, item_stack, Sound::EntityStriderAmbient)
    }
}

impl ItemSteerable for StriderEntity {
    // strider: sync boost time (ItemBasedSteering.boost)
    fn boost(&self) -> bool {
        if !self.steering.boost() {
            return false;
        }
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::strider::DATA_BOOST_TIME,
            VarInt(self.steering.boost_time_total.load(Ordering::Relaxed)),
        );
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
