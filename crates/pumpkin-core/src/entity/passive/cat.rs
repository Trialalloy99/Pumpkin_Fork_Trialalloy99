use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use pumpkin_data::cat_sound_variant::CatSoundVariant;
use pumpkin_data::cat_variant::CatVariant;
use pumpkin_data::dye_color::DyeColor;
use pumpkin_data::entity::{EntityPose, EntityStatus, EntityType};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::SoundCategory;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::server::actor_event::ActorEventID;
use pumpkin_protocol::codec::var_int::VarInt;
use rand::RngExt;
use uuid::Uuid;

use crate::entity::ai::goal::tempt::TemptGoal;
use crate::entity::custom_sound::CustomSound;
use crate::entity::mob::spawn::SpawnGroupData;
use crate::entity::passive::cat_goals;
use crate::entity::{
    Entity, EntityBase,
    ai::behavior::{
        feline,
        fleeing::{self, Flee},
        hunting::{self, Hunt},
    },
    ai::goal::{
        active_target::TargetCondition, escape_danger::EscapeDangerGoal,
        follow_owner::FollowOwnerGoal, sit_when_ordered_to::SitWhenOrderedToGoal, swim::SwimGoal,
    },
    ai::target_match::TargetMatch,
    mob::{Mob, MobEntity},
    passive::{
        animal::Animal,
        tamable::{TamableAnimal, TamableData, is_untamed},
    },
    player::Player,
};
use crate::world::World;

const CAT_FLEES: &[Flee] = &[feline::flee_players(4, is_untamed)];

const CAT_HUNTS: &[Hunt] = &[
    Hunt::new(1, TargetMatch::Type(&EntityType::RABBIT), false).when(TargetCondition::Untamed),
    hunting::baby_turtles(1, false).when(TargetCondition::Untamed),
];

pub struct CatEntity {
    pub mob_entity: MobEntity,
    pub variant: AtomicU8,
    pub sound_variant: AtomicU8,
    pub collar_color: AtomicU8,
    pub tamable_data: TamableData,
    pub is_lying: AtomicBool,
    pub relax_state_one: AtomicBool,
}

impl CatEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let cat = Self {
            mob_entity,
            variant: AtomicU8::new(CatVariant::Black.id()),
            sound_variant: AtomicU8::new(0), // Default to classic
            collar_color: AtomicU8::new(14), // Default to red
            tamable_data: TamableData::default(),
            is_lying: AtomicBool::new(false),
            relax_state_one: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(cat);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let goal_selector = &mob_arc.mob_entity.goals_selector;

            // Goal 1: SwimGoal (FloatGoal)
            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            // Goal 1: TamableAnimalPanicGoal (EscapeDangerGoal)
            goal_selector.add_goal(1, EscapeDangerGoal::for_tamable(1.5));
            // Goal 2: SitWhenOrderedToGoal
            goal_selector.add_goal(2, Box::new(SitWhenOrderedToGoal::new()));
            goal_selector.add_goal(3, Box::new(cat_goals::CatRelaxOnOwnerGoal::new()));
            // Goal 4: CatTemptGoal
            goal_selector.add_goal(4, feline::cat_tempt_goal());
            // Goal 4: CatAvoidEntityGoal (when untamed)
            fleeing::apply(goal_selector, CAT_FLEES);
            goal_selector.add_goal(5, Box::new(cat_goals::CatLieOnBlockGoal::new()));
            // Goal 6: FollowOwnerGoal
            goal_selector.add_goal(6, FollowOwnerGoal::new(1.0, 10.0, 5.0));
            goal_selector.add_goal(7, Box::new(cat_goals::CatSitOnBlockGoal::new()));
            // Goal 8-12: LeapAtTarget, OcelotAttack, Breed, Stroll, LookAtPlayer
            feline::apply_tail(goal_selector, mob_weak, 8);
        };

        {
            let target_selector = &mob_arc.mob_entity.target_selector;

            // NonTameRandomTargetGoal for Rabbit and Turtle
            hunting::apply(target_selector, &mob_arc.mob_entity, CAT_HUNTS);
        };

        mob_arc
    }

    pub fn get_tame_flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.is_in_sitting_pose() {
            flags |= 0x01;
        }
        if self.is_tame() {
            flags |= 0x04;
        }
        flags
    }

    pub fn is_lying(&self) -> bool {
        self.is_lying.load(Ordering::Relaxed)
    }

    pub fn is_relax_state_one(&self) -> bool {
        self.relax_state_one.load(Ordering::Relaxed)
    }

    pub fn get_collar_color(&self) -> u8 {
        self.collar_color.load(Ordering::Relaxed)
    }

    pub fn set_collar_color(&self, color: u8) {
        self.collar_color.store(color, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::CAT_COLLAR_COLOR,
            VarInt(color as i32),
        );
    }

    pub fn set_tame(&self, tame: bool, owner: Option<Uuid>) {
        self.tamable_data.is_tame.store(tame, Ordering::Relaxed);
        self.set_owner(owner);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::TAMEABLE_FLAGS,
            self.get_tame_flags(),
        );
        entity.set_synced_data(pumpkin_data::tracked_data::cat::OWNER_UUID, owner);
    }

    pub fn set_variant(&self, variant: u8) {
        self.variant.store(variant, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::CAT_VARIANT,
            VarInt(variant as i32),
        );
    }

    pub fn set_lying(&self, lying: bool) {
        self.is_lying.store(lying, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::cat::IS_LYING, lying);
    }

    pub fn set_relax_state_one(&self, relax: bool) {
        self.relax_state_one.store(relax, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(pumpkin_data::tracked_data::cat::RELAX_STATE_ONE, relax);
    }

    fn set_sound_variant_raw(&self, id: u8) {
        if let Some(sound_variant) = CatSoundVariant::from_id(id) {
            self.set_sound_variant(sound_variant);
        }
    }

    fn sound_set(&self) -> CatSoundVariant {
        CatSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed)).unwrap_or_default()
    }

    fn is_baby_cat(&self) -> bool {
        self.get_entity().age.load(Ordering::Relaxed) < 0
    }

    pub fn hiss(&self) {
        let entity = self.get_entity();
        entity.world.load().play_sound_fine(
            self.sound_set().hiss_sound(self.is_baby_cat()),
            SoundCategory::Neutral,
            &entity.pos.load(),
            1.0,
            1.0,
        );
    }

    pub fn set_sound_variant(&self, sound_variant: CatSoundVariant) {
        self.sound_variant
            .store(sound_variant as u8, Ordering::Relaxed);
        let entity = self.get_entity();
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::SOUND_VARIANT,
            VarInt(sound_variant as u8 as i32),
        );
    }
}

impl CustomSound for CatEntity {
    fn death_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        let is_baby = self.get_entity().age.load(Ordering::Relaxed) < 0;
        let sound_variant = CatSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.death_sound(is_baby))
    }

    fn hurt_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        let is_baby = self.get_entity().age.load(Ordering::Relaxed) < 0;
        let sound_variant = CatSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.hurt_sound(is_baby))
    }
}

impl Animal for CatEntity {
    // cat: variant eat sound (Cat.playEatingSound)
    fn eating_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        let is_baby = self.get_entity().age.load(Ordering::Relaxed) < 0;
        let sound_variant = CatSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        Some(sound_variant.eat_sound(is_baby))
    }

    // cat: offspring variant, owner and collar mix (Cat.getBreedOffspring)
    fn get_breed_offspring(&self, partner: &dyn EntityBase) -> Arc<dyn EntityBase> {
        let entity = self.get_entity();
        let world = entity.world.load_full();
        let baby = Self::new(Entity::new(
            world.clone(),
            entity.pos.load(),
            &EntityType::CAT,
        ));
        if let Some(partner) = partner.cast_any().downcast_ref::<Self>() {
            let variant = if rand::random::<bool>() {
                self.variant.load(Ordering::Relaxed)
            } else {
                partner.variant.load(Ordering::Relaxed)
            };
            baby.set_variant(variant);
            baby.set_sound_variant_raw(rand::random_range(0..CatSoundVariant::ALL.len()) as u8);
            if self.is_tame() {
                baby.set_tame(true, self.get_owner());
                let first = DyeColor::from(self.get_collar_color());
                let second = DyeColor::from(partner.get_collar_color());
                let mixed =
                    crate::entity::passive::sheep::find_color_mix_in_recipes(&world, first, second)
                        .unwrap_or(if rand::random::<bool>() {
                            first
                        } else {
                            second
                        });
                baby.set_collar_color(mixed.id());
            }
        }
        baby
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        let item = item_stack.get_item();
        item.has_tag(&tag::Item::MINECRAFT_CAT_FOOD) || item == &Item::COD || item == &Item::SALMON
    }
}

impl TamableAnimal for CatEntity {
    fn get_tamable_data(&self) -> &TamableData {
        &self.tamable_data
    }
}

impl Mob for CatEntity {
    fn mob_is_stepping_carefully(&self) -> bool {
        self.get_mob_entity().living_entity.entity.pose.load() == EntityPose::Crouching
            || self.get_mob_entity().living_entity.entity.is_sneaking()
    }

    fn as_custom_sound(&self) -> Option<&dyn CustomSound> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_tamable(&self) -> Option<&dyn TamableAnimal> {
        Some(self)
    }

    fn mob_metadata(&self) -> Option<crate::entity::mob::MobMetadata> {
        let mut meta = crate::entity::mob::MobMetadata::new();
        if self.get_entity().age.load(Ordering::Relaxed) < 0 {
            meta = meta.baby(true);
        }
        if self.is_tame() {
            meta = meta.tamed(true);
        }
        if self.is_in_sitting_pose() {
            meta = meta.sitting(true);
        }
        meta = meta.collar_color(
            pumpkin_data::tracked_data::cat::CAT_COLLAR_COLOR,
            pumpkin_data::dye_color::DyeColor::from(self.collar_color.load(Ordering::Relaxed)),
        );
        Some(meta)
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        let variant_id = u32::from(self.variant.load(Ordering::Relaxed));
        let variant_str =
            self.resolve_dynamic_entry_id("minecraft:cat_variant", variant_id, |id| {
                CatVariant::all()
                    .get(id as usize)
                    .map_or("minecraft:tabby", CatVariant::asset_id)
                    .to_string()
            });
        nbt.put_string("variant", variant_str);
        let sound_variant = CatSoundVariant::from_id(self.sound_variant.load(Ordering::Relaxed))
            .unwrap_or_default();
        nbt.put_string(
            "sound_variant",
            format!("minecraft:{}", sound_variant.to_name()),
        );
        nbt.put_byte(
            "CollarColor",
            self.collar_color.load(Ordering::Relaxed) as i8,
        );
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        if let Some(variant_str) = nbt.get_string("variant")
            && let Some(id) =
                self.resolve_dynamic_raw_id("minecraft:cat_variant", variant_str, |n| {
                    CatVariant::from_name(n).map(|v| u32::from(v.id()))
                })
            && let Ok(variant_id) = u8::try_from(id)
        {
            self.set_variant(variant_id);
        }
        if let Some(sound_str) = nbt.get_string("sound_variant")
            && let Some(sound_variant) = CatSoundVariant::from_name(sound_str)
        {
            self.sound_variant
                .store(sound_variant as u8, Ordering::Relaxed);
        }
        if let Some(collar) = nbt.get_byte("CollarColor") {
            self.collar_color.store(collar as u8, Ordering::Relaxed);
        } else if let Some(collar_int) = nbt.get_int("CollarColor") {
            self.collar_color.store(collar_int as u8, Ordering::Relaxed);
        }
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_set_variant_name(&self, name: &str) {
        if let Some(id) = self.resolve_dynamic_raw_id("minecraft:cat_variant", name, |n| {
            CatVariant::from_name(n).map(|v| u32::from(v.id()))
        }) && let Ok(variant_id) = u8::try_from(id)
        {
            self.set_variant(variant_id);
        }
    }

    fn mob_set_sound_variant_name(&self, name: &str) {
        if let Some(v) = CatSoundVariant::from_name(name) {
            self.set_sound_variant(v);
        }
    }

    fn mob_init_data_tracker(&self) {
        let entity = self.get_entity();
        let is_baby = entity.age.load(Ordering::Relaxed) < 0;
        if is_baby {
            entity.set_synced_data(pumpkin_data::tracked_data::cat::BABY_ID, true);
        }
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::TAMEABLE_FLAGS,
            self.get_tame_flags(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::OWNER_UUID,
            self.get_owner(),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::CAT_VARIANT,
            VarInt(self.variant.load(Ordering::Relaxed) as i32),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::IS_LYING,
            self.is_lying.load(Ordering::Relaxed),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::RELAX_STATE_ONE,
            self.relax_state_one.load(Ordering::Relaxed),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::CAT_COLLAR_COLOR,
            VarInt(self.collar_color.load(Ordering::Relaxed) as i32),
        );
        entity.set_synced_data(
            pumpkin_data::tracked_data::cat::SOUND_VARIANT,
            VarInt(self.sound_variant.load(Ordering::Relaxed) as i32),
        );
    }

    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let item = item_stack.get_item();
        let is_food = self.is_food(item_stack);

        if self.is_tame() {
            if self.get_owner_uuid() == Some(player.gameprofile.id) {
                if item.has_tag(&tag::Item::MINECRAFT_CAT_COLLAR_DYES)
                    || item.has_tag(&tag::Item::C_DYES)
                {
                    if let Some(dye) = item_stack
                        .get_data_component::<pumpkin_data::data_component_impl::DyeImpl>()
                        && dye.color.id() != self.get_collar_color()
                    {
                        self.set_collar_color(dye.color.id());
                        self.mob_entity
                            .persistence_required
                            .store(true, Ordering::Relaxed);
                        item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                        return true;
                    }
                } else if is_food
                    && self.mob_entity.living_entity.health.load()
                        < self.mob_entity.living_entity.get_max_health()
                {
                    // cat: TamableAnimal.feed(healingFactor 1, defaultHeal 1)
                    let heal = item_stack
                        .get_data_component::<pumpkin_data::data_component_impl::FoodImpl>()
                        .map_or(1.0, |food| food.nutrition as f32);
                    item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                    self.mob_entity.living_entity.heal(heal);
                    self.play_eating_sound();
                    return true;
                }

                let parent_interaction =
                    self.mob_entity
                        .mob_interact(player, item_stack, self.can_be_leashed());
                if !parent_interaction {
                    self.order_to_sit(!self.is_ordered_to_sit());
                    return true;
                }
                return parent_interaction;
            }
        } else if is_food {
            item_stack.decrement_unless_creative(player.gamemode.load(), 1);
            self.play_eating_sound();

            let mut rng = rand::rng();
            if rng.random_range(0..3) == 0 && self.approve_taming(player) {
                self.set_tame(true, Some(player.gameprofile.id));
                self.set_ordered_to_sit(true);
                self.get_entity().world.load().send_entity_status(
                    self.get_entity(),
                    EntityStatus::TamingSucceeded,
                    ActorEventID::TamingSucceeded,
                );
            } else {
                self.get_entity().world.load().send_entity_status(
                    self.get_entity(),
                    EntityStatus::TamingFailed,
                    ActorEventID::TamingFailed,
                );
            }

            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
            return true;
        }

        let interacted = self
            .mob_entity
            .mob_interact(player, item_stack, self.can_be_leashed());
        if interacted {
            self.mob_entity
                .persistence_required
                .store(true, Ordering::Relaxed);
        }
        interacted
    }

    fn can_mate(&self, partner: &dyn EntityBase) -> bool {
        self.is_tame()
            && partner
                .cast_any()
                .downcast_ref::<Self>()
                .is_some_and(TamableAnimal::is_tame)
    }

    fn get_ambient_sound_interval(&self) -> i32 {
        120
    }

    // cat: Cat.getAmbientSound
    fn get_ambient_sound(&self) -> Option<pumpkin_data::sound::Sound> {
        let sounds = self.sound_set();
        let baby = self.is_baby_cat();
        if !self.is_tame() {
            return Some(sounds.stray_ambient_sound());
        }
        if self.mob_entity.is_in_love() {
            Some(sounds.purr_sound(baby))
        } else if rand::random_range(0..4) == 0 {
            Some(sounds.purreow_sound(baby))
        } else {
            Some(sounds.ambient_sound(baby))
        }
    }

    // cat: coat by spawn conditions, random sound set (Cat.finalizeSpawn)
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.finalize_spawn_super(group_data);
        // cat: all_black needs a full moon (moon_brightness >= 0.9), the rest have no condition
        let full_moon = matches!(
            world
                .environment_attributes()
                .get_dimension_value_moon_phase(),
            pumpkin_data::environment_attribute::MoonPhase::FullMoon
        );
        // cat: all_black has priority 1 inside a swamp hut (cats_spawn_as_black), then only it qualifies
        let in_swamp_hut =
            pumpkin_world::generation::generator::structure_finder::structure_spawns_at(
                &world.level.world_gen(),
                &self.get_entity().block_pos.load(),
                "creature",
            )
            .is_some_and(|spawns| {
                spawns
                    .iter()
                    .any(|entry| entry.entity_type == "minecraft:cat")
            });
        let candidates: Vec<CatVariant> = [
            CatVariant::AllBlack,
            CatVariant::Black,
            CatVariant::BritishShorthair,
            CatVariant::Calico,
            CatVariant::Jellie,
            CatVariant::Persian,
            CatVariant::Ragdoll,
            CatVariant::Red,
            CatVariant::Siamese,
            CatVariant::Tabby,
            CatVariant::White,
        ]
        .into_iter()
        .filter(|variant| {
            if in_swamp_hut {
                *variant == CatVariant::AllBlack
            } else {
                *variant != CatVariant::AllBlack || full_moon
            }
        })
        .collect();
        let variant = candidates[rand::random_range(0..candidates.len())];
        self.set_variant(variant.id());
        self.set_sound_variant_raw(rand::random_range(0..CatSoundVariant::ALL.len()) as u8);
        group_data
    }

    // cat: BABY_DIMENSIONS
    fn baby_dimensions(&self) -> Option<pumpkin_util::math::boundingbox::EntityDimensions> {
        Some(pumpkin_util::math::boundingbox::EntityDimensions {
            width: 0.3,
            height: 0.35,
            eye_height: 0.343_75,
        })
    }

    // cat: crouch and sprint follow the move speed (Cat.customServerAiStep)
    fn custom_server_ai_step(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let (pose, sprinting) = {
            let steered = self.mob_entity.steered();
            let control = steered.move_control();
            if control.has_wanted() {
                let speed = control.speed_modifier();
                if speed == 0.6 {
                    (EntityPose::Crouching, false)
                } else if speed == 1.33 {
                    (EntityPose::Standing, true)
                } else {
                    (EntityPose::Standing, false)
                }
            } else {
                (EntityPose::Standing, false)
            }
        };
        entity.set_pose(pose);
        entity.set_sprinting(sprinting);
    }

    // cat: begging (Cat.tick) and purring while lying (Cat.handleLieDown)
    fn mob_tick(&self, _caller: &dyn EntityBase) {
        let entity = self.get_entity();
        let tick_count = entity.tick_count.load(Ordering::Relaxed);
        let sounds = self.sound_set();
        let baby = self.is_baby_cat();
        let world = entity.world.load();
        if !self.is_tame()
            && tick_count % 100 == 0
            && self.mob_entity.goals_selector.is_running::<TemptGoal>()
        {
            world.play_sound_fine(
                sounds.beg_for_food_sound(baby),
                SoundCategory::Neutral,
                &entity.pos.load(),
                1.0,
                1.0,
            );
        }
        if (self.is_lying() || self.is_relax_state_one()) && tick_count % 5 == 0 {
            world.play_sound_fine(
                sounds.purr_sound(baby),
                SoundCategory::Neutral,
                &entity.pos.load(),
                0.6 + 0.4 * (rand::random::<f32>() - rand::random::<f32>()),
                1.0,
            );
        }
    }

    fn remove_when_far_away(&self, _distance_sq: f64) -> bool {
        !self.is_tame() && self.get_entity().tick_count.load(Ordering::Relaxed) > 2400
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::entity::ai::test_support::world;
    use pumpkin_util::math::vector3::Vector3;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn a_crouching_cat_steps_carefully_and_a_standing_one_does_not() -> TestResult {
        let folder = tempfile::tempdir()?;
        let world = world(&folder);
        let cat = CatEntity::new(Entity::new(
            world.clone(),
            Vector3::new(0.5, 64.0, 0.5),
            &EntityType::CAT,
        ));
        assert!(!cat.is_stepping_carefully());
        cat.get_entity().pose.store(EntityPose::Crouching);
        assert!(cat.is_stepping_carefully());
        world.level.shutdown().await;
        Ok(())
    }
}
