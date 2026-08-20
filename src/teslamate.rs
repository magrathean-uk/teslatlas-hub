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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlySource {
    url: Url,
}

impl ReadOnlySource {
    pub fn parse(value: &str) -> Result<Self, TeslaMateSourceError> {
        let url = Url::parse(value).map_err(TeslaMateSourceError::Url)?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err(TeslaMateSourceError::Scheme);
        }
        if url.host_str().is_none() {
            return Err(TeslaMateSourceError::Host);
        }
        if url.password().is_some() {
            return Err(TeslaMateSourceError::EmbeddedSecret);
        }
        if !url
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback())
        {
            return Err(TeslaMateSourceError::LoopbackRequired);
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

impl fmt::Display for ReadOnlySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "postgresql://")?;
        if let Some(user) = self.user() {
            write!(formatter, "{user}@")?;
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
    #[error("TeslaMate migration source requires a literal loopback address")]
    LoopbackRequired,
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
            "postgresql://teslamate@127.0.0.1:5433/teslamate"
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
    fn requires_a_literal_loopback_postgres_host() {
        let ipv6 = ReadOnlySource::parse("postgresql://reader@[::1]/teslamate")
            .expect("IPv6 loopback source");
        assert_eq!(ipv6.host(), "[::1]");
        for source in [
            "postgresql://reader@localhost/teslamate",
            "postgresql://reader@db.example/teslamate",
            "postgresql://reader@127.0.0.1,127.0.0.2/teslamate",
            "postgresql://reader@192.168.1.2/teslamate",
        ] {
            assert!(matches!(
                ReadOnlySource::parse(source),
                Err(TeslaMateSourceError::LoopbackRequired)
                    | Err(TeslaMateSourceError::Host)
                    | Err(TeslaMateSourceError::Url(_))
            ));
        }
    }

    #[test]
    fn rejects_embedded_secrets_and_connection_options() {
        let secret = ReadOnlySource::parse("postgresql://reader:secret@localhost/teslamate")
            .expect_err("password must be rejected");
        assert!(matches!(secret, TeslaMateSourceError::EmbeddedSecret));

        let option =
            ReadOnlySource::parse("postgresql://reader@localhost/teslamate?sslmode=disable")
                .expect_err("hostname plaintext must be rejected");
        assert!(matches!(option, TeslaMateSourceError::LoopbackRequired));

        assert!(matches!(
            ReadOnlySource::parse("postgresql://reader@127.0.0.1/teslamate?sslmode=disable"),
            Err(TeslaMateSourceError::Parameters)
        ));
        assert!(
            ReadOnlySource::parse("postgresql://reader@192.168.1.2/teslamate?sslmode=disable")
                .is_err()
        );
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
