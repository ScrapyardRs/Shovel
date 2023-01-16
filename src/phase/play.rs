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
    pub player_id: i32,
    pub hardcore: bool,
    pub game_type: u8,
    pub previous_game_type: u8,
    pub seed: u64,
    pub max_players: i32,
    pub chunk_radius: i32,
    pub simulation_distance: i32,
    pub reduced_debug_info: bool,
    pub show_death_screen: bool,
    pub is_debug: bool,
    pub is_flat: bool,
    pub last_death_location: Option<GlobalPos>,
}
