// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit, allow-listed TeslaMate write-back operations.
//!
//! Normal migration and collection never call this module. Every operation
//! targets one typed TeslaMate row, defaults to rollback, and commits only
//! when the caller explicitly requests `apply`.

use std::{str::FromStr, time::Duration};

use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use tokio::time::timeout;
use tokio_postgres::{Config, IsolationLevel, NoTls, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{credentials::TeslaMatePostgresPassword, teslamate::ReadOnlySource};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_BACK_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeslaMateCost(Decimal);

impl FromStr for TeslaMateCost {
    type Err = TeslaMateWriteBackError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value =
            Decimal::from_str_exact(value).map_err(|_| TeslaMateWriteBackError::InvalidCost)?;
        if value.is_sign_negative()
            || value.scale() > 2
            || value > Decimal::new(99_999_999_999_999, 2)
        {
            return Err(TeslaMateWriteBackError::InvalidCost);
        }
        Ok(Self(value))
    }
}

impl std::fmt::Display for TeslaMateCost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargeCostWriteBackReceipt {
    pub applied: bool,
    pub car_id: i64,
    pub charging_process_id: i64,
    pub previous_cost: Option<String>,
    pub new_cost: String,
    pub affected_rows: u64,
}

pub async fn write_back_charge_cost(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    car_id: i64,
    charging_process_id: i64,
    cost: TeslaMateCost,
    apply: bool,
) -> Result<ChargeCostWriteBackReceipt, TeslaMateWriteBackError> {
    let car_id = i16::try_from(car_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(TeslaMateWriteBackError::InvalidCarId)?;
    let charging_process_id = i32::try_from(charging_process_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(TeslaMateWriteBackError::InvalidChargingProcessId)?;
    let (mut client, mut connection_task) = connect_writable(source, password).await?;
    let result = timeout(WRITE_BACK_TIMEOUT, async {
        let transaction = client
            .build_transaction()
            .isolation_level(IsolationLevel::Serializable)
            .start()
            .await
            .map_err(|_| TeslaMateWriteBackError::Transaction)?;
        transaction
            .batch_execute(
                "SET LOCAL statement_timeout = '30s';
                 SET LOCAL lock_timeout = '5s';
                 SET LOCAL search_path = pg_catalog;",
            )
            .await
            .map_err(|_| TeslaMateWriteBackError::Transaction)?;
        let read_only: String = transaction
            .query_one("SHOW transaction_read_only", &[])
            .await
            .map_err(|_| TeslaMateWriteBackError::Transaction)?
            .get(0);
        if read_only != "off" {
            return Err(TeslaMateWriteBackError::ReadOnlyRole);
        }
        let row = transaction
            .query_opt(
                "SELECT cost
                   FROM public.charging_processes
                  WHERE id = $1 AND car_id = $2
                  FOR UPDATE",
                &[&charging_process_id, &car_id],
            )
            .await
            .map_err(|_| TeslaMateWriteBackError::Query)?
            .ok_or(TeslaMateWriteBackError::ChargingProcessNotFound)?;
        let previous: Option<Decimal> = row
            .try_get(0)
            .map_err(|_| TeslaMateWriteBackError::InvalidStoredCost)?;
        let changed = previous != Some(cost.0);
        let affected_rows = if apply && changed {
            transaction
                .execute(
                    "UPDATE public.charging_processes
                        SET cost = $1
                      WHERE id = $2 AND car_id = $3",
                    &[&cost.0, &charging_process_id, &car_id],
                )
                .await
                .map_err(|_| TeslaMateWriteBackError::Update)?
        } else {
            0
        };
        if apply && changed && affected_rows != 1 {
            return Err(TeslaMateWriteBackError::AffectedRowMismatch);
        }
        let receipt = ChargeCostWriteBackReceipt {
            applied: apply,
            car_id: i64::from(car_id),
            charging_process_id: i64::from(charging_process_id),
            previous_cost: previous.map(|value| value.to_string()),
            new_cost: cost.to_string(),
            affected_rows,
        };
        if apply {
            transaction
                .commit()
                .await
                .map_err(|_| TeslaMateWriteBackError::Commit)?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|_| TeslaMateWriteBackError::Rollback)?;
        }
        Ok(receipt)
    })
    .await
    .unwrap_or(Err(TeslaMateWriteBackError::OperationTimeout));
    drop(client);
    let connection_result = timeout(CONNECTION_SHUTDOWN_TIMEOUT, &mut connection_task).await;
    if connection_result.is_err() {
        connection_task.abort();
        let _ = connection_task.await;
        return match result {
            Err(error) => Err(error),
            Ok(_) => Err(TeslaMateWriteBackError::ConnectionShutdown),
        };
    }
    if matches!(connection_result, Ok(Ok(Err(_))) | Ok(Err(_))) && result.is_ok() {
        return Err(TeslaMateWriteBackError::Connection);
    }
    result
}

async fn connect_writable(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
) -> Result<
    (
        tokio_postgres::Client,
        tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
    ),
    TeslaMateWriteBackError,
> {
    let user = source
        .user()
        .ok_or(TeslaMateWriteBackError::SourceUserRequired)?;
    let mut configuration = Config::new();
    configuration
        .host(source.connection_host())
        .port(source.port())
        .user(user)
        .password(password.as_str())
        .dbname(source.database_name());
    if source.is_loopback() {
        configuration.ssl_mode(SslMode::Disable);
        let (client, connection) = timeout(CONNECT_TIMEOUT, configuration.connect(NoTls))
            .await
            .map_err(|_| TeslaMateWriteBackError::ConnectTimeout)?
            .map_err(|_| TeslaMateWriteBackError::Connection)?;
        return Ok((client, tokio::spawn(connection)));
    }

    crate::crypto::install_default_provider();
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| TeslaMateWriteBackError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }
    configuration.ssl_mode(SslMode::Require);
    let (client, connection) = timeout(CONNECT_TIMEOUT, configuration.connect(tls))
        .await
        .map_err(|_| TeslaMateWriteBackError::ConnectTimeout)?
        .map_err(|_| TeslaMateWriteBackError::Connection)?;
    Ok((client, tokio::spawn(connection)))
}

#[derive(Debug, Error)]
pub enum TeslaMateWriteBackError {
    #[error("TeslaMate write-back requires a source user")]
    SourceUserRequired,
    #[error("TeslaMate car id is invalid")]
    InvalidCarId,
    #[error("TeslaMate charging-process id is invalid")]
    InvalidChargingProcessId,
    #[error(
        "TeslaMate charge cost must be non-negative, at most 12 integer digits, and at most 2 decimal places"
    )]
    InvalidCost,
    #[error("TeslaMate write-back connection timed out")]
    ConnectTimeout,
    #[error("TeslaMate write-back connection failed")]
    Connection,
    #[error("TeslaMate write-back connection did not stop")]
    ConnectionShutdown,
    #[error("TeslaMate write-back native trust store is unavailable")]
    NativeTrustStoreUnavailable,
    #[error("TeslaMate write-back transaction failed")]
    Transaction,
    #[error("TeslaMate write-back operation timed out")]
    OperationTimeout,
    #[error("TeslaMate write-back role is read-only")]
    ReadOnlyRole,
    #[error("TeslaMate charging process was not found for that car")]
    ChargingProcessNotFound,
    #[error("TeslaMate stored charge cost is invalid")]
    InvalidStoredCost,
    #[error("TeslaMate write-back query failed")]
    Query,
    #[error("TeslaMate charge-cost update failed")]
    Update,
    #[error("TeslaMate charge-cost update affected an unexpected row count")]
    AffectedRowMismatch,
    #[error("TeslaMate write-back commit failed")]
    Commit,
    #[error("TeslaMate write-back rollback failed")]
    Rollback,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_parser_matches_teslamate_numeric_14_2() {
        for valid in ["0", "0.01", "999999999999.99"] {
            assert!(valid.parse::<TeslaMateCost>().is_ok(), "{valid}");
        }
        for invalid in ["-1", "0.001", "1000000000000.00", "nan", ""] {
            assert!(invalid.parse::<TeslaMateCost>().is_err(), "{invalid}");
        }
    }
}
