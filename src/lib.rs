#![forbid(unsafe_code)]
// Intentional API surface: multi-arg import/finalize and complex store
// closures are preferred over artificial parameter objects for now.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod collector;
pub mod config;
pub mod credentials;
pub mod crypto;
pub mod data_recovery;
pub mod db;
#[cfg(test)]
pub mod fake_tesla;
pub mod geocoder;
pub mod http_range;
pub mod hub_pack;
#[cfg(unix)]
#[doc(hidden)]
pub mod hub_user_process;
pub mod legacy_auth;
pub mod lifecycle;
pub mod location;
#[cfg(target_os = "macos")]
pub mod macos_launch_agent;
mod manifest_signing;
pub mod owner_api;
pub mod protocol;
pub mod server;
pub mod terrain;
pub mod terrain_cache;
pub mod tesla_stream;
pub mod teslamate;
pub mod teslamate_credentials;
pub mod teslamate_direct;
pub mod teslamate_fragments;
pub mod teslamate_import;
pub mod teslamate_parity;
pub mod teslamate_projection;
pub mod teslamate_projection_state;
pub mod teslamate_reader;
pub mod teslamate_schema;
pub mod teslamate_stage;
pub mod teslamate_token;
pub mod transport;
pub mod updates_delivery;
pub mod updates_logical;
#[cfg(unix)]
pub(crate) mod user_lifetime_lock;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
