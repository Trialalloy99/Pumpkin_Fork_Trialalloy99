use std::sync::Arc;

use pumpkin_data::BlockState;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::Taggable;
use pumpkin_util::math::position::BlockPos;

use crate::entity::{
    Entity, EntityBase,
    ai::behavior::hunting::Hunt,
    ai::pathfinder::node::PathType,
    ai::target_match::TargetMatch,
    mob::{
        Mob, MobEntity, equipment::RegionalDifficulty, skeleton::SkeletonEntityBase,
        spawn::SpawnGroupData,
    },
    projectile::arrow::ArrowEntity,
};
use crate::world::World;

const FINALIZE_ATTACK_DAMAGE: f64 = 4.0;

const WITHER_SKELETON_HUNTS: &[Hunt] = &[Hunt::new(3, TargetMatch::PIGLIN_LIKE, true)];

pub struct WitherSkeletonEntity {
    entity: Arc<SkeletonEntityBase>,
}

impl WitherSkeletonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = SkeletonEntityBase::with_hunts(entity, WITHER_SKELETON_HUNTS);
        // wither skeleton: setPathfindingMalus LAVA 8
        entity
            .mob_entity
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_pathfinding_malus(PathType::Lava, 8.0);
        let skeleton = Self { entity };
        Arc::new(skeleton)
    }
}

impl Mob for WitherSkeletonEntity {
    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityWitherSkeletonStep, 0.15, 1.0);
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity.mob_entity
    }

    fn get_ambient_sound(&self) -> Option<Sound> {
        Some(Sound::EntityWitherSkeletonAmbient)
    }

    // wither skeleton: no preferred weapon (getPreferredWeaponType null)
    fn preferred_weapon_type(&self) -> Option<&'static pumpkin_data::tag::Tag> {
        None
    }

    // wither skeleton: canHoldItem rejects bows and crossbows
    fn can_hold_item(&self, stack: &ItemStack) -> bool {
        !stack
            .item
            .has_tag(&pumpkin_data::tag::Item::MINECRAFT_WITHER_SKELETON_DISLIKED_WEAPONS)
    }

    // skeleton: no spears (AbstractSkeleton.wantsToPickUp)
    fn mob_wants_to_pick_up(&self, stack: &ItemStack) -> bool {
        !stack
            .item
            .has_tag(&pumpkin_data::tag::Item::MINECRAFT_SPEARS)
    }

    fn post_tick(&self) {
        self.entity.follow_vehicle_body();
    }

    // wither skeleton: stone sword only, no armor (populateDefaultEquipmentSlots)
    fn populate_default_equipment_slots(
        &self,
        _world: &Arc<World>,
        _difficulty: &RegionalDifficulty,
    ) {
        self.entity
            .mob_entity
            .living_entity
            .entity_equipment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(
                &EquipmentSlot::MAIN_HAND,
                ItemStack::new(1, &Item::STONE_SWORD),
            );
    }

    // wither skeleton: no spawn enchantments (populateDefaultEquipmentEnchantments)
    fn populate_default_equipment_enchantments(&self, _difficulty: &RegionalDifficulty) {}

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.entity.finalize_skeleton_spawn(self, world, group_data);
        self.entity
            .mob_entity
            .living_entity
            .set_attribute_base(&Attributes::ATTACK_DAMAGE, FINALIZE_ATTACK_DAMAGE);
        group_data
    }

    fn on_attack(&self, target: &dyn EntityBase) {
        if let Some(living) = target.get_living_entity() {
            living.add_effect(Effect {
                effect_type: &StatusEffect::WITHER,
                duration: 200,
                amplifier: 0,
                ambient: false,
                show_particles: true,
                show_icon: true,
                blend: false,
            });
        }
    }

    fn get_arrow(&self, arrow: ArrowEntity) -> ArrowEntity {
        // vanilla lights it for 100 seconds
        arrow.set_flame(true);
        arrow
    }
}
