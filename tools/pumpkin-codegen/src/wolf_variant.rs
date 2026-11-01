use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use heck::ToPascalCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;

#[derive(Deserialize)]
struct WolfVariantJson {
    assets: WolfAssetsJson,
    #[serde(default)]
    baby_assets: Option<WolfAssetsJson>,
    #[serde(default)]
    spawn_conditions: Vec<SpawnConditionJson>,
}

#[derive(Deserialize)]
struct SpawnConditionJson {
    #[serde(default)]
    condition: Option<BiomeConditionJson>,
    priority: i32,
}

#[derive(Deserialize)]
struct BiomeConditionJson {
    biomes: BiomesJson,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BiomesJson {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct WolfAssetsJson {
    angry: String,
    tame: String,
    wild: String,
}

pub fn build() -> TokenStream {
    let dir = Path::new("../../assets/datapack/data/minecraft/wolf_variant");

    let mut variants: BTreeMap<String, WolfVariantJson> = BTreeMap::new();

    let mut entries = fs::read_dir(dir)
        .expect("read wolf_variant dir")
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "json") {
            continue;
        }

        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let content = fs::read_to_string(&path).expect("read wolf_variant file");
        let json: WolfVariantJson =
            serde_json::from_str(&content).expect("parse wolf_variant JSON");

        variants.insert(stem, json);
    }

    let mut enum_variants = Vec::new();
    let mut from_name_arms = Vec::new();
    let mut to_name_arms = Vec::new();
    let mut asset_id_arms = Vec::new();
    let mut wild_asset_arms = Vec::new();
    let mut tame_asset_arms = Vec::new();
    let mut angry_asset_arms = Vec::new();
    let mut all_variants = Vec::new();

    for (name, json) in &variants {
        let variant_ident = format_ident!("{}", name.to_pascal_case());
        let namespaced = format!("minecraft:{name}");
        let wild = &json.assets.wild;
        let tame = &json.assets.tame;
        let angry = &json.assets.angry;

        enum_variants.push(quote! { #variant_ident });
        from_name_arms.push(quote! {
            #namespaced | #name => Some(Self::#variant_ident)
        });
        to_name_arms.push(quote! {
            Self::#variant_ident => #name
        });
        asset_id_arms.push(quote! {
            Self::#variant_ident => #namespaced
        });
        wild_asset_arms.push(quote! {
            Self::#variant_ident => #wild
        });
        tame_asset_arms.push(quote! {
            Self::#variant_ident => #tame
        });
        angry_asset_arms.push(quote! {
            Self::#variant_ident => #angry
        });
        all_variants.push(quote! { Self::#variant_ident });
    }

    // wolf variant: biome spawn conditions, highest priority first (VariantUtils.selectVariantToSpawn)
    let mut conditions: Vec<(i32, String, Option<BiomesJson>)> = Vec::new();
    for (name, json) in variants {
        for spawn in json.spawn_conditions {
            conditions.push((
                spawn.priority,
                name.clone(),
                spawn.condition.map(|condition| condition.biomes),
            ));
        }
    }
    conditions.sort_by(|a, b| b.0.cmp(&a.0));
    let mut selection_arms = Vec::new();
    for (_, name, biomes) in &conditions {
        let variant_ident = format_ident!("{}", name.to_pascal_case());
        let biomes = match biomes {
            None => {
                selection_arms.push(quote! { return Self::#variant_ident; });
                break;
            }
            Some(BiomesJson::One(biome)) => vec![biome.clone()],
            Some(BiomesJson::Many(biomes)) => biomes.clone(),
        };
        let checks = biomes.iter().map(|biome| {
            if let Some(tag) = biome.strip_prefix('#') {
                let tag_ident = format_ident!("{}", tag.replace([':', '/'], "_").to_uppercase());
                quote! { crate::tag::WorldgenBiome::#tag_ident.0.contains(&bare) }
            } else {
                let bare = biome
                    .strip_prefix("minecraft:")
                    .unwrap_or(biome)
                    .to_string();
                quote! { bare == #bare }
            }
        });
        selection_arms.push(quote! {
            if #(#checks)||* {
                return Self::#variant_ident;
            }
        });
    }

    quote! {
        /* This file is generated. Do not edit manually. */

        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(u8)]
        pub enum WolfVariant {
            #(#enum_variants,)*
        }

        impl WolfVariant {
            #[doc = "Selects the spawn variant for a biome from the vanilla spawn conditions."]
            #[must_use]
            pub fn select_for_biome(biome_name: &str) -> Self {
                let bare = biome_name.strip_prefix("minecraft:").unwrap_or(biome_name);
                #(#selection_arms)*
            }

            #[doc = "Returns the wolf variant from a resource name (bare or namespaced)."]
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    #(#from_name_arms,)*
                    _ => None,
                }
            }

            #[doc = "Returns the numeric ID of the wolf variant in the synced registry."]
            #[must_use]
            pub const fn id(&self) -> u8 {
                *self as u8
            }

            #[doc = "Returns the bare string name of the wolf variant."]
            #[must_use]
            pub const fn to_name(&self) -> &'static str {
                match self {
                    #(#to_name_arms,)*
                }
            }

            #[doc = "Returns the fully-qualified asset id of the wolf variant."]
            #[must_use]
            pub const fn asset_id(&self) -> &'static str {
                match self {
                    #(#asset_id_arms,)*
                }
            }

            #[doc = "Returns the wild texture asset path."]
            #[must_use]
            pub const fn wild_texture(&self) -> &'static str {
                match self {
                    #(#wild_asset_arms,)*
                }
            }

            #[doc = "Returns the tame texture asset path."]
            #[must_use]
            pub const fn tame_texture(&self) -> &'static str {
                match self {
                    #(#tame_asset_arms,)*
                }
            }

            #[doc = "Returns the angry texture asset path."]
            #[must_use]
            pub const fn angry_texture(&self) -> &'static str {
                match self {
                    #(#angry_asset_arms,)*
                }
            }

            #[doc = "Returns all vanilla wolf variants."]
            #[must_use]
            pub const fn all() -> &'static [Self] {
                &[
                    #(#all_variants,)*
                ]
            }
        }
    }
}
