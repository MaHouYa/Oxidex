pub mod config;
pub mod daemon_model;
pub mod device;
pub mod index;
pub mod ipc;
pub mod model;
pub mod rules;
pub mod scanner;
pub mod snapshot;
pub mod stream;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
