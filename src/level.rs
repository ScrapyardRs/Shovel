use std::collections::HashMap;
use std::sync::Arc;

use mcprotocol::clientbound::play::ClientboundPlayRegistry;
use mcprotocol::common::play::{BlockPos, BlockUpdate, SectionPos};

use crate::phase::play::ConnectedPlayer;

#[macro_export]
macro_rules! empty_light_data {
    () => {
        mcprotocol::clientbound::play::LightUpdateData {
            trust_edges: false,
            sky_y_mask: mcprotocol::common::bit_set::BitSet::value_of(vec![])
                .expect("bit set failed to create"),
            block_y_mask: mcprotocol::common::bit_set::BitSet::value_of(vec![])
                .expect("bit set failed to create"),
            empty_sky_y_mask: mcprotocol::common::bit_set::BitSet::value_of(vec![])
                .expect("bit set failed to create"),
            empty_block_y_mask: mcprotocol::common::bit_set::BitSet::value_of(vec![])
                .expect("bit set failed to create"),
            sky_updates: vec![vec![]; 2048],
            block_updates: vec![vec![]; 2048],
        }
    };
}

#[derive(Default)]
pub struct LevelMediator {
    updates: HashMap<SectionPos, HashMap<BlockPos, i32>>,
}

impl LevelMediator {
    pub fn update(&mut self, pos: BlockPos, block_id: i32) {
        let section_pos = pos.into();
        if self.updates.contains_key(&section_pos) {
            self.updates
                .get_mut(&section_pos)
                .unwrap()
                .insert(pos, block_id);
        } else {
            let mut section_updates = HashMap::new();
            section_updates.insert(pos, block_id);
            self.updates.insert(section_pos, section_updates);
        }
    }

    pub fn validate_positions(&self, player: &ConnectedPlayer) -> bool {
        for section_pos in self.updates.keys() {
            if !player.knows_chunk(section_pos.x, section_pos.z) {
                return false;
            }
        }
        true
    }

    pub fn into_updates(self) -> Vec<Arc<ClientboundPlayRegistry>> {
        self.updates
            .into_iter()
            .map(
                |(section_pos, updates)| ClientboundPlayRegistry::SectionBlocksUpdate {
                    section_pos,
                    suppress_light_update: false,
                    update_info: updates
                        .into_iter()
                        .map(|(block_pos, block_id)| BlockUpdate {
                            block_id,
                            block_pos,
                        })
                        .collect(),
                },
            )
            .map(Arc::new)
            .collect()
    }
}
