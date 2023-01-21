use std::io::Cursor;
use std::sync::Arc;

use drax::nbt::{EnsuredCompoundTag, Tag};
use drax::prelude::PacketComponent;
use mcprotocol::clientbound::play::ClientboundPlayRegistry;
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    MoveEntityPos, MoveEntityPosRot, MoveEntityRot, TeleportEntity,
};
use mcprotocol::common::play::{GlobalPos, Location, SimpleLocation};
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::RwLock;

use crate::client::{PendingPosition, StructuredWriterClone};
use crate::phase::ConnectionInformation;

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

#[derive(Default)]
pub struct TrackingDetails {
    pub(crate) is_loaded_in_world: bool,
    pub(crate) tick: usize,
    pub(crate) rot: (i32, i32),
    pub(crate) was_on_ground: bool,
}

pub struct PacketLocker {
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

    pub fn next_packet(&mut self) -> Result<Option<ServerboundPlayRegistry>, ()> {
        match self.packet_listener.try_recv() {
            Ok(packet) => Ok(Some(packet)),
            Err(err) => match err {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(()),
            },
        }
    }
}

pub struct ConnectedPlayer {
    pub(crate) writer: StructuredWriterClone<OwnedWriteHalf>,
    pub profile: GameProfile,
    pub packets: PacketLocker,
    pub position: Location,
    pub(crate) pending_position: Arc<RwLock<PendingPosition>>,
    pub entity_id: i32,
    pub(crate) tracking: TrackingDetails,
}

impl ConnectedPlayer {
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
    pub async fn write_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &self,
        packet: &P,
    ) -> drax::prelude::Result<()> {
        self.writer.write_packet(packet).await
    }

    pub async fn poll_location(&mut self) -> Option<ClientboundPlayRegistry> {
        let pending_position = self.pending_position.read().await;
        let is_loaded = pending_position.is_loaded.clone();
        if !is_loaded {
            drop(pending_position);
            return None;
        }
        let mut pending_location = pending_position.location.clone();
        let on_ground = pending_position.on_ground.clone();
        drop(pending_position);
        if !self.tracking.is_loaded_in_world {
            self.tracking.is_loaded_in_world = true;
        }

        pending_location.inner_loc.x = pending_location.inner_loc.x.clamp(-3.0E7, 3.0E7);
        pending_location.inner_loc.y = pending_location.inner_loc.y.clamp(-2.0E7, 2.0E7);
        pending_location.inner_loc.z = pending_location.inner_loc.z.clamp(-3.0E7, 3.0E7);
        pending_location.yaw = crate::math::wrap_degrees(pending_location.yaw);
        pending_location.pitch = crate::math::wrap_degrees(pending_location.pitch);

        let y_rot_bits = f32::floor((pending_location.yaw * 256.0) / 360.0) as i32;
        let x_rot_bits = f32::floor((pending_location.pitch * 256.0) / 360.0) as i32;

        let delta_x = self.position.inner_loc.x - pending_location.inner_loc.x;
        let delta_y = self.position.inner_loc.y - pending_location.inner_loc.y;
        let delta_z = self.position.inner_loc.z - pending_location.inner_loc.z;

        let (ex, ey, ez) =
            crate::math::encode_position(self.position.inner_loc, pending_location.inner_loc);

        let sm = 32767;
        let smi = -32768;
        let flag4 = ex < smi || ey < smi || ez < smi || ex > sm || ey > sm || ez > sm;

        let change_flag_1 =
            ((delta_x * delta_x) + (delta_y * delta_y) + (delta_z * delta_z)) >= 7.62939453125E-6;
        let change_flag_2 = change_flag_1 || self.tracking.tick % 60 == 0;
        let change_flag_3 = i32::abs(y_rot_bits - self.tracking.rot.0) >= 1
            || i32::abs(x_rot_bits - self.tracking.rot.1) >= 1;

        let to_send = if !flag4 && self.tracking.was_on_ground == on_ground {
            if !change_flag_2 || !change_flag_3 {
                if change_flag_2 {
                    Some(MoveEntityPos {
                        id: self.entity_id,
                        xa: ex as i16,
                        ya: ey as i16,
                        za: ez as i16,
                        on_ground,
                    })
                } else if change_flag_3 {
                    Some(MoveEntityRot {
                        entity_id: self.entity_id,
                        y_rot: y_rot_bits as u8,
                        x_rot: x_rot_bits as u8,
                        on_ground,
                    })
                } else {
                    unreachable!()
                }
            } else {
                Some(MoveEntityPosRot {
                    id: self.entity_id,
                    xa: ex as i16,
                    ya: ey as i16,
                    za: ez as i16,
                    y_rot: y_rot_bits as u8,
                    x_rot: x_rot_bits as u8,
                    on_ground,
                })
            }
        } else {
            self.tracking.was_on_ground = on_ground;
            Some(TeleportEntity {
                entity_id: self.entity_id,
                location: SimpleLocation {
                    x: pending_location.inner_loc.x,
                    y: pending_location.inner_loc.y,
                    z: pending_location.inner_loc.z,
                },
                y_rot: y_rot_bits as u8,
                x_rot: x_rot_bits as u8,
                on_ground,
            })
        };

        if change_flag_3 {
            self.tracking.rot = (y_rot_bits, x_rot_bits);
        }

        self.tracking.tick += 1;
        self.position = pending_location;
        to_send
    }
}
