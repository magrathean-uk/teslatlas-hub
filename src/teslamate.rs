//! TeslaMate is a migration-only source. This module intentionally contains no
//! source writes and no credential transport.

use std::{fmt, net::IpAddr};

use thiserror::Error;
use url::Url;

use crate::teslamate_schema::READ_ONLY_SESSION_SQL;

pub const REQUIRED_TABLES: &[&str] = &[
    "cars",
    "drives",
    "positions",
    "charging_processes",
    "charges",
    "addresses",
    "geofences",
    "states",
    "updates",
];

/// A credential-free PostgreSQL endpoint for a TeslaMate read-only import.
/// Database credentials are supplied out-of-band through a protected local
/// file or stdin.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadOnlySource {
    url: Url,
}

impl ReadOnlySource {
    pub fn parse(value: &str) -> Result<Self, TeslaMateSourceError> {
        let url = Url::parse(value).map_err(TeslaMateSourceError::Url)?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err(TeslaMateSourceError::Scheme);
        }
        let host = url.host_str().ok_or(TeslaMateSourceError::Host)?;
        if host.contains(',') {
            return Err(TeslaMateSourceError::MultipleHosts);
        }
        if url.password().is_some() {
            return Err(TeslaMateSourceError::EmbeddedSecret);
        }
        if url.path().trim_matches('/').is_empty() {
            return Err(TeslaMateSourceError::Database);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(TeslaMateSourceError::Parameters);
        }
        Ok(Self { url })
    }

    pub fn database_name(&self) -> &str {
        self.url.path().trim_start_matches('/')
    }

    pub fn host(&self) -> &str {
        self.url.host_str().expect("validated host")
    }

    /// `url` retains brackets around IPv6 literals; tokio-postgres expects
    /// the literal address without URL syntax.
    pub fn connection_host(&self) -> &str {
        self.host().trim_matches(['[', ']'])
    }

    pub fn port(&self) -> u16 {
        self.url.port().unwrap_or(5432)
    }

    pub fn user(&self) -> Option<&str> {
        let user = self.url.username();
        (!user.is_empty()).then_some(user)
    }

    pub fn is_loopback(&self) -> bool {
        self.host()
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    }

    /// Session setup that must execute before inspecting the source schema.
    pub fn session_sql(&self) -> [&'static str; 3] {
        READ_ONLY_SESSION_SQL
    }

    /// Schema check that never invokes an unqualified source relation.
    pub fn schema_check_sql(&self) -> &'static str {
        "SELECT relname FROM pg_catalog.pg_class \
         WHERE relnamespace = 'public'::pg_catalog.regnamespace \
         AND relkind = 'r' \
         AND relname = ANY(ARRAY['cars','drives','positions','charging_processes','charges','addresses','geofences','states','updates']) \
         ORDER BY relname"
    }
}

impl fmt::Debug for ReadOnlySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadOnlySource")
            .field("scheme", &self.url.scheme())
            .field("userinfo", &self.user().map(|_| "[redacted]"))
            .field("host", &self.host())
            .field("port", &self.port())
            .field("database", &self.database_name())
            .finish()
    }
}

impl fmt::Display for ReadOnlySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "postgresql://")?;
        if self.user().is_some() {
            write!(formatter, "[redacted]@")?;
        }
        write!(
            formatter,
            "{}:{}/{}",
            self.host(),
            self.port(),
            self.database_name()
        )
    }
}

#[derive(Debug, Error)]
pub enum TeslaMateSourceError {
    #[error("invalid PostgreSQL source URL: {0}")]
    Url(url::ParseError),
    #[error("TeslaMate migration requires a postgres or postgresql URL")]
    Scheme,
    #[error("TeslaMate migration source requires a host")]
    Host,
    #[error("TeslaMate migration source requires exactly one PostgreSQL host")]
    MultipleHosts,
    #[error("TeslaMate migration source requires a database name")]
    Database,
    #[error("embedded source credentials are not permitted")]
    EmbeddedSecret,
    #[error("source URL query parameters and fragments are not permitted")]
    Parameters,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_credential_free_postgres_source() {
        let source = ReadOnlySource::parse("postgresql://teslamate@127.0.0.1:5433/teslamate")
            .expect("valid source");
        assert_eq!(source.host(), "127.0.0.1");
        assert_eq!(source.port(), 5433);
        assert_eq!(source.database_name(), "teslamate");
        assert_eq!(
            source.to_string(),
            "postgresql://[redacted]@127.0.0.1:5433/teslamate"
        );
        assert!(source.session_sql()[1].contains("REPEATABLE READ, READ ONLY"));
        assert!(source.session_sql()[2].contains("TIME ZONE 'UTC'"));
    }

    #[test]
    fn defaults_postgres_urls_to_the_standard_port() {
        let source =
            ReadOnlySource::parse("postgresql://reader@127.0.0.1/teslamate").expect("valid source");
        assert_eq!(source.port(), 5432);
        assert!(source.is_loopback());
    }

    #[test]
    fn supports_literal_loopback_and_remote_postgres_hosts() {
        let ipv6 = ReadOnlySource::parse("postgresql://reader@[::1]/teslamate")
            .expect("IPv6 loopback source");
        assert_eq!(ipv6.host(), "[::1]");
        assert_eq!(ipv6.connection_host(), "::1");
        for source in [
            "postgresql://reader@localhost/teslamate",
            "postgresql://reader@db.example/teslamate",
            "postgresql://reader@192.168.1.2/teslamate",
        ] {
            assert!(ReadOnlySource::parse(source).is_ok(), "{source}");
        }
        assert!(matches!(
            ReadOnlySource::parse("postgresql://reader@127.0.0.1,127.0.0.2/teslamate"),
            Err(TeslaMateSourceError::MultipleHosts)
        ));
    }

    #[test]
    fn rejects_embedded_secrets_and_connection_options() {
        let secret = ReadOnlySource::parse("postgresql://reader:secret@localhost/teslamate")
            .expect_err("password must be rejected");
        assert!(matches!(secret, TeslaMateSourceError::EmbeddedSecret));

        let option =
            ReadOnlySource::parse("postgresql://reader@localhost/teslamate?sslmode=disable")
                .expect_err("connection parameters must be rejected");
        assert!(matches!(option, TeslaMateSourceError::Parameters));

        assert!(matches!(
            ReadOnlySource::parse("postgresql://reader@127.0.0.1/teslamate?sslmode=disable"),
            Err(TeslaMateSourceError::Parameters)
        ));
        assert!(
            ReadOnlySource::parse("postgresql://reader@192.168.1.2/teslamate?sslmode=disable")
                .is_err()
        );
        assert!(matches!(
            ReadOnlySource::parse("postgresql://reader@db.example/teslamate#sslmode=disable"),
            Err(TeslaMateSourceError::Parameters)
        ));
        assert!(
            ReadOnlySource::parse("postgresql://reader:sec%72et@db.example/teslamate").is_err()
        );
    }

    #[test]
    fn source_errors_never_echo_embedded_secrets() {
        for (url, markers) in [
            (
                "postgresql://reader:postgres-secret@127.0.0.1/db",
                &["reader", "postgres-secret"][..],
            ),
            (
                "postgresql://encoded-user:percent%2Dencoded%2Dpassword@127.0.0.1/db",
                &[
                    "encoded-user",
                    "percent%2Dencoded%2Dpassword",
                    "percent-encoded-password",
                ][..],
            ),
        ] {
            let error = ReadOnlySource::parse(url).expect_err("secret is forbidden");
            let rendered = format!("{error} {error:?}");
            for marker in markers {
                assert!(!rendered.contains(marker), "leaked marker {marker}");
            }
        }
    }

    #[test]
    fn source_display_and_debug_redact_plain_and_percent_encoded_userinfo() {
        for (url, markers) in [
            (
                "postgresql://plain-user-marker@db.example:5433/teslamate",
                &["plain-user-marker"][..],
            ),
            (
                "postgresql://encoded-user%40marker@db.example:5433/teslamate",
                &["encoded-user", "%40marker", "@marker"][..],
            ),
        ] {
            let source = ReadOnlySource::parse(url).expect("credential-free source URL");
            let rendered = format!("{source} {source:?}");
            for marker in markers {
                assert!(!rendered.contains(marker), "leaked marker {marker}");
            }
            assert!(rendered.contains("[redacted]"));
            assert!(rendered.contains("db.example"));
            assert!(rendered.contains("5433"));
            assert!(rendered.contains("teslamate"));
        }
    }

    #[test]
    fn schema_check_is_fully_qualified() {
        let source = ReadOnlySource::parse("postgres://127.0.0.1/teslamate").expect("source");
        assert!(source.schema_check_sql().contains("pg_catalog.pg_class"));
        assert!(
            source
                .schema_check_sql()
                .contains("'public'::pg_catalog.regnamespace")
        );
        assert_eq!(REQUIRED_TABLES.len(), 9);
    }
}
