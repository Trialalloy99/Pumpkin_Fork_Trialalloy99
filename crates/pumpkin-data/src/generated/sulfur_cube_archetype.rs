/* This file is generated. Do not edit manually. */
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierOperation {
    Add,
    MultiplyBase,
    MultiplyTotal,
}
#[derive(Clone, Copy, Debug)]
pub struct AttributeEntry {
    pub attribute: &'static crate::attributes::Attributes,
    pub id: &'static str,
    pub amount: f64,
    pub operation: ModifierOperation,
}
#[derive(Clone, Copy, Debug)]
pub struct ExplosionData {
    pub power: u32,
    pub causes_fire: bool,
    pub fuse: i32,
}
#[derive(Clone, Copy, Debug)]
pub struct ContactDamage {
    pub damage_type: &'static crate::damage::DamageType,
    pub amount: f32,
    pub attribute_to_source: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct KnockbackModifiers {
    pub horizontal_power: f32,
    pub vertical_power: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct SoundSettings {
    pub hit_sound: crate::sound::Sound,
    pub push_sound: crate::sound::Sound,
    pub push_sound_cooldown: f32,
    pub push_sound_impulse_threshold: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct SulfurCubeArchetype {
    pub name: &'static str,
    pub items: &'static crate::tag::Tag,
    pub attribute_modifiers: &'static [AttributeEntry],
    pub buoyant: bool,
    pub explosion: Option<ExplosionData>,
    pub contact_damage: Option<ContactDamage>,
    pub knockback_modifiers: KnockbackModifiers,
    pub sound_settings: SoundSettings,
}
pub const DEFAULT_KNOCKBACK_MODIFIERS: KnockbackModifiers = KnockbackModifiers {
    horizontal_power: 0.33,
    vertical_power: 0.06,
};
pub const DEFAULT_SOUND_SETTINGS: SoundSettings = SoundSettings {
    hit_sound: crate::sound::Sound::EntitySulfurCubeRegularHit,
    push_sound: crate::sound::Sound::EntitySulfurCubeRegularPush,
    push_sound_cooldown: 0.5,
    push_sound_impulse_threshold: 0.2,
};
pub const SULFUR_CUBE_ARCHETYPES: &[SulfurCubeArchetype] = &[
    SulfurCubeArchetype {
        name: "minecraft:bouncy",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_BOUNCY,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:bouncy_add_knockback_resistance",
                amount: -2f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:bouncy_add_explosion_knockback_resistance",
                amount: -2f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:bouncy_add_bounciness",
                amount: 0.8999999761581421f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:bouncy_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:bouncy_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: true,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.105f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeBouncyHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeBouncyPush,
            push_sound_cooldown: 0.7f32,
            push_sound_impulse_threshold: 0.3f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:explosive",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_EXPLOSIVE,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:explosive_add_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:explosive_add_explosion_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:explosive_add_bounciness",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:explosive_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:explosive_mul_air_drag_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: true,
        explosion: Some(ExplosionData {
            power: 3u32,
            causes_fire: false,
            fuse: 120i32,
        }),
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeExplosiveHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeExplosivePush,
            push_sound_cooldown: 0.7f32,
            push_sound_impulse_threshold: 0.1f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:fast_flat",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_FAST_FLAT,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:fast_flat_add_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:fast_flat_add_explosion_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:fast_flat_add_bounciness",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:fast_flat_mul_friction_modifier",
                amount: -0.7999999970197678f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:fast_flat_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.9125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeFastFlatHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeFastFlatPush,
            push_sound_cooldown: 0.9f32,
            push_sound_impulse_threshold: 0.03f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:fast_sliding",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_FAST_SLIDING,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:fast_sliding_add_knockback_resistance",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:fast_sliding_add_explosion_knockback_resistance",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:fast_sliding_add_bounciness",
                amount: 0.10000000149011612f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:fast_sliding_mul_friction_modifier",
                amount: -0.949999999254942f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:fast_sliding_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.6625f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeFastSlidingHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeFastSlidingPush,
            push_sound_cooldown: 1f32,
            push_sound_impulse_threshold: 0.05f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:high_resistance",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_HIGH_RESISTANCE,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:high_resistance_add_knockback_resistance",
                amount: 0.699999988079071f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:high_resistance_add_explosion_knockback_resistance",
                amount: 0.699999988079071f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:high_resistance_add_bounciness",
                amount: 0.20000000298023224f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:high_resistance_mul_friction_modifier",
                amount: 0f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:high_resistance_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeHighResistanceHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeHighResistancePush,
            push_sound_cooldown: 0.7f32,
            push_sound_impulse_threshold: 0.03f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:hot",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_HOT,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:hot_add_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:hot_add_explosion_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:hot_add_bounciness",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:hot_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:hot_mul_air_drag_modifier",
                amount: -0.8999999985098839f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: true,
        explosion: None,
        contact_damage: Some(ContactDamage {
            damage_type: &crate::damage::DamageType::SULFUR_CUBE_HOT,
            amount: 1f32,
            attribute_to_source: false,
        }),
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeHotHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeHotPush,
            push_sound_cooldown: 0.7f32,
            push_sound_impulse_threshold: 0.2f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:light",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_LIGHT,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:light_add_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:light_add_explosion_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:light_add_bounciness",
                amount: 1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:light_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:light_mul_air_drag_modifier",
                amount: 0.7999999523162842f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: true,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.18f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeLightHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeLightPush,
            push_sound_cooldown: 0.7f32,
            push_sound_impulse_threshold: 0.2f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:regular",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_REGULAR,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:regular_add_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:regular_add_explosion_knockback_resistance",
                amount: -1f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:regular_add_bounciness",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:regular_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:regular_mul_air_drag_modifier",
                amount: -0.8999999985098839f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: true,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeRegularHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeRegularPush,
            push_sound_cooldown: 0.5f32,
            push_sound_impulse_threshold: 0.2f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:slow_bouncy",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_SLOW_BOUNCY,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_bouncy_add_knockback_resistance",
                amount: 0.4000000059604645f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_bouncy_add_explosion_knockback_resistance",
                amount: 0.4000000059604645f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:slow_bouncy_add_bounciness",
                amount: 0.6000000238418579f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:slow_bouncy_mul_friction_modifier",
                amount: -0.699999988079071f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:slow_bouncy_mul_air_drag_modifier",
                amount: -0.949999999254942f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.24f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeSlowBouncyHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeSlowBouncyPush,
            push_sound_cooldown: 0.5f32,
            push_sound_impulse_threshold: 0.05f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:slow_flat",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_SLOW_FLAT,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_flat_add_knockback_resistance",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_flat_add_explosion_knockback_resistance",
                amount: 0.5f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:slow_flat_add_bounciness",
                amount: 0.4000000059604645f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:slow_flat_mul_friction_modifier",
                amount: -0.5999999940395355f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:slow_flat_mul_air_drag_modifier",
                amount: -0.8999999985098839f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.105f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeSlowFlatHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeSlowFlatPush,
            push_sound_cooldown: 0.9f32,
            push_sound_impulse_threshold: 0.03f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:slow_sliding",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_SLOW_SLIDING,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_sliding_add_knockback_resistance",
                amount: 0.800000011920929f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:slow_sliding_add_explosion_knockback_resistance",
                amount: 0.800000011920929f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:slow_sliding_add_bounciness",
                amount: 0.10000000149011612f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:slow_sliding_mul_friction_modifier",
                amount: -0.949999999254942f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:slow_sliding_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeSlowSlidingHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeSlowSlidingPush,
            push_sound_cooldown: 1f32,
            push_sound_impulse_threshold: 0.02f32,
        },
    },
    SulfurCubeArchetype {
        name: "minecraft:sticky",
        items: &crate::tag::Item::MINECRAFT_SULFUR_CUBE_ARCHETYPE_STICKY,
        attribute_modifiers: &[
            AttributeEntry {
                attribute: &crate::attributes::Attributes::KNOCKBACK_RESISTANCE,
                id: "minecraft:sticky_add_knockback_resistance",
                amount: -2f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::EXPLOSION_KNOCKBACK_RESISTANCE,
                id: "minecraft:sticky_add_explosion_knockback_resistance",
                amount: -2f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::BOUNCINESS,
                id: "minecraft:sticky_add_bounciness",
                amount: 0f64,
                operation: ModifierOperation::Add,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::FRICTION_MODIFIER,
                id: "minecraft:sticky_mul_friction_modifier",
                amount: 1f64,
                operation: ModifierOperation::MultiplyTotal,
            },
            AttributeEntry {
                attribute: &crate::attributes::Attributes::AIR_DRAG_MODIFIER,
                id: "minecraft:sticky_mul_air_drag_modifier",
                amount: -0.9900000002235174f64,
                operation: ModifierOperation::MultiplyTotal,
            },
        ],
        buoyant: false,
        explosion: None,
        contact_damage: None,
        knockback_modifiers: KnockbackModifiers {
            horizontal_power: 0.4125f32,
            vertical_power: 0.09f32,
        },
        sound_settings: SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeStickyHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeStickyPush,
            push_sound_cooldown: 0.5f32,
            push_sound_impulse_threshold: 0.05f32,
        },
    },
];
