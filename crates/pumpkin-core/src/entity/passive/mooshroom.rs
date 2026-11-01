use std::sync::{
    Arc, Weak,
    atomic::{AtomicI32, Ordering},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::BlockState;
use pumpkin_data::cow_sound_variant::CowSoundVariant;
use pumpkin_data::data_component_impl::{
    DataComponentImpl, SuspiciousStewEffect, SuspiciousStewEffectsImpl,
};
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;
use uuid::Uuid;

use crate::world::loot::{LootContextParameters, generate_loot_from_handle};

use crate::entity::{
    Entity, EntityBase,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    custom_sound::CustomSound,
    item::ItemEntity,
    mob::{Mob, MobEntity, conversion},
    passive::animal::Animal,
    player::Player,
};

const TEMPT_ITEMS: &[&Item] = &[&Item::WHEAT];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum MooshroomVariant {
    #[default]
    Red = 0,
    Brown = 1,
}

impl MooshroomVariant {
    #[must_use]
    pub const fn from_id(id: i32) -> Self {
        match id {
            1 => Self::Brown,
            _ => Self::Red,
        }
    }

    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Brown => "brown",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "brown" => Self::Brown,
            _ => Self::Red,
        }
    }
}

pub struct MooshroomEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub variant: AtomicI32,
    pub stew_effects: std::sync::Mutex<Option<SuspiciousStewEffectsImpl>>,
    pub last_lightning_bolt_uuid: AtomicCell<Option<Uuid>>,
}

impl MooshroomEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let mooshroom = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            variant: AtomicI32::new(MooshroomVariant::Red.id()),
            stew_effects: std::sync::Mutex::new(None),
            last_lightning_bolt_uuid: AtomicCell::new(None),
        };
        let mob_arc = Arc::new(mooshroom);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, EscapeDangerGoal::new(2.0));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(3, Box::new(TemptGoal::new(1.25, TEMPT_ITEMS, false)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.25)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::water_avoiding(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }

    #[must_use]
    pub fn get_variant(&self) -> MooshroomVariant {
        MooshroomVariant::from_id(self.variant.load(Ordering::Relaxed))
    }

    pub fn set_variant(&self, variant: MooshroomVariant) {
        self.variant.store(variant.id(), Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::mooshroom::DATA_TYPE,
            VarInt(variant.id()),
        );
    }

    fn fill_item_stack(
        player: &Player,
        world: &Arc<crate::world::World>,
        item_stack: &mut ItemStack,
        mut filled: ItemStack,
    ) {
        let gamemode = player.gamemode.load();
        if gamemode != pumpkin_util::GameMode::Creative && item_stack.item_count == 1 {
            *item_stack = filled;
        } else {
            item_stack.decrement_unless_creative(gamemode, 1);
            if !player.inventory.insert_stack_anywhere(&mut filled) && !filled.is_empty() {
                world.drop_stack(&player.position().to_block_pos(), filled);
            }
        }
    }

    // mooshroom: shear into a cow, drop the variant's mushrooms (MushroomCow.shear)
    pub fn shear(&self, category: SoundCategory, tool: &ItemStack) {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();
        world.play_sound(Sound::EntityMooshroomShear, category, &pos);
        let height = f64::from(entity.height());
        let variant = self.get_variant();
        let tool = tool.clone();
        let rolled = self.get_random().random::<i64>();
        conversion::convert_single(self, &EntityType::COW, false, false, |_cow| {
            world.spawn_particle(
                pos + Vector3::new(0.0, height * 0.5, 0.0),
                Vector3::new(0.0, 0.0, 0.0),
                0.0,
                1,
                Particle::Explosion,
            );
            let key = format!("minecraft:shearing/mooshroom/{}", variant.as_str());
            if let Some(table) = world.get_loot_table(&key) {
                let params = LootContextParameters {
                    world: Some(&world),
                    position: Some(pos),
                    this_entity: Some(self),
                    tool: Some(tool.clone()),
                    ..Default::default()
                };
                for drop in generate_loot_from_handle(&table, rolled, &params) {
                    for _ in 0..drop.item_count {
                        world.spawn_entity(Arc::new(ItemEntity::new(
                            Entity::new(
                                world.clone(),
                                pos + Vector3::new(0.0, height, 0.0),
                                &EntityType::ITEM,
                            ),
                            drop.copy_with_count(1),
                        )));
                    }
                }
            }
        });
    }

    // mooshroom: flower -> suspicious stew effect (FlowerBlock/EyeblossomBlock stew effects)
    fn flower_stew_effect(item: &Item) -> Option<(&'static str, i32)> {
        Some(match item {
            i if i == &Item::DANDELION
                || i == &Item::GOLDEN_DANDELION
                || i == &Item::BLUE_ORCHID =>
            {
                ("minecraft:saturation", 7)
            }
            i if i == &Item::TORCHFLOWER || i == &Item::POPPY => ("minecraft:night_vision", 100),
            i if i == &Item::ALLIUM => ("minecraft:fire_resistance", 60),
            i if i == &Item::AZURE_BLUET || i == &Item::OPEN_EYEBLOSSOM => {
                ("minecraft:blindness", 220)
            }
            i if i == &Item::RED_TULIP
                || i == &Item::ORANGE_TULIP
                || i == &Item::WHITE_TULIP
                || i == &Item::PINK_TULIP =>
            {
                ("minecraft:weakness", 140)
            }
            i if i == &Item::OXEYE_DAISY => ("minecraft:regeneration", 140),
            i if i == &Item::CORNFLOWER => ("minecraft:jump_boost", 100),
            i if i == &Item::WITHER_ROSE => ("minecraft:wither", 140),
            i if i == &Item::LILY_OF_THE_VALLEY => ("minecraft:poison", 220),
            i if i == &Item::CLOSED_EYEBLOSSOM => ("minecraft:nausea", 140),
            _ => return None,
        })
    }
}

impl AgeableMob for MooshroomEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for MooshroomEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        item_stack.item.has_tag(&tag::Item::MINECRAFT_COW_FOOD)
            || TEMPT_ITEMS.iter().any(|i| i.id == item_stack.item.id)
    }

    // mooshroom: parent variant, 1/1024 flip if both match (MushroomCow.getOffspringVariant)
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let baby = Self::new(Entity::new(
            entity.world.load_full(),
            entity.pos.load(),
            &EntityType::MOOSHROOM,
        ));
        if let Some(partner) = partner.cast_any().downcast_ref::<Self>() {
            let mut random = self.get_random();
            let (own, mate) = (self.get_variant(), partner.get_variant());
            let variant = if own == mate && random.random_range(0..1024) == 0 {
                if own == MooshroomVariant::Brown {
                    MooshroomVariant::Red
                } else {
                    MooshroomVariant::Brown
                }
            } else if random.random::<bool>() {
                own
            } else {
                mate
            };
            baby.variant.store(variant.id(), Ordering::Relaxed);
        }
        baby
    }
}

impl CustomSound for MooshroomEntity {
    fn death_sound(&self) -> Option<Sound> {
        Some(CowSoundVariant::Classic.death_sound(self.is_baby()))
    }

    fn hurt_sound(&self) -> Option<Sound> {
        Some(CowSoundVariant::Classic.hurt_sound(self.is_baby()))
    }

    // mooshroom: quieter than default (AbstractCow.getSoundVolume)
    fn sound_volume(&self) -> f32 {
        0.4
    }
}

impl Mob for MooshroomEntity {
    // mooshroom: own baby hitbox (MushroomCow.BABY_DIMENSIONS)
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.45,
            height: 0.7,
            eye_height: 0.69,
        })
    }

    /// Vanilla `AbstractCow.getSoundSet` is the classic cow for mooshrooms.
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        let sound = CowSoundVariant::Classic.step_sound(self.is_baby());
        self.play_entity_sound(sound, 0.15, 1.0);
    }

    fn mob_get_component(
        &self,
        component: pumpkin_data::data_component::DataComponent,
    ) -> Option<Box<dyn pumpkin_data::data_component_impl::DataComponentImpl>> {
        use pumpkin_data::{
            data_component::DataComponent, data_component_impl::MooshroomVariantImpl,
        };
        match component {
            DataComponent::MooshroomVariant => Some(Box::new(MooshroomVariantImpl {
                value: self.get_variant().as_str().into(),
            })),
            _ => None,
        }
    }

    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_string("Type", self.get_variant().as_str().to_string());
        if let Some(effects) = &*self
            .stew_effects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            nbt.put("stew_effects", effects.write_data());
        }
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(variant_str) = nbt.get_string("Type") {
            self.set_variant(MooshroomVariant::from_name(variant_str));
        }
        *self
            .stew_effects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = nbt
            .get("stew_effects")
            .and_then(SuspiciousStewEffectsImpl::read_data);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::mooshroom::DATA_BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::mooshroom::DATA_TYPE,
            VarInt(self.get_variant().id()),
        );
    }

    // mooshroom: stew, milk, shear, feed flowers (MushroomCow/AbstractCow.mobInteract)
    #[expect(clippy::too_many_lines, reason = "mirrors MushroomCow.mobInteract")]
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let pos = entity.pos.load();

        if item == &Item::BOWL && !self.is_baby() {
            let effects = self
                .stew_effects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let is_suspicious = effects.is_some();
            let mut stew = ItemStack::new(
                1,
                if is_suspicious {
                    &Item::SUSPICIOUS_STEW
                } else {
                    &Item::MUSHROOM_STEW
                },
            );
            if let Some(effects) = effects {
                stew.set_data_component(effects);
            }
            Self::fill_item_stack(player, &world, item_stack, stew);
            self.play_entity_sound(
                if is_suspicious {
                    Sound::EntityMooshroomSuspiciousMilk
                } else {
                    Sound::EntityMooshroomMilk
                },
                1.0,
                1.0,
            );
            return true;
        }

        if item == &Item::BUCKET && !self.is_baby() {
            world.play_sound_expect(
                player,
                Sound::EntityCowMilk,
                SoundCategory::Players,
                &player.position(),
            );
            Self::fill_item_stack(
                player,
                &world,
                item_stack,
                ItemStack::new(1, &Item::MILK_BUCKET),
            );
            return true;
        }

        if item == &Item::SHEARS && !self.is_baby() {
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
            world.emit_game_event(GameEvent::Shear.name(), pos);
            let _ = player.damage_item_stack(item_stack, 1);
            return true;
        }

        if self.get_variant() == MooshroomVariant::Brown
            && !self.is_baby()
            && let Some((effect, duration)) = Self::flower_stew_effect(item)
        {
            let mut stew = self
                .stew_effects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if stew.is_some() {
                world.spawn_particle(
                    pos + Vector3::new(0.0, f64::from(entity.height()) * 0.5, 0.0),
                    Vector3::new(0.5, 0.0, 0.5),
                    0.0,
                    2,
                    Particle::Smoke,
                );
            } else {
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                world.spawn_particle(
                    pos + Vector3::new(0.0, f64::from(entity.height()) * 0.5, 0.0),
                    Vector3::new(0.5, 0.0, 0.5),
                    0.0,
                    4,
                    Particle::Effect,
                );
                *stew = Some(SuspiciousStewEffectsImpl {
                    effects: std::borrow::Cow::Owned(vec![SuspiciousStewEffect {
                        effect: std::borrow::Cow::Borrowed(effect),
                        duration,
                    }]),
                });
                drop(stew);
                self.play_entity_sound(Sound::EntityMooshroomEat, 2.0, 1.0);
            }
            return true;
        }

        self.animal_interact(
            player,
            item_stack,
            CowSoundVariant::Classic.ambient_sound(self.is_baby()),
        )
    }

    // mooshroom: lightning swaps red/brown once per bolt, no damage (MushroomCow.thunderHit)
    fn mob_on_lightning_strike(
        &self,
        _caller: &dyn EntityBase,
        lightning: &crate::entity::lightning::LightningBoltEntity,
    ) {
        let bolt = lightning.get_entity().entity_uuid;
        if self.last_lightning_bolt_uuid.swap(Some(bolt)) == Some(bolt) {
            return;
        }
        self.set_variant(if self.get_variant() == MooshroomVariant::Red {
            MooshroomVariant::Brown
        } else {
            MooshroomVariant::Red
        });
        self.play_entity_sound(Sound::EntityMooshroomConvert, 2.0, 1.0);
    }

    // mooshroom: likes mycelium (MushroomCow.getWalkTargetValue)
    fn get_walk_target_value(&self, pos: &BlockPos) -> f32 {
        let world = self.get_entity().world.load();
        if world.get_block(&pos.down()) == &pumpkin_data::Block::MYCELIUM {
            10.0
        } else {
            world.get_light_level_dependent_magic_value(pos) - 0.5
        }
    }

    // mooshroom: classic cow ambient (AbstractCow.getAmbientSound)
    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(CowSoundVariant::Classic.ambient_sound(self.is_baby()))
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }
}
