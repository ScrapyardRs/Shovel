use std::collections::HashSet;
use std::io::Cursor;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;

use drax::nbt::{EnsuredCompoundTag, Tag};
use drax::prelude::PacketComponent;
use mcprotocol::clientbound::play::ClientboundPlayRegistry::Disconnect;
use mcprotocol::clientbound::play::{ClientboundPlayRegistry, LevelChunkData, RelativeArgument};
use mcprotocol::common::chat::Chat;
use mcprotocol::common::chunk::Chunk;
use mcprotocol::common::play::{GlobalPos, ItemStack, Location};
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::client::PendingPosition;
use crate::entity::tracking::{EntityPositionTracker, TrackableEntity};
use crate::inventory::PlayerInventory;
use crate::level::PlayerLevel;
use crate::phase::ConnectionInformation;
use crate::{empty_light_data, PacketSend};

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

pub struct PacketLocker {
    pub(crate) send: PacketSend,
    pub(crate) packet_listener: UnboundedReceiver<ServerboundPlayRegistry>,
    pub active: bool,
    pub connection_information: ConnectionInformation,
}

impl PacketLocker {
    pub fn mutate_receiver<
        F: FnOnce(
            UnboundedReceiver<ServerboundPlayRegistry>,
        ) -> UnboundedReceiver<ServerboundPlayRegistry>,
    >(
        mut self,
        mutate_func: F,
    ) -> Self {
        self.packet_listener = {
            let pl = self.packet_listener;
            mutate_func(pl)
        };
        self
    }

    pub fn clone_writer(&self) -> PacketSend {
        self.send.clone()
    }

    pub fn next_packet(&mut self) -> Option<ServerboundPlayRegistry> {
        if !self.active {
            return None;
        }
        match self.packet_listener.try_recv() {
            Ok(packet) => Some(packet),
            Err(err) => match err {
                TryRecvError::Empty => None,
                TryRecvError::Disconnected => {
                    self.active = false;
                    None
                }
            },
        }
    }

    pub fn write_packet(&mut self, packet: Arc<ClientboundPlayRegistry>) {
        if !self.active {
            return;
        }
        if self.send.send(packet).is_err() {
            self.active = false;
        }
    }

    pub fn write_owned_packet(&mut self, packet: ClientboundPlayRegistry) {
        self.write_packet(Arc::new(packet))
    }
}

pub struct ConnectedPlayer {
    // conn
    pub packets: PacketLocker,
    // base player info
    pub(crate) entity_id: i32,
    pub(crate) profile: GameProfile,
    // position information
    pub(crate) is_position_loaded: bool,
    pub(crate) position: Location,
    pub(crate) on_ground: bool,
    pub(crate) pending_position: Arc<RwLock<PendingPosition>>,
    pub(crate) teleport_id_incr: AtomicI32,
    // level information
    pub(crate) known_chunks: HashSet<(i32, i32)>,
    // inventory
    pub(crate) player_inventory: PlayerInventory,
}

impl TrackableEntity for ConnectedPlayer {
    fn id(&self) -> i32 {
        self.entity_id
    }

    fn uuid(&self) -> Uuid {
        self.profile.id
    }

    fn location(&self) -> Location {
        self.position
    }

    fn on_ground(&self) -> bool {
        self.on_ground
    }

    fn create_entity(position_tracker: &EntityPositionTracker) -> ClientboundPlayRegistry {
        ClientboundPlayRegistry::AddPlayer {
            entity_id: position_tracker.entity_id_ref,
            player_id: position_tracker.entity_uuid_ref,
            location: position_tracker.last_tracked_location.inner_loc,
            y_rot: position_tracker.rot_cache.0 as u8,
            x_rot: position_tracker.rot_cache.1 as u8,
        }
    }
}

impl ConnectedPlayer {
    pub fn disconnect<C: Into<Chat>>(&mut self, chat: C) {
        self.packets.write_owned_packet(Disconnect {
            reason: chat.into(),
        });
        self.packets.active = false;
    }

    pub fn username(&self) -> &String {
        &self.profile.name
    }

    pub fn profile(&self) -> &GameProfile {
        &self.profile
    }

    pub fn player_inventory(&self) -> &PlayerInventory {
        &self.player_inventory
    }

    pub fn player_inventory_mut(&mut self) -> &mut PlayerInventory {
        &mut self.player_inventory
    }

    pub fn next_packet(&mut self) -> Option<ServerboundPlayRegistry> {
        self.packets.next_packet()
    }

    pub fn write_packet(&mut self, packet: Arc<ClientboundPlayRegistry>) {
        self.packets.write_packet(packet)
    }

    pub fn write_owned_packet(&mut self, packet: ClientboundPlayRegistry) {
        self.packets.write_owned_packet(packet)
    }

    pub async fn teleport_local(&mut self, location: Location) {
        self.teleport(location, false).await
    }

    pub async fn teleport(&mut self, location: Location, pause_position: bool) {
        let teleport_id = self
            .teleport_id_incr
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut pending = self.pending_position.write().await;
        pending.pending_teleport = Some((location, teleport_id));
        if pause_position {
            pending.is_loaded = false;
        }
        drop(pending);
        self.position = location.clone();
        self.packets
            .write_owned_packet(ClientboundPlayRegistry::PlayerPosition {
                location,
                relative_arguments: RelativeArgument::new(0x0),
                id: teleport_id,
                dismount: false,
            });
        if pause_position {
            self.is_position_loaded = false;
        }
    }

    pub fn mutate_receiver<
        F: FnOnce(
            UnboundedReceiver<ServerboundPlayRegistry>,
        ) -> UnboundedReceiver<ServerboundPlayRegistry>,
    >(
        mut self,
        func: F,
    ) -> Self {
        self.packets = self.packets.mutate_receiver(func);
        self
    }

    #[inline]
    pub fn is_loaded(&self) -> bool {
        self.is_position_loaded
    }

    pub async fn update_location(&mut self) -> bool {
        let pending_position = self.pending_position.read().await;
        if !pending_position.is_loaded {
            return false;
        }
        let pending = pending_position.location;
        drop(pending_position);
        if !self.is_position_loaded {
            self.is_position_loaded = true;
        }

        let changed_chunk = if f64::floor(self.position.inner_loc.x) as i32 >> 4
            != f64::floor(pending.inner_loc.x) as i32 >> 4
        {
            true
        } else {
            f64::floor(self.position.inner_loc.z) as i32 >> 4
                != f64::floor(pending.inner_loc.z) as i32 >> 4
        };
        self.position = pending;
        changed_chunk
    }

    pub fn knows_chunk(&self, chunk_x: i32, chunk_z: i32) -> bool {
        self.known_chunks.contains(&(chunk_x, chunk_z))
    }

    pub fn send_chunk(&mut self, chunk: Chunk) {
        self.known_chunks.insert((chunk.x(), chunk.z()));
        self.write_packet(Arc::new(ClientboundPlayRegistry::LevelChunkWithLight {
            chunk_data: LevelChunkData {
                chunk,
                block_entities: vec![],
            },
            light_data: empty_light_data!(),
        }));
    }

    pub fn forget_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        self.known_chunks.remove(&(chunk_x, chunk_z));
        self.write_packet(Arc::new(ClientboundPlayRegistry::ForgetLevelChunk {
            x: chunk_x,
            z: chunk_z,
        }));
    }

    pub fn update_center_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        self.write_packet(Arc::new(ClientboundPlayRegistry::SetChunkCacheCenter {
            x: chunk_x,
            z: chunk_z,
        }));
    }

    pub async fn render_level(&mut self, level: &PlayerLevel) {
        let chunk_changed = self.update_location().await;
        if chunk_changed {
            level.poll_player(self, true);
            self.update_center_chunk(
                f64::floor(self.position.inner_loc.x) as i32 >> 4,
                f64::floor(self.position.inner_loc.z) as i32 >> 4,
            );
        }
    }

    // inventory stuff
    pub fn clear_inventory(&mut self) {
        let forward = self.player_inventory_mut().clear();
        self.write_owned_packet(forward);
    }

    pub fn refresh_player_inventory(&mut self) {
        let forward = self.player_inventory_mut().refresh();
        self.write_owned_packet(forward);
    }

    pub fn set_player_inventory(&mut self, items: &[Option<ItemStack>]) {
        let forward = self.player_inventory_mut().set_all(items);
        self.write_owned_packet(forward);
    }

    pub fn set_player_inventory_slot(
        &mut self,
        item: Option<ItemStack>,
        slot_x: usize,
        slot_y: usize,
    ) {
        let forward = self.player_inventory_mut().set(item, slot_x, slot_y);
        self.write_owned_packet(forward);
    }

    pub fn set_head(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_head(item);
        self.write_owned_packet(forward);
    }

    pub fn set_chest(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_chest(item);
        self.write_owned_packet(forward);
    }

    pub fn set_legs(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_legs(item);
        self.write_owned_packet(forward);
    }

    pub fn set_feet(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_feet(item);
        self.write_owned_packet(forward);
    }

    pub fn set_offhand(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_offhand(item);
        self.write_owned_packet(forward);
    }

    pub fn set_crafting_output(&mut self, item: Option<ItemStack>) {
        let forward = self.player_inventory_mut().set_crafting_output(item);
        self.write_owned_packet(forward);
    }

    pub fn set_crafting_slot(&mut self, item: Option<ItemStack>, slot_x: usize, slot_y: usize) {
        let forward = self
            .player_inventory_mut()
            .set_crafting_slot(item, slot_x, slot_y);
        self.write_owned_packet(forward);
    }

    pub fn set_current_slot(&mut self, slot: u8) {
        let forward = self.player_inventory_mut().set_current_slot(slot);
        self.write_owned_packet(forward);
    }

    // general helper

    pub fn send_message<C: Into<Chat>>(&mut self, chat: C) {
        self.write_owned_packet(ClientboundPlayRegistry::SystemChat {
            content: chat.into(),
            overlay: false,
        });
    }

    pub fn clear_known_chunks(&mut self) {
        for (x, z) in self.known_chunks.drain().collect::<Vec<_>>() {
            // if self.position.inner_loc.x as i32 >> 4 == x
            //     && self.position.inner_loc.z as i32 >> 4 == z
            // {
            //     self.known_chunks.remove(&(x, z));
            //     continue;
            // }
            self.forget_chunk(x, z);
        }
    }
}
