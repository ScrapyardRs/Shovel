use drax::nbt::Tag;
use mcprotocol::common::chat::Chat;
use mcprotocol::common::play::ItemStack;
use mcprotocol::common::registry::RegistryKey;

use crate::GLOBAL_REGISTRIES;

pub struct ItemBuilder {
    item_type: i32,
    data: i32,
    count: u8,
    display_name: Option<Chat>,
    lore: Option<Vec<Chat>>,
    extra_nbt: Vec<(String, Tag)>,
}

impl ItemBuilder {
    pub fn new(item: &str) -> Self {
        let item_id = *GLOBAL_REGISTRIES.get_id(RegistryKey::Items, item).unwrap();
        Self {
            item_type: item_id,
            data: 0,
            count: 1,
            display_name: None,
            lore: None,
            extra_nbt: vec![],
        }
    }

    pub fn display_name<C: Into<Chat>>(mut self, chat: C) -> Self {
        self.display_name = Some(chat.into());
        self
    }

    pub fn lore<C: Into<Chat>>(mut self, lore: Vec<C>) -> Self {
        self.lore = Some(lore.into_iter().map(Into::into).collect());
        self
    }

    pub fn add_lore<C: Into<Chat>>(mut self, lore_line: C) -> Self {
        if let Some(lore) = &mut self.lore {
            lore.push(lore_line.into());
        } else {
            self.lore = Some(vec![lore_line.into()]);
        }
        self
    }

    pub fn add_all_lore<C: Into<Chat>>(mut self, in_lore: Vec<C>) -> Self {
        if let Some(lore) = &mut self.lore {
            lore.extend(in_lore.into_iter().map(Into::into));
        } else {
            self.lore = Some(in_lore.into_iter().map(|x| x.into()).collect());
        }
        self
    }

    pub fn set_lore<C: Into<Chat>>(mut self, line: usize, lore_line: C) -> Self {
        if let Some(lore) = &mut self.lore {
            lore[line] = lore_line.into();
        } else {
            panic!("Cannot set lore line {} on a non-existent lore", line);
        }
        self
    }

    pub fn add_all_nbt(mut self, nbt: Vec<(String, Tag)>) -> Self {
        self.extra_nbt.extend(nbt);
        self
    }

    pub fn add_nbt<S: Into<String>>(mut self, key: S, value: Tag) -> Self {
        self.extra_nbt.push((key.into(), value));
        self
    }

    pub fn build(self) -> ItemStack {
        let mut tag_data = vec![];
        if self.display_name.is_some() || self.lore.is_some() {
            let mut display_tag_data = vec![];
            if let Some(display_name) = self.display_name {
                display_tag_data.push((
                    "Name".to_string(),
                    Tag::string(serde_json::to_string(&display_name).unwrap()),
                ));
            }

            if let Some(lore) = self.lore {
                display_tag_data.push((
                    "Lore".to_string(),
                    Tag::TagList((
                        Tag::string("").get_tag_bit(),
                        lore.into_iter()
                            .map(|lore_line| {
                                Tag::string(serde_json::to_string(&lore_line).unwrap())
                            })
                            .collect(),
                    )),
                ));
            }

            tag_data.push(("display".to_string(), Tag::compound_tag(display_tag_data)));
        }
        if self.data != 0 {
            tag_data.push(("Damage".to_string(), Tag::TagInt(self.data)));
        }

        tag_data.extend(self.extra_nbt);

        if tag_data.is_empty() {
            ItemStack {
                item_id: self.item_type,
                count: self.count,
                tag: None,
            }
        } else {
            ItemStack {
                item_id: self.item_type,
                count: self.count,
                tag: Some(Tag::compound_tag(tag_data)),
            }
        }
    }
}
