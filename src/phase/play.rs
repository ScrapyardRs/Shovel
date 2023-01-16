use drax::nbt::{EnsuredCompoundTag, Tag};
use drax::prelude::PacketComponent;
use mcprotocol::common::play::GlobalPos;
use std::io::Cursor;

const CURRENT_CODEC_BYTES: &[u8] = include_bytes!("761.b.nbt");
// todo we should implement the codec better than this
//  this is a temporary solution for testing around
//  potential updates to the codec while we update the
//  rest of the system
pub async fn get_current_dimension_snapshot() -> drax::prelude::Result<Option<Tag>> {
    let mut read_v = Cursor::new(Vec::from(CURRENT_CODEC_BYTES));
    EnsuredCompoundTag::<0>::decode(&mut (), &mut read_v).await
}

pub struct ClientLoginProperties {
    player_id: i32,
    hardcore: bool,
    game_type: u8,
    previous_game_type: u8,
    seed: u64,
    max_players: i32,
    chunk_radius: i32,
    simulation_distance: i32,
    reduced_debug_info: bool,
    show_death_screen: bool,
    is_debug: bool,
    is_flat: bool,
    last_death_location: Option<GlobalPos>,
}
