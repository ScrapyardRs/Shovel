#![feature(macro_metavar_expr)]
#![feature(iter_next_chunk)]
#![feature(int_roundings)]
#![feature(variant_count)]

pub mod client;
pub mod crypto;
pub mod phase;
pub mod server;

#[macro_export]
macro_rules! spawn_local {
    ($(@pre { $pre_tt:tt* })?$($tt:tt)*) => {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| {
            err_explain!(format!("Error setting up thread builder: {}", err))
        })?;

        std::thread::spawn(move || {
            let local = tokio::task::LocalSet::new();
            $($pre_tt)*
            local.spawn_local(async move {
               $($tt)*
            });
            rt.block_on(local);
        });
    };
}

pub mod version_constants {
    pub const CURRENT_PROTOCOL_VERSION: i32 = 761;
    pub const CURRENT_PROTOCOL_VERSION_STRING: &str = "1.19.3";
}
