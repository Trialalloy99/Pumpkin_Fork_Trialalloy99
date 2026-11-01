use pumpkin_data::BlockState;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use rand::RngExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::entity::item::ItemEntity;
use crate::entity::player::Player;
use crate::entity::{
    Entity, EntityBase,
    mob::{
        Mob,
        skeleton::{SkeletonEntityBase, abstract_skeleton_mob},
    },
    projectile::arrow::ArrowEntity,
};
use crate::world::loot::{LootContextParameters, generate_loot_from_handle};

pub struct BoggedSkeletonEntity {
    entity: Arc<SkeletonEntityBase>,
    sheared: AtomicBool,
}

impl BoggedSkeletonEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = SkeletonEntityBase::new(entity);
        Arc::new(Self {
            entity,
            sheared: AtomicBool::new(false),
        })
    }

    pub fn is_sheared(&self) -> bool {
        self.sheared.load(Ordering::Relaxed)
    }

    pub fn set_sheared(&self, sheared: bool) {
        self.sheared.store(sheared, Ordering::Relaxed);
        self.get_entity()
            .set_synced_data(pumpkin_data::tracked_data::bogged::DATA_SHEARED, sheared);
    }

    // bogged: shear sound, mushroom drops at head height (Bogged.shear, spawnShearedMushrooms)
    pub fn shear(&self, category: SoundCategory, tool: &ItemStack) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let pos = entity.pos.load();
        world.play_sound(Sound::EntityBoggedShear, category, &pos);
        if let Some(table) = world.get_loot_table("minecraft:shearing/bogged") {
            let params = LootContextParameters {
                world: Some(&world),
                position: Some(pos),
                this_entity: Some(self),
                tool: Some(tool.clone()),
                ..Default::default()
            };
            let height = f64::from(entity.height());
            let mut rng = self.get_random();
            for stack in generate_loot_from_handle(&table, rng.random(), &params) {
                let item_entity = ItemEntity::new(
                    Entity::new(
                        world.clone(),
                        pos.add_raw(0.0, height, 0.0),
                        &EntityType::ITEM,
                    ),
                    stack,
                );
                world.spawn_entity(Arc::new(item_entity));
            }
        }
        self.set_sheared(true);
    }
}

impl Mob for BoggedSkeletonEntity {
    abstract_skeleton_mob!(entity);

    fn mob_play_step_sound(&self, _pos: &BlockPos, _state: &BlockState) {
        self.play_entity_sound(Sound::EntityBoggedStep, 0.15, 1.0);
    }

    // bogged: poison arrows, 100 ticks (Bogged.getArrow)
    fn get_arrow(&self, arrow: ArrowEntity) -> ArrowEntity {
        arrow.add_effect(&StatusEffect::POISON, 100);
        arrow
    }

    // bogged: slower bow (Bogged.getHardAttackInterval/getAttackInterval)
    fn bow_attack_intervals(&self) -> (i32, i32) {
        (50, 70)
    }

    // bogged: shears once (Bogged.mobInteract)
    fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        if item_stack.get_item() != &Item::SHEARS || self.is_sheared() {
            return false;
        }
        self.shear(SoundCategory::Players, item_stack);
        let entity = self.get_entity();
        entity
            .world
            .load()
            .emit_game_event(GameEvent::Shear.name(), entity.pos.load());
        let _ = player.damage_item_stack(item_stack, 1);
        true
    }

    fn mob_init_data_tracker(&self) {
        self.get_entity().set_synced_data(
            pumpkin_data::tracked_data::bogged::DATA_SHEARED,
            self.is_sheared(),
        );
    }

    fn mob_write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("sheared", self.is_sheared());
    }

    fn mob_read_nbt(&self, nbt: &NbtCompound) {
        self.set_sheared(nbt.get_bool("sheared").unwrap_or(false));
    }
}
