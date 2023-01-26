use std::collections::HashMap;
use std::sync::Arc;

use crate::client::ConditionalPacket;
use mcprotocol::clientbound::play::ClientboundPlayRegistry;
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    MoveEntityPos, MoveEntityPosRot, MoveEntityRot, TeleportEntity,
};
use mcprotocol::common::play::{Location, SimpleLocation};
use uuid::Uuid;

use crate::PacketSend;

pub enum TrackingState {
    WaitingForPlayers { packet_sender: PacketSend },
    Active { packet_sender: PacketSend },
    UnknownNonPlayerEntity,
    KnownNonPlayerEntity,
}

pub struct EntityPositionTracker {
    pub entity_id_ref: i32,
    pub entity_uuid_ref: Uuid,
    pub last_tracked_location: Location,
    tick: usize,
    pub rot_cache: (i32, i32, i32),
    was_on_ground: bool,
    state: TrackingState,
    create_entity_fn: fn(&EntityPositionTracker) -> ClientboundPlayRegistry,
}

impl EntityPositionTracker {
    pub fn create_from_initial_location(
        entity: i32,
        entity_uuid: Uuid,
        initial: Location,
        start_on_ground: bool,
        create_entity_fn: fn(&EntityPositionTracker) -> ClientboundPlayRegistry,
        initial_state: TrackingState,
    ) -> Self {
        let y_rot_bits = f32::floor((initial.pitch * 256.0) / 360.0) as i32;
        Self {
            entity_id_ref: entity,
            entity_uuid_ref: entity_uuid,
            last_tracked_location: initial,
            tick: 0,
            rot_cache: (
                y_rot_bits,
                f32::floor((initial.yaw * 256.0) / 360.0) as i32,
                y_rot_bits,
            ),
            was_on_ground: start_on_ground,
            state: initial_state,
            create_entity_fn,
        }
    }

    pub fn poll_location(
        &mut self,
        rel_location: Location,
        on_ground: bool,
    ) -> Vec<ClientboundPlayRegistry> {
        let force_update = self.tick == 0;

        let y_rot_bits = f32::floor((rel_location.yaw * 256.0) / 360.0) as i32;
        let x_rot_bits = f32::floor((rel_location.pitch * 256.0) / 360.0) as i32;

        let delta_x = self.last_tracked_location.inner_loc.x - rel_location.inner_loc.x;
        let delta_y = self.last_tracked_location.inner_loc.y - rel_location.inner_loc.y;
        let delta_z = self.last_tracked_location.inner_loc.z - rel_location.inner_loc.z;

        let (ex, ey, ez) = crate::math::encode_position(
            self.last_tracked_location.inner_loc,
            rel_location.inner_loc,
        );

        let sm = 32767;
        let smi = -32768;
        let flag4 = ex < smi || ey < smi || ez < smi || ex > sm || ey > sm || ez > sm;

        let change_flag_1 =
            ((delta_x * delta_x) + (delta_y * delta_y) + (delta_z * delta_z)) >= 7.62939453125E-6;
        let change_flag_2 = change_flag_1 || self.tick % 20 == 0;
        let change_flag_3 = i32::abs(y_rot_bits - self.rot_cache.0) >= 1
            || i32::abs(x_rot_bits - self.rot_cache.1) >= 1;

        let mut to_send = Vec::with_capacity(2);

        if !flag4 && self.was_on_ground == on_ground && !force_update {
            if !change_flag_2 || !change_flag_3 {
                if change_flag_2 {
                    to_send.push(MoveEntityPos {
                        id: self.entity_id_ref,
                        xa: ex as i16,
                        ya: ey as i16,
                        za: ez as i16,
                        on_ground,
                    });
                } else if change_flag_3 {
                    to_send.push(MoveEntityRot {
                        entity_id: self.entity_id_ref,
                        y_rot: y_rot_bits as u8,
                        x_rot: x_rot_bits as u8,
                        on_ground,
                    });
                }
            } else {
                to_send.push(MoveEntityPosRot {
                    id: self.entity_id_ref,
                    xa: ex as i16,
                    ya: ey as i16,
                    za: ez as i16,
                    y_rot: y_rot_bits as u8,
                    x_rot: x_rot_bits as u8,
                    on_ground,
                });
            }
        } else {
            self.was_on_ground = on_ground;
            to_send.push(TeleportEntity {
                entity_id: self.entity_id_ref,
                location: SimpleLocation {
                    x: rel_location.inner_loc.x,
                    y: rel_location.inner_loc.y,
                    z: rel_location.inner_loc.z,
                },
                y_rot: y_rot_bits as u8,
                x_rot: x_rot_bits as u8,
                on_ground,
            });
        }

        if i32::abs(y_rot_bits - self.rot_cache.2) >= 1 || force_update {
            to_send.push(ClientboundPlayRegistry::RotateHead {
                entity_id: self.entity_id_ref,
                y_head_rot: y_rot_bits as u8,
            });
            self.rot_cache.2 = y_rot_bits;
        }

        if change_flag_3 || force_update {
            self.rot_cache.0 = y_rot_bits;
            self.rot_cache.1 = x_rot_bits;
        }

        self.tick += 1;
        self.last_tracked_location = rel_location;
        to_send
    }
}

pub trait TrackableEntity {
    fn id(&self) -> i32;

    fn uuid(&self) -> Uuid;

    fn location(&self) -> Location;

    fn on_ground(&self) -> bool;

    fn create_entity(position_tracker: &EntityPositionTracker) -> ClientboundPlayRegistry;

    fn create_entity_with_type(
        position_tracker: &EntityPositionTracker,
        entity_type: i32,
    ) -> ClientboundPlayRegistry {
        ClientboundPlayRegistry::AddEntity {
            id: position_tracker.entity_id_ref,
            uuid: Default::default(),
            entity_type,
            location: position_tracker.last_tracked_location.inner_loc,
            x_rot: position_tracker.rot_cache.1 as u8,
            y_rot: position_tracker.rot_cache.0 as u8,
            y_head_rot: position_tracker.rot_cache.2 as u8,
            data: 0,
            xa: 0,
            ya: 0,
            za: 0,
        }
    }
}

#[macro_export]
macro_rules! assign_entity_tracker {
    ($self:ident, $self_ref:ident, $id_getter:expr, $uuid_getter:expr, $location_getter:expr, $on_ground_getter:expr, $entity_type:literal) => {
        impl TrackableEntity for $self {
            fn id($self_ref: &Self) -> i32 {
                $id_getter
            }

            fn uuid($self_ref: &Self) -> Uuid {
                $uuid_getter
            }

            fn location($self_ref: &Self) -> Location {
                $location_getter
            }

            fn on_ground($self_ref: &Self) -> bool {
                $on_ground_getter
            }

            fn create_entity(position_tracker: &EntityPositionTracker) -> ClientboundPlayRegistry {
                ClientboundPlayRegistry::AddEntity {
                    id: position_tracker.entity_id_ref,
                    uuid: position_tracker.entity_uuid_ref,
                    entity_type: $entity_type,
                    location: position_tracker.last_tracked_location.inner_loc,
                    x_rot: position_tracker.rot_cache.1 as u8,
                    y_rot: position_tracker.rot_cache.0 as u8,
                    y_head_rot: position_tracker.rot_cache.2 as u8,
                    data: 0,
                    xa: 0,
                    ya: 0,
                    za: 0,
                }
            }
        }
    };
}

#[derive(Default)]
pub struct EntityTracker {
    pub entities: HashMap<Uuid, EntityPositionTracker>,
    pub removed_entities_since_last_tick: Vec<i32>,
}

pub struct EntityData {
    pub entity_location: Location,
    pub entity_on_ground: bool,
}

impl EntityTracker {
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            removed_entities_since_last_tick: vec![],
        }
    }

    pub fn contains(&self, entity_id: &Uuid) -> bool {
        self.entities.contains_key(entity_id)
    }

    pub fn add_entity<T: TrackableEntity>(&mut self, entity: &T) {
        let uuid = T::uuid(entity);
        self.entities.insert(
            uuid,
            EntityPositionTracker::create_from_initial_location(
                T::id(entity),
                uuid,
                T::location(entity),
                T::on_ground(entity),
                T::create_entity,
                TrackingState::UnknownNonPlayerEntity,
            ),
        );
    }

    pub fn add_player<T: TrackableEntity>(&mut self, entity: &T, packet_sender: PacketSend) {
        let uuid = T::uuid(entity);
        self.entities.insert(
            uuid,
            EntityPositionTracker::create_from_initial_location(
                T::id(entity),
                uuid,
                T::location(entity),
                T::on_ground(entity),
                T::create_entity,
                TrackingState::WaitingForPlayers { packet_sender },
            ),
        );
    }

    pub fn remove_entity(&mut self, uuid: Uuid) {
        if let Some(removed) = self.entities.remove(&uuid) {
            // remove current & any lingering entities from the player
            if let TrackingState::WaitingForPlayers { packet_sender }
            | TrackingState::Active { packet_sender } = removed.state
            {
                let mut current_removed_entities = self.removed_entities_since_last_tick.clone();
                current_removed_entities.extend(self.entities.values().map(|x| x.entity_id_ref));
                let remove_packet = ClientboundPlayRegistry::RemoveEntities {
                    entity_ids: current_removed_entities,
                };
                let _ = packet_sender.send(Arc::new(remove_packet));
            }

            self.removed_entities_since_last_tick
                .push(removed.entity_id_ref);
        }
    }

    pub fn tick<PositionAccessor: Fn(Uuid) -> EntityData>(&mut self, accessor: PositionAccessor) {
        let mut broadcasts: Vec<ConditionalPacket<EntityPositionTracker>> = vec![];

        for entity in self.entities.values_mut() {
            let EntityData {
                entity_location,
                entity_on_ground,
            } = accessor(entity.entity_uuid_ref);
            let packets = entity.poll_location(entity_location, entity_on_ground);

            let id = entity.entity_id_ref;

            let create_entity_packet = (entity.create_entity_fn)(entity);

            match entity.state {
                TrackingState::WaitingForPlayers { .. } | TrackingState::UnknownNonPlayerEntity => {
                    broadcasts.push(ConditionalPacket::Conditional(
                        create_entity_packet,
                        Box::new(move |e| e.entity_id_ref != id),
                    ));
                }
                TrackingState::Active { .. } | TrackingState::KnownNonPlayerEntity => {
                    broadcasts.push(ConditionalPacket::Conditional(
                        create_entity_packet,
                        Box::new(move |e| {
                            e.entity_id_ref != id
                                && matches!(e.state, TrackingState::WaitingForPlayers { .. })
                        }),
                    ));
                    broadcasts.extend(packets.into_iter().map(|x| {
                        ConditionalPacket::<EntityPositionTracker>::Conditional(
                            x,
                            Box::new(move |e| {
                                e.entity_id_ref != id
                                    && matches!(e.state, TrackingState::Active { .. })
                            }),
                        )
                    }));
                }
            }
        }

        broadcasts.push(ConditionalPacket::Unconditional(
            ClientboundPlayRegistry::RemoveEntities {
                entity_ids: self.removed_entities_since_last_tick.drain(..).collect(),
            },
        ));

        for packet in broadcasts {
            packet.send_to_clients(self.entities.values(), |entity, packet| {
                if let TrackingState::WaitingForPlayers { packet_sender }
                | TrackingState::Active { packet_sender } = &entity.state
                {
                    let _ = packet_sender.send(packet);
                }
            });
        }

        for client in self.entities.values_mut() {
            if let TrackingState::WaitingForPlayers { packet_sender } = &client.state {
                client.state = TrackingState::Active {
                    packet_sender: packet_sender.clone(),
                };
            } else if let TrackingState::UnknownNonPlayerEntity = &client.state {
                client.state = TrackingState::KnownNonPlayerEntity;
            }
        }
    }
}
