use std::io::Cursor;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;

use drax::nbt::{EnsuredCompoundTag, Tag};
use drax::prelude::PacketComponent;
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    MoveEntityPos, MoveEntityPosRot, MoveEntityRot, TeleportEntity,
};
use mcprotocol::clientbound::play::{ClientboundPlayRegistry, RelativeArgument};
use mcprotocol::common::play::{GlobalPos, Location, SimpleLocation};
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::RwLock;

use crate::client::PendingPosition;
use crate::phase::ConnectionInformation;
use crate::PacketSend;

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
    pub(crate) rot: (i32, i32, i32),
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

macro_rules! poll_location_impl {
    () => {
        pub async fn poll_location(&mut self) -> Vec<ClientboundPlayRegistry> {
            let pending_position = self.pending_position.read().await;
            let is_loaded = pending_position.is_loaded.clone();
            if !is_loaded {
                drop(pending_position);
                return vec![];
            }
            let mut pending_location = pending_position.location.clone();
            let on_ground = pending_position.on_ground.clone();
            drop(pending_position);
            let send_everything = !self.tracking.is_loaded_in_world || self.tracking.tick == 10;
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

            let change_flag_1 = ((delta_x * delta_x) + (delta_y * delta_y) + (delta_z * delta_z))
                >= 7.62939453125E-6;
            let change_flag_2 = change_flag_1 || self.tracking.tick % 20 == 0;
            let change_flag_3 = i32::abs(y_rot_bits - self.tracking.rot.0) >= 1
                || i32::abs(x_rot_bits - self.tracking.rot.1) >= 1;

            let mut to_send = Vec::with_capacity(2);

            if !flag4 && self.tracking.was_on_ground == on_ground && !send_everything {
                if !change_flag_2 || !change_flag_3 {
                    if change_flag_2 {
                        to_send.push(MoveEntityPos {
                            id: self.entity_id,
                            xa: ex as i16,
                            ya: ey as i16,
                            za: ez as i16,
                            on_ground,
                        });
                    } else if change_flag_3 {
                        to_send.push(MoveEntityRot {
                            entity_id: self.entity_id,
                            y_rot: y_rot_bits as u8,
                            x_rot: x_rot_bits as u8,
                            on_ground,
                        });
                    }
                } else {
                    to_send.push(MoveEntityPosRot {
                        id: self.entity_id,
                        xa: ex as i16,
                        ya: ey as i16,
                        za: ez as i16,
                        y_rot: y_rot_bits as u8,
                        x_rot: x_rot_bits as u8,
                        on_ground,
                    });
                }
            } else {
                self.tracking.was_on_ground = on_ground;
                to_send.push(TeleportEntity {
                    entity_id: self.entity_id,
                    location: SimpleLocation {
                        x: pending_location.inner_loc.x,
                        y: pending_location.inner_loc.y,
                        z: pending_location.inner_loc.z,
                    },
                    y_rot: y_rot_bits as u8,
                    x_rot: x_rot_bits as u8,
                    on_ground,
                });
            }

            if i32::abs(y_rot_bits - self.tracking.rot.2) >= 1 || send_everything {
                to_send.push(ClientboundPlayRegistry::RotateHead {
                    entity_id: self.entity_id,
                    y_head_rot: y_rot_bits as u8,
                });
                self.tracking.rot.2 = y_rot_bits;
            }

            if change_flag_3 || send_everything {
                self.tracking.rot.0 = y_rot_bits;
                self.tracking.rot.1 = x_rot_bits;
            }

            self.tracking.tick += 1;
            self.position = pending_location;
            to_send
        }
    };
}

pub struct ConnectedPlayer {
    pub(crate) writer: StructuredWriterClone<OwnedWriteHalf>,
    pub profile: GameProfile,
    pub packets: PacketLocker,
    pub position: Location,
    pub(crate) pending_position: Arc<RwLock<PendingPosition>>,
    pub entity_id: i32,
    pub(crate) tracking: TrackingDetails,
    pub(crate) teleport_id_incr: AtomicI32,
}

impl ConnectedPlayer {
    pub async fn teleport(&mut self, location: Location) -> drax::prelude::Result<()> {
        let teleport_id = self
            .teleport_id_incr
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut pending = self.pending_position.write().await;
        pending.pending_teleport = Some((location.clone(), teleport_id));
        self.write_packet(&ClientboundPlayRegistry::PlayerPosition {
            location,
            relative_arguments: RelativeArgument::new(0x0),
            id: teleport_id,
            dismount: false,
        })
        .await
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
    pub async fn write_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &self,
        packet: &P,
    ) -> drax::prelude::Result<()> {
        self.writer.write_packet(packet).await
    }

    pub fn is_loaded(&self) -> bool {
        self.tracking.is_loaded_in_world
    }

    poll_location_impl!();

    #[inline]
    pub fn clone_writer(&self) -> StructuredWriterClone<OwnedWriteHalf> {
        self.writer.clone()
    }
}

pub struct PartedPlayer {
    pub packet_sender: PacketSend,
    pub profile: GameProfile,
    pub packets: PacketLocker,
    pub position: Location,
    pub(crate) pending_position: Arc<RwLock<PendingPosition>>,
    pub entity_id: i32,
    pub(crate) tracking: TrackingDetails,
    pub(crate) teleport_id_incr: AtomicI32,
}

impl PartedPlayer {
    // pub async fn teleport(&mut self, location: Location) {
    //     let teleport_id = self
    //         .teleport_id_incr
    //         .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    //     let mut pending = self.pending_position.write().await;
    //     pending.pending_teleport = Some((location.clone(), teleport_id));
    //     self.write_packet(&ClientboundPlayRegistry::PlayerPosition {
    //         location,
    //         relative_arguments: RelativeArgument::new(0x0),
    //         id: teleport_id,
    //         dismount: false,
    //     })
    //     .await
    // }

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

    // #[inline]
    // pub async fn write_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
    //     &self,
    //     packet: &P,
    // ) -> drax::prelude::Result<()> {
    //     self.writer.write_packet(packet).await
    // }

    pub fn is_loaded(&self) -> bool {
        self.tracking.is_loaded_in_world
    }

    poll_location_impl!();

    // #[inline]
    // pub fn clone_sender(&self) -> UnboundedSender<ClientboundPlayRegistry> {
    //     self.writer.clone()
    // }
}
