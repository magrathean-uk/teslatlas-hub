#![forbid(unsafe_code)]

pub mod collector;
pub mod config;
pub mod credentials;
pub mod crypto;
pub mod db;
pub mod geocoder;
pub mod http_range;
pub mod hub_pack;
pub mod legacy_auth;
pub mod legacy_token_state;
pub mod lifecycle;
pub mod location;
mod manifest_signing;
pub mod mqtt;
pub mod owner_api;
pub mod protocol;
pub mod server;
pub mod setup;
pub mod tesla_stream;
pub mod teslamate;
pub mod teslamate_direct;
pub mod teslamate_fragments;
pub mod teslamate_import;
pub mod teslamate_projection;
pub mod teslamate_reader;
pub mod teslamate_schema;
pub mod teslamate_stage;
pub mod teslamate_token;
pub mod transport;
pub mod terrain;
pub mod terrain_cache;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
