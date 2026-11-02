use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use heck::{ToPascalCase, ToShoutySnakeCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;

#[derive(Deserialize)]
struct ArchetypeJson {
    items: String,
    attribute_modifiers: Vec<AttributeJson>,
    #[serde(default)]
    buoyant: bool,
    explosion: Option<ExplosionJson>,
    contact_damage: Option<ContactDamageJson>,
    knockback_modifiers: KnockbackJson,
    sound_settings: SoundJson,
}

#[derive(Deserialize)]
struct AttributeJson {
    attribute: String,
    id: String,
    amount: f64,
    operation: String,
}

#[derive(Deserialize)]
struct ExplosionJson {
    power: u32,
    causes_fire: bool,
    fuse: i32,
}

#[derive(Deserialize)]
struct ContactDamageJson {
    damage_type: String,
    amount: f32,
    attribute_to_source: bool,
}

#[derive(Deserialize)]
struct KnockbackJson {
    horizontal_power: f32,
    vertical_power: f32,
}

#[derive(Deserialize)]
struct SoundJson {
    hit_sound: String,
    push_sound: String,
    push_sound_cooldown: f32,
    push_sound_impulse_threshold: f32,
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn sound_ident(name: &str) -> proc_macro2::Ident {
    format_ident!("{}", strip(name).replace('.', "_").to_pascal_case())
}

pub fn build() -> TokenStream {
    let dir = Path::new("../../assets/datapack/data/minecraft/sulfur_cube_archetype");

    let mut archetypes: BTreeMap<String, ArchetypeJson> = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("read sulfur_cube_archetype dir") {
        let path = entry.expect("read dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let content = fs::read_to_string(&path).expect("read sulfur_cube_archetype file");
        archetypes.insert(
            stem,
            serde_json::from_str(&content).expect("parse sulfur_cube_archetype JSON"),
        );
    }

    let entries = archetypes.iter().map(|(name, json)| {
        let items = format_ident!(
            "MINECRAFT_{}",
            json.items
                .trim_start_matches('#')
                .trim_start_matches("minecraft:")
                .replace('/', "_")
                .to_shouty_snake_case()
        );
        let modifiers = json.attribute_modifiers.iter().map(|modifier| {
            let attribute = format_ident!("{}", strip(&modifier.attribute).to_shouty_snake_case());
            let id = &modifier.id;
            let amount = modifier.amount;
            let operation = match modifier.operation.as_str() {
                "add_value" => quote! { ModifierOperation::Add },
                "add_multiplied_base" => quote! { ModifierOperation::MultiplyBase },
                "add_multiplied_total" => quote! { ModifierOperation::MultiplyTotal },
                other => panic!("unknown attribute operation {other}"),
            };
            quote! {
                AttributeEntry {
                    attribute: &crate::attributes::Attributes::#attribute,
                    id: #id,
                    amount: #amount,
                    operation: #operation,
                }
            }
        });
        let buoyant = json.buoyant;
        let explosion = json.explosion.as_ref().map_or_else(
            || quote! { None },
            |e| {
                let (power, causes_fire, fuse) = (e.power, e.causes_fire, e.fuse);
                quote! { Some(ExplosionData { power: #power, causes_fire: #causes_fire, fuse: #fuse }) }
            },
        );
        let contact_damage = json.contact_damage.as_ref().map_or_else(
            || quote! { None },
            |c| {
                let damage_type = format_ident!("{}", strip(&c.damage_type).to_shouty_snake_case());
                let (amount, attribute_to_source) = (c.amount, c.attribute_to_source);
                quote! {
                    Some(ContactDamage {
                        damage_type: &crate::damage::DamageType::#damage_type,
                        amount: #amount,
                        attribute_to_source: #attribute_to_source,
                    })
                }
            },
        );
        let (horizontal_power, vertical_power) = (
            json.knockback_modifiers.horizontal_power,
            json.knockback_modifiers.vertical_power,
        );
        let hit_sound = sound_ident(&json.sound_settings.hit_sound);
        let push_sound = sound_ident(&json.sound_settings.push_sound);
        let push_sound_cooldown = json.sound_settings.push_sound_cooldown;
        let push_sound_impulse_threshold = json.sound_settings.push_sound_impulse_threshold;
        let name = format!("minecraft:{name}");
        quote! {
            SulfurCubeArchetype {
                name: #name,
                items: &crate::tag::Item::#items,
                attribute_modifiers: &[#(#modifiers),*],
                buoyant: #buoyant,
                explosion: #explosion,
                contact_damage: #contact_damage,
                knockback_modifiers: KnockbackModifiers {
                    horizontal_power: #horizontal_power,
                    vertical_power: #vertical_power,
                },
                sound_settings: SoundSettings {
                    hit_sound: crate::sound::Sound::#hit_sound,
                    push_sound: crate::sound::Sound::#push_sound,
                    push_sound_cooldown: #push_sound_cooldown,
                    push_sound_impulse_threshold: #push_sound_impulse_threshold,
                },
            }
        }
    });

    quote! {
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

        // SulfurCubeArchetype.DEFAULT_KNOCKBACK_MODIFIERS
        pub const DEFAULT_KNOCKBACK_MODIFIERS: KnockbackModifiers = KnockbackModifiers {
            horizontal_power: 0.33,
            vertical_power: 0.06,
        };

        // SulfurCubeArchetype.DEFAULT_SOUND_SETTINGS
        pub const DEFAULT_SOUND_SETTINGS: SoundSettings = SoundSettings {
            hit_sound: crate::sound::Sound::EntitySulfurCubeRegularHit,
            push_sound: crate::sound::Sound::EntitySulfurCubeRegularPush,
            push_sound_cooldown: 0.5,
            push_sound_impulse_threshold: 0.2,
        };

        pub const SULFUR_CUBE_ARCHETYPES: &[SulfurCubeArchetype] = &[#(#entries),*];
    }
}
