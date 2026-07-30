#![forbid(unsafe_code)]

pub mod collector;
pub mod config;
pub mod credentials;
pub mod crypto;
pub mod db;
pub mod http_range;
pub mod hub_pack;
pub mod lifecycle;
mod manifest_signing;
pub mod owner_api;
pub mod protocol;
pub mod server;
pub mod setup;
pub mod teslamate;
pub mod teslamate_fragments;
pub mod teslamate_import;
pub mod teslamate_projection;
pub mod teslamate_reader;
pub mod teslamate_schema;
pub mod teslamate_stage;
pub mod transport;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
