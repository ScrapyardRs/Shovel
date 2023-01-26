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
