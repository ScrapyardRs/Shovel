#![feature(macro_metavar_expr)]
#![feature(iter_next_chunk)]
#![feature(int_roundings)]
#![feature(variant_count)]

pub mod client;
pub mod crypto;
pub mod phase;
pub mod server;
pub mod tick;
pub(crate) mod math;

pub mod version_constants {
    pub const CURRENT_PROTOCOL_VERSION: i32 = 761;
    pub const CURRENT_PROTOCOL_VERSION_STRING: &str = "1.19.3";
}
