use pumpkin_data::block_properties::{BedPart, WhiteBedLikeProperties as BedProperties};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::{Block, BlockStateId, villager::VillagerProfession};
use pumpkin_world::poi::POI_TYPE_NETHER_PORTAL;

#[must_use]
pub fn profession_for_block(block: &Block) -> Option<VillagerProfession> {
    match block {
        block if block == &Block::COMPOSTER => Some(VillagerProfession::Farmer),
        block if block == &Block::LECTERN => Some(VillagerProfession::Librarian),
        block if block == &Block::BLAST_FURNACE => Some(VillagerProfession::Armorer),
        block if block == &Block::SMOKER => Some(VillagerProfession::Butcher),
        block if block == &Block::CARTOGRAPHY_TABLE => Some(VillagerProfession::Cartographer),
        block if block == &Block::BREWING_STAND => Some(VillagerProfession::Cleric),
        block if block == &Block::BARREL => Some(VillagerProfession::Fisherman),
        block if block == &Block::FLETCHING_TABLE => Some(VillagerProfession::Fletcher),
        block
            if [
                &Block::CAULDRON,
                &Block::WATER_CAULDRON,
                &Block::LAVA_CAULDRON,
                &Block::POWDER_SNOW_CAULDRON,
            ]
            .contains(&block) =>
        {
            Some(VillagerProfession::Leatherworker)
        }
        block if block == &Block::STONECUTTER => Some(VillagerProfession::Mason),
        block if block == &Block::LOOM => Some(VillagerProfession::Shepherd),
        block if block == &Block::SMITHING_TABLE => Some(VillagerProfession::Toolsmith),
        block if block == &Block::GRINDSTONE => Some(VillagerProfession::Weaponsmith),
        _ => None,
    }
}

// poi: VillagerProfession.heldJobSite, the poi type of a profession's workstation
#[must_use]
pub const fn poi_type_for_profession(profession: VillagerProfession) -> Option<&'static str> {
    Some(match profession {
        VillagerProfession::Armorer => "minecraft:armorer",
        VillagerProfession::Butcher => "minecraft:butcher",
        VillagerProfession::Cartographer => "minecraft:cartographer",
        VillagerProfession::Cleric => "minecraft:cleric",
        VillagerProfession::Farmer => "minecraft:farmer",
        VillagerProfession::Fisherman => "minecraft:fisherman",
        VillagerProfession::Fletcher => "minecraft:fletcher",
        VillagerProfession::Leatherworker => "minecraft:leatherworker",
        VillagerProfession::Librarian => "minecraft:librarian",
        VillagerProfession::Mason => "minecraft:mason",
        VillagerProfession::Shepherd => "minecraft:shepherd",
        VillagerProfession::Toolsmith => "minecraft:toolsmith",
        VillagerProfession::Weaponsmith => "minecraft:weaponsmith",
        VillagerProfession::None | VillagerProfession::Nitwit => return None,
    })
}

// poi: the profession a workstation poi type gives (acquirableJobSite)
#[must_use]
pub fn profession_for_poi_type(poi_type: &str) -> Option<VillagerProfession> {
    ALL_PROFESSIONS
        .into_iter()
        .find(|profession| poi_type_for_profession(*profession) == Some(poi_type))
}

const ALL_PROFESSIONS: [VillagerProfession; 13] = [
    VillagerProfession::Armorer,
    VillagerProfession::Butcher,
    VillagerProfession::Cartographer,
    VillagerProfession::Cleric,
    VillagerProfession::Farmer,
    VillagerProfession::Fisherman,
    VillagerProfession::Fletcher,
    VillagerProfession::Leatherworker,
    VillagerProfession::Librarian,
    VillagerProfession::Mason,
    VillagerProfession::Shepherd,
    VillagerProfession::Toolsmith,
    VillagerProfession::Weaponsmith,
];

// poi: PoiTypes max tickets per type
#[must_use]
pub fn max_tickets_for_type(poi_type: &str) -> i32 {
    match poi_type {
        "minecraft:home" => 1,
        "minecraft:meeting" => 32,
        other if profession_for_poi_type(other).is_some() => 1,
        _ => 0,
    }
}

#[must_use]
pub fn poi_type_for_block(block: &Block) -> Option<&'static str> {
    Some(match profession_for_block(block)? {
        VillagerProfession::Armorer => "minecraft:armorer",
        VillagerProfession::Butcher => "minecraft:butcher",
        VillagerProfession::Cartographer => "minecraft:cartographer",
        VillagerProfession::Cleric => "minecraft:cleric",
        VillagerProfession::Farmer => "minecraft:farmer",
        VillagerProfession::Fisherman => "minecraft:fisherman",
        VillagerProfession::Fletcher => "minecraft:fletcher",
        VillagerProfession::Leatherworker => "minecraft:leatherworker",
        VillagerProfession::Librarian => "minecraft:librarian",
        VillagerProfession::Mason => "minecraft:mason",
        VillagerProfession::Shepherd => "minecraft:shepherd",
        VillagerProfession::Toolsmith => "minecraft:toolsmith",
        VillagerProfession::Weaponsmith => "minecraft:weaponsmith",
        VillagerProfession::None | VillagerProfession::Nitwit => return None,
    })
}

/// Vanilla `PoiTypes.forState`: the POI type a block state registers, with its max tickets.
#[must_use]
pub fn poi_type_for_state(state_id: BlockStateId) -> Option<(&'static str, i32)> {
    let block = Block::from_state_id(state_id);
    if block.has_tag(&tag::Block::MINECRAFT_BEDS) {
        return (BedProperties::from_state_id(state_id).part == BedPart::Head)
            .then_some(("minecraft:home", 1));
    }
    if block.has_tag(&tag::Block::MINECRAFT_LIGHTNING_RODS) {
        return Some(("minecraft:lightning_rod", 0));
    }
    if let Some(poi_type) = poi_type_for_block(block) {
        return Some((poi_type, 1));
    }
    Some(match block {
        block if block == &Block::BELL => ("minecraft:meeting", 32),
        block if block == &Block::BEEHIVE => ("minecraft:beehive", 0),
        block if block == &Block::BEE_NEST => ("minecraft:bee_nest", 0),
        block if block == &Block::NETHER_PORTAL => (POI_TYPE_NETHER_PORTAL, 0),
        block if block == &Block::LODESTONE => ("minecraft:lodestone", 0),
        block if block == &Block::TEST_INSTANCE_BLOCK => ("minecraft:test_instance", 0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_villager_workstation_maps_to_its_profession() {
        let workstations = [
            (&Block::COMPOSTER, VillagerProfession::Farmer),
            (&Block::LECTERN, VillagerProfession::Librarian),
            (&Block::BLAST_FURNACE, VillagerProfession::Armorer),
            (&Block::SMOKER, VillagerProfession::Butcher),
            (&Block::CARTOGRAPHY_TABLE, VillagerProfession::Cartographer),
            (&Block::BREWING_STAND, VillagerProfession::Cleric),
            (&Block::BARREL, VillagerProfession::Fisherman),
            (&Block::FLETCHING_TABLE, VillagerProfession::Fletcher),
            (&Block::CAULDRON, VillagerProfession::Leatherworker),
            (&Block::STONECUTTER, VillagerProfession::Mason),
            (&Block::LOOM, VillagerProfession::Shepherd),
            (&Block::SMITHING_TABLE, VillagerProfession::Toolsmith),
            (&Block::GRINDSTONE, VillagerProfession::Weaponsmith),
        ];

        for (block, profession) in workstations {
            assert_eq!(profession_for_block(block), Some(profession));
            assert_eq!(
                poi_type_for_block(block).unwrap(),
                format!(
                    "minecraft:{}",
                    profession.translation_key().rsplit('.').next().unwrap()
                )
            );
        }
        assert_eq!(profession_for_block(&Block::DIRT), None);
        assert_eq!(poi_type_for_block(&Block::DIRT), None);
    }
}
