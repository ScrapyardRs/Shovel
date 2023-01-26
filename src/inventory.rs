use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    ContainerSetContent, ContainerSetSlot, OpenScreen,
};
use mcprotocol::clientbound::play::{ClientboundPlayRegistry, MenuType};
use mcprotocol::common::chat::Chat;
use mcprotocol::common::play::ItemStack;
use mcprotocol::serverbound::play::{ClickType, ContainerSlot};

use crate::phase::play::ConnectedPlayer;

pub mod item;

pub struct PlayerInventory {
    pub crafting_output: Option<ItemStack>,
    pub crafting_slots: [[Option<ItemStack>; 2]; 2],
    pub equipment_slots: [Option<ItemStack>; 4],
    pub inventory_slots: [[Option<ItemStack>; 9]; 4],
    pub offhand_slot: Option<ItemStack>,
    pub current_slot: u8,
    pub state_id: i32,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        PlayerInventory {
            crafting_output: None,
            crafting_slots: [none_arr!(2), none_arr!(2)],
            inventory_slots: [none_arr!(9), none_arr!(9), none_arr!(9), none_arr!(9)],
            equipment_slots: none_arr!(4),
            offhand_slot: None,
            current_slot: 0,
            state_id: 0,
        }
    }
}

impl PlayerInventory {
    pub fn next_state_id(&mut self) -> i32 {
        self.state_id += 1;
        self.state_id - 1
    }

    pub fn current_state_id(&self) -> i32 {
        self.state_id
    }

    pub fn clear(&mut self) -> ClientboundPlayRegistry {
        self.crafting_output = None;
        self.crafting_slots = [none_arr!(2), none_arr!(2)];
        self.inventory_slots = [none_arr!(9), none_arr!(9), none_arr!(9), none_arr!(9)];
        self.equipment_slots = none_arr!(4);
        self.offhand_slot = None;
        self.refresh()
    }

    pub fn refresh(&self) -> ClientboundPlayRegistry {
        let next_state_id = self.current_state_id();

        let mut items = Vec::with_capacity(46);

        items.push(self.crafting_output.as_ref().cloned());

        for row in self.crafting_slots.iter() {
            for item in row.iter() {
                items.push(item.as_ref().cloned());
            }
        }

        for item in self.equipment_slots.iter() {
            items.push(item.as_ref().cloned());
        }

        for slots in &self.inventory_slots {
            for slot in slots {
                items.push(slot.as_ref().cloned());
            }
        }

        items.push(self.offhand_slot.as_ref().cloned());

        ContainerSetContent {
            container_id: 0,
            state_id: next_state_id,
            items,
            carried_item: None,
        }
    }

    pub fn set_all(&mut self, items: &[Option<ItemStack>]) -> ClientboundPlayRegistry {
        for (i, item) in items.iter().enumerate() {
            self.inventory_slots[i / 9][i % 9] = item.as_ref().cloned();
        }
        self.refresh()
    }

    pub fn set(
        &mut self,
        item: Option<ItemStack>,
        slot_x: usize,
        slot_y: usize,
    ) -> ClientboundPlayRegistry {
        self.inventory_slots[slot_y][slot_x] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: (((slot_y * 9) + slot_x) + 9) as u16,
            item,
        }
    }

    pub fn set_head(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.equipment_slots[0] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 5,
            item,
        }
    }

    pub fn set_chest(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.equipment_slots[1] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 6,
            item,
        }
    }

    pub fn set_legs(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.equipment_slots[2] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 7,
            item,
        }
    }

    pub fn set_feet(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.equipment_slots[3] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 8,
            item,
        }
    }

    pub fn set_offhand(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.offhand_slot = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 45,
            item,
        }
    }

    pub fn set_crafting_output(&mut self, item: Option<ItemStack>) -> ClientboundPlayRegistry {
        self.crafting_output = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: 0,
            item,
        }
    }

    pub fn set_crafting_slot(
        &mut self,
        item: Option<ItemStack>,
        slot_x: usize,
        slot_y: usize,
    ) -> ClientboundPlayRegistry {
        self.crafting_slots[slot_y][slot_x] = item.as_ref().cloned();
        let next_state_id = self.next_state_id();
        ContainerSetSlot {
            container_id: 0,
            state_id: next_state_id,
            slot: (((slot_y * 2) + slot_x) + 1) as u16,
            item,
        }
    }

    pub fn set_current_slot_unaware(&mut self, slot: u8) {
        self.current_slot = slot;
    }

    pub fn set_current_slot(&mut self, slot: u8) -> ClientboundPlayRegistry {
        self.set_current_slot_unaware(slot);
        ClientboundPlayRegistry::SetCarriedItem { slot }
    }
}

pub enum ClickWith {
    Left,
    Right,
}

pub struct ClickContext<'a, C> {
    pub extra: &'a mut C,
    pub player: &'a mut ConnectedPlayer,
    pub menu_ref: &'a mut Menu<C>,
    pub click_type: ClickType,
    pub click_with: ClickWith,
    pub slot: u16,
    pub changed_slots: Vec<ContainerSlot>,
    pub carried_item: Option<ItemStack>,
}

pub type ClickHandler<C> = fn(ClickContext<C>);

pub struct MenuItem<C> {
    pub item: Option<ItemStack>,
    pub action: Option<ClickHandler<C>>,
}

impl<C> Clone for MenuItem<C> {
    fn clone(&self) -> Self {
        MenuItem {
            item: self.item.as_ref().cloned(),
            action: self.action,
        }
    }
}

impl<C> MenuItem<C> {
    pub fn empty() -> Self {
        Self {
            item: None,
            action: None,
        }
    }

    pub fn item_only(item: ItemStack) -> Self {
        Self {
            item: Some(item),
            action: None,
        }
    }

    pub fn full(item: ItemStack, action: ClickHandler<C>) -> Self {
        Self {
            item: Some(item),
            action: Some(action),
        }
    }
}

pub struct Menu<C> {
    title: Chat,
    rows: u8,
    container_id: u8,
    container_type: MenuType,
    items: Vec<MenuItem<C>>,
    state_lock: i32,
}

impl<C> Menu<C> {
    pub fn incr_state_lock(&mut self) {
        self.state_lock += 1;
    }

    pub fn send_to_player(&self, player: &mut ConnectedPlayer) {
        player.write_owned_packet(OpenScreen {
            container_id: self.container_id as i32,
            container_type: self.container_type,
            title: self.title.clone(),
        });
        self.refresh_contents(player);
    }

    pub fn refresh_contents(&self, player: &mut ConnectedPlayer) {
        let mut total_items = Vec::<Option<ItemStack>>::with_capacity(self.items.len() * 9);
        for y in 0..self.rows as usize {
            for x in 0..9 {
                total_items.push(self.items[(y * 9) + x].item.as_ref().cloned());
            }
        }

        player.write_owned_packet(ContainerSetContent {
            container_id: self.container_id,
            state_id: self.state_lock,
            items: total_items,
            carried_item: None,
        });
    }

    pub fn get_clicker(&self, state_id: i32, slot: u16) -> Option<ClickHandler<C>> {
        if self.state_lock > state_id {
            return None;
        }

        if slot <= self.items.len() as u16 {
            return Some(
                self.items[slot as usize]
                    .action
                    .as_ref()
                    .cloned()
                    .unwrap_or(|ctx| {
                        ctx.menu_ref.refresh_contents(ctx.player);
                    }),
            );
        }

        Some(|ctx| {
            ctx.menu_ref.refresh_contents(ctx.player);
            let update_packet = ctx.player.player_inventory_mut().refresh();
            ctx.player.write_owned_packet(update_packet);
        })
    }

    pub fn close(&self, player: &mut ConnectedPlayer) {
        player.write_owned_packet(ClientboundPlayRegistry::ContainerClose {
            container_id: self.container_id,
        });
    }

    pub fn set_click_handler(&mut self, slot_x: usize, slot_y: usize, handler: ClickHandler<C>) {
        self.items[(slot_y * 9) + slot_x].action = Some(handler);
    }

    pub fn clear_click_handler(&mut self, slot_x: usize, slot_y: usize) {
        self.items[(slot_y * 9) + slot_x].action = None;
    }

    pub fn set_items_unaware(&mut self, items: &[MenuItem<C>]) -> bool {
        let mut changed = false;
        for (i, item) in items.iter().enumerate() {
            if self.items[i].item.ne(&item.item) {
                self.items[i] = item.clone();
                if !changed {
                    self.incr_state_lock();
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn set_item_unaware(
        &mut self,
        slot_x: usize,
        slot_y: usize,
        item: Option<ItemStack>,
    ) -> bool {
        let idx = (slot_y * 9) + slot_x;
        let changed = self.items[idx].item.eq(&item);
        if changed {
            self.incr_state_lock();
            self.items[idx].item = item;
        }
        changed
    }

    pub fn set_item(
        &mut self,
        to: &mut ConnectedPlayer,
        slot_x: usize,
        slot_y: usize,
        item: Option<ItemStack>,
    ) -> bool {
        let changed = self.set_item_unaware(slot_x, slot_y, item.clone());

        if !changed {
            return false;
        }

        to.write_owned_packet(ContainerSetSlot {
            container_id: self.container_id,
            state_id: self.state_lock,
            slot: ((slot_y * 9) + slot_x) as u16,
            item: self.items[(slot_y * 9) + slot_x].item.as_ref().cloned(),
        });

        true
    }

    pub fn from_rows<Ch: Into<Chat>>(title: Ch, rows: u8) -> Self {
        let container_type = match rows {
            1 => MenuType::Generic9x1,
            2 => MenuType::Generic9x2,
            3 => MenuType::Generic9x3,
            4 => MenuType::Generic9x4,
            5 => MenuType::Generic9x5,
            6 => MenuType::Generic9x6,
            _ => {
                panic!("Invalid number of rows for menu: {}", rows);
            }
        };
        let mut items = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            for _ in 0..9 {
                items.push(MenuItem {
                    item: None,
                    action: None,
                });
            }
        }
        Self {
            title: title.into(),
            container_id: 1,
            container_type,
            items,
            state_lock: 0,
            rows,
        }
    }
}
