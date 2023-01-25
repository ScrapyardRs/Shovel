use mcprotocol::common::chunk::{BasicRegion, CachedLevel};

use crate::entity::tracking::TrackableEntity;
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

pub struct PlayerLevel {
    poll_radius: i32,
    cached_level: CachedLevel,
}

impl PlayerLevel {
    pub fn new(poll_radius: i32) -> Self {
        Self {
            poll_radius,
            cached_level: CachedLevel::default(),
        }
    }

    pub fn insert_region(&mut self, region: BasicRegion) {
        self.cached_level.insert_region(region);
    }

    pub fn poll_player(&self, player: &mut ConnectedPlayer, full_radius_check: bool) {
        let poll_radius = self.poll_radius;
        let chunk_x = f64::floor(player.location().inner_loc.x) as i32 >> 4;
        let chunk_z = f64::floor(player.location().inner_loc.z) as i32 >> 4;

        if full_radius_check {
            for x in -poll_radius..=poll_radius {
                for z in -poll_radius..=poll_radius {
                    if player.knows_chunk(chunk_x + x, chunk_z + z) {
                        continue;
                    }

                    let chunk = self.cached_level.clone_cached(chunk_x + x, chunk_z + z);
                    player.send_chunk(chunk);
                }
            }
        } else {
            unimplemented!("This is an optimization which will be added later")
        }
    }
}
