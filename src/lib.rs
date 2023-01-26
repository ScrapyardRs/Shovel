#![feature(macro_metavar_expr)]
#![feature(iter_next_chunk)]
#![feature(int_roundings)]
#![feature(variant_count)]
#![feature(once_cell)]
#![feature(const_for)]
#![feature(generic_const_exprs)]
#![feature(const_trait_impl)]
#![feature(const_mut_refs)]
#![feature(const_intoiterator_identity)]
#![feature(const_swap)]

use mcprotocol::clientbound::play::ClientboundPlayRegistry;
use mcprotocol::common::registry::GlobalRegistry;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[macro_export]
macro_rules! none_arr {
    ($size:literal) => {
        [Option::<()>::None; $size].map(|_| None)
    };
}

pub mod client;
pub mod crypto;
pub mod entity;
pub mod inventory;
pub mod level;
pub mod math;
pub mod phase;
pub mod server;
pub mod tick;

#[macro_export]
macro_rules! lazy_lock {
    ($ident:ident -> $ty:ty) => {
        pub static $ident: std::sync::LazyLock<$ty> = std::sync::LazyLock::new(|| <$ty>::create());
    };
}

lazy_lock!(GLOBAL_REGISTRIES -> GlobalRegistry);

pub type PacketSend = UnboundedSender<Arc<ClientboundPlayRegistry>>;

pub mod version_constants {
    pub const CURRENT_PROTOCOL_VERSION: i32 = 761;
    pub const CURRENT_PROTOCOL_VERSION_STRING: &str = "1.19.3";
}
