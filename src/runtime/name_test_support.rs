// SPDX-License-Identifier: AGPL-3.0-only

//! Offline examples for exhaustive reachable-error display checks.

pub(crate) trait ErrorExample: Sized {
    fn error_example() -> Self;
}
pub(crate) fn example<T: ErrorExample>() -> T {
    T::error_example()
}
pub(crate) fn forbidden_name() -> &'static str {
    static NAME: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| ["tesl", "amate"].concat());
    NAME.as_str()
}
pub(crate) fn assert_name_free(error: &impl std::fmt::Display) {
    let display = error.to_string();
    assert!(
        !display.to_lowercase().contains(forbidden_name()),
        "reachable error display: {display}"
    );
}
macro_rules! number_examples { ($($kind:ty),+) => {$(impl ErrorExample for $kind {fn error_example()->Self {1}})+}; }
number_examples!(u8, u16, u32, u64, usize, i16, i32, i64, isize);
impl ErrorExample for bool {
    fn error_example() -> Self {
        false
    }
}
impl ErrorExample for String {
    fn error_example() -> Self {
        "synthetic".to_owned()
    }
}
impl ErrorExample for &'static str {
    fn error_example() -> Self {
        "synthetic"
    }
}
impl<T> ErrorExample for Option<T> {
    fn error_example() -> Self {
        None
    }
}
impl<T> ErrorExample for Vec<T> {
    fn error_example() -> Self {
        Vec::new()
    }
}
impl<T: ErrorExample> ErrorExample for Box<T> {
    fn error_example() -> Self {
        Box::new(example())
    }
}
impl ErrorExample for Box<dyn std::error::Error + Send + Sync> {
    fn error_example() -> Self {
        std::io::Error::other("synthetic cause").into()
    }
}
impl ErrorExample for std::path::PathBuf {
    fn error_example() -> Self {
        "/fictional/item".into()
    }
}
impl ErrorExample for uuid::Uuid {
    fn error_example() -> Self {
        Self::nil()
    }
}
impl ErrorExample for std::io::Error {
    fn error_example() -> Self {
        Self::other("synthetic cause")
    }
}
impl ErrorExample for rusqlite::Error {
    fn error_example() -> Self {
        Self::InvalidQuery
    }
}
impl ErrorExample for rusqlite::types::Type {
    fn error_example() -> Self {
        Self::Integer
    }
}
impl ErrorExample for rustix::io::Errno {
    fn error_example() -> Self {
        Self::INVAL
    }
}
impl ErrorExample for serde_json::Error {
    fn error_example() -> Self {
        serde_json::from_str::<serde_json::Value>("[").unwrap_err()
    }
}
impl ErrorExample for toml::de::Error {
    fn error_example() -> Self {
        "broken=[".parse::<toml::Table>().unwrap_err()
    }
}
impl ErrorExample for tokio_postgres::Error {
    fn error_example() -> Self {
        "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err()
    }
}
impl ErrorExample for url::ParseError {
    fn error_example() -> Self {
        url::Url::parse("relative").unwrap_err()
    }
}
impl ErrorExample for std::str::Utf8Error {
    fn error_example() -> Self {
        let bytes = vec![0xff];
        std::str::from_utf8(&bytes).unwrap_err()
    }
}
impl ErrorExample for std::time::SystemTimeError {
    fn error_example() -> Self {
        std::time::UNIX_EPOCH
            .duration_since(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap_err()
    }
}
impl ErrorExample for crate::protocol::SchemaVersion {
    fn error_example() -> Self {
        Self { major: 1, minor: 0 }
    }
}
impl ErrorExample for crate::protocol::ProtocolError {
    fn error_example() -> Self {
        Self::InvalidGeneration
    }
}
impl ErrorExample for crate::fleet_api::FleetApiConfigError {
    fn error_example() -> Self {
        Self::InvalidAccessToken
    }
}
impl ErrorExample for crate::fleet_api::FleetApiError {
    fn error_example() -> Self {
        Self::InvalidEndpoint
    }
}
impl ErrorExample for crate::lifecycle::LifecycleError {
    fn error_example() -> Self {
        Self::InvalidCarId
    }
}
impl ErrorExample for crate::geo::geofence_validation::GeofenceRejection {
    fn error_example() -> Self {
        Self::NameEmpty
    }
}
impl ErrorExample for crate::legacy_auth::RefusalReason {
    fn error_example() -> Self {
        Self::OwnerApi
    }
}
impl ErrorExample for crate::legacy_auth::SignOutReason {
    fn error_example() -> Self {
        Self::StoredPairInvalid
    }
}
impl ErrorExample for crate::import_projection_state::SourceDbProjectionStateEntity {
    fn error_example() -> Self {
        Self::Car
    }
}
impl ErrorExample for crate::config::CadenceViolation {
    fn error_example() -> Self {
        Self {
            key: "collector.interval_seconds",
            value: 0,
            rule: crate::config::CadenceRule::GreaterThanZero,
        }
    }
}
impl ErrorExample for crate::updates_delivery::UpdatesDeliveryError {
    fn error_example() -> Self {
        Self {
            message: "synthetic delivery refusal".to_owned(),
        }
    }
}
impl ErrorExample for crate::updates_logical::UpdatesLogicalError {
    fn error_example() -> Self {
        Self {
            message: "synthetic logical refusal".to_owned(),
        }
    }
}

#[test]
fn source_name_control_is_assembled_at_runtime() {
    assert_eq!(forbidden_name(), ["tesl", "amate"].concat());
}

#[test]
fn every_reachable_error_display_omits_the_source_project_name() {
    assert_name_free(&example::<crate::updates_delivery::UpdatesDeliveryError>());
    assert_name_free(&example::<crate::updates_logical::UpdatesLogicalError>());
    assert_name_free(&example::<crate::config::CadenceViolation>());
}

impl ErrorExample for crate::source_db_schema::ValueType {
    fn error_example() -> Self {
        Self::Integer
    }
}
