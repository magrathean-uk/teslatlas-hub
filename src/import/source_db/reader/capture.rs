// SPDX-License-Identifier: AGPL-3.0-only

/// Read every fixed history projection inside one repeatable-read, read-only
/// transaction. It neither writes to PostgreSQL nor receives a source URL
/// containing credentials.
pub async fn read_history(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
) -> Result<SourceDbHistory, SourceDbReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(SourceDbReaderError::InvalidSelectedCarId);
    }
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = SourceDbSnapshotSession::new(client, connection_task);

    let result = read_history_in_session(session.client(), source, selected_car_id, limits).await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(history), Ok(())) => Ok(history),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Capture a source-consistent source database snapshot into one private local
/// SQLite stage. PostgreSQL rows are decoded and committed page-by-page; no
/// complete history vector exists while the source transaction is open.
///
/// An interrupted capture is explicitly discarded. PostgreSQL repeatable-read
/// snapshots cannot be safely resumed after a reconnect, so only a sealed
/// stage may move on to later pack production.
pub async fn capture_history_to_stage(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
) -> Result<SourceDbStage, SourceDbReaderError> {
    capture_history_to_stage_internal(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
        false,
    )
    .await
    .map(|(stage, _token, _session)| stage)
}

/// Capture history and the active open session from one source snapshot.
pub async fn capture_history_to_stage_with_session(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
) -> Result<(SourceDbStage, SourceDbOpenSession), SourceDbReaderError> {
    let (stage, _token, session) = capture_history_to_stage_internal(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
        false,
    )
    .await?;
    Ok((stage, session))
}

/// Capture history and the opaque legacy OAuth pair from one source snapshot.
/// The returned ciphertexts are never decrypted or rewritten here. Callers
/// that need cutover-consistent credentials should use this companion instead
/// of opening a second PostgreSQL transaction after history capture.
pub async fn capture_history_to_stage_with_legacy_token(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
) -> Result<(SourceDbStage, LegacyOwnerTokenCiphertexts), SourceDbReaderError> {
    let (stage, token, _session) = capture_history_to_stage_internal(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
        true,
    )
    .await?;
    let token = match token {
        Some(token) => token,
        None => {
            return Err(discard_stage_after_error(
                stage,
                SourceDbReaderError::LegacyTokenPairMissing,
            ));
        }
    };
    Ok((stage, token))
}

/// Capture history, the active open session, and the opaque legacy OAuth pair
/// from one repeatable-read source snapshot. The session is retained only as
/// typed projection data for atomic Hub lifecycle publication.
pub async fn capture_history_to_stage_with_legacy_token_and_session(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
) -> Result<
    (
        SourceDbStage,
        SourceDbOpenSession,
        LegacyOwnerTokenCiphertexts,
    ),
    SourceDbReaderError,
> {
    let (stage, token, session) = capture_history_to_stage_internal(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
        true,
    )
    .await?;
    let token = match token {
        Some(token) => token,
        None => {
            return Err(discard_stage_after_error(
                stage,
                SourceDbReaderError::LegacyTokenPairMissing,
            ));
        }
    };
    Ok((stage, session, token))
}

fn discard_stage_after_error(
    stage: SourceDbStage,
    primary: SourceDbReaderError,
) -> SourceDbReaderError {
    match stage.discard() {
        Ok(()) => primary,
        Err(error) => SourceDbReaderError::StageCleanupFailure {
            primary: Box::new(primary),
            cleanup: SourceDbStageCleanupFailureKind::from(&error),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDbStageCleanupFailureKind {
    MissingOrChanged,
    UnsafePath,
    RemoveFailed,
    Other,
}

impl From<&SourceDbStageError> for SourceDbStageCleanupFailureKind {
    fn from(error: &SourceDbStageError) -> Self {
        match error {
            SourceDbStageError::InspectPath { .. }
            | SourceDbStageError::StagePathIdentityChanged(_)
            | SourceDbStageError::DirectoryIdentityChanged(_) => Self::MissingOrChanged,
            SourceDbStageError::UnexpectedLinkCount { actual: 0, .. } => Self::MissingOrChanged,
            SourceDbStageError::SymlinkPath(_)
            | SourceDbStageError::ExpectedDirectory(_)
            | SourceDbStageError::ExpectedFile(_)
            | SourceDbStageError::UnexpectedLinkCount { .. }
            | SourceDbStageError::UnexpectedOwner { .. }
            | SourceDbStageError::InsecurePermissions { .. }
            | SourceDbStageError::InvalidStagePath => Self::UnsafePath,
            SourceDbStageError::RemoveStage { .. } => Self::RemoveFailed,
            _ => Self::Other,
        }
    }
}

impl std::fmt::Display for SourceDbStageCleanupFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MissingOrChanged => "missing-or-changed",
            Self::UnsafePath => "unsafe-path",
            Self::RemoveFailed => "remove-failed",
            Self::Other => "other",
        })
    }
}

async fn capture_history_to_stage_internal(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
    include_legacy_token: bool,
) -> Result<
    (
        SourceDbStage,
        Option<LegacyOwnerTokenCiphertexts>,
        SourceDbOpenSession,
    ),
    SourceDbReaderError,
> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(SourceDbReaderError::InvalidSelectedCarId);
    }
    if limits.parallel_copy_lanes > 1 {
        return capture_history_to_stage_parallel_with_legacy_token(
            source,
            password,
            selected_car_id,
            limits,
            imports_dir,
            include_legacy_token,
        )
        .await;
    }
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let mut stage = SourceDbStage::create(
        imports_dir,
        SourceDbStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;

    let (client, connection_task) = match connect_source(source, password, limits).await {
        Ok(connection) => connection,
        Err(error) => {
            return Err(discard_stage_after_error(stage, error));
        }
    };
    let session = SourceDbSnapshotSession::new(client, connection_task);

    let capture = capture_history_in_session(
        session.client(),
        source,
        selected_car_id,
        limits,
        &mut stage,
    )
    .await;
    let open_session = if capture.is_ok() {
        Some(read_open_session_in_client(session.client(), selected_car_id, limits).await)
    } else {
        None
    };
    let token = if include_legacy_token && open_session.as_ref().is_some_and(Result::is_ok) {
        Some(read_legacy_token_ciphertexts_in_client(session.client()).await)
    } else {
        None
    };
    let rollback = session.finish().await;
    if let Err(error) = capture {
        return Err(discard_stage_after_error(stage, error));
    }
    let open_session = match open_session {
        Some(Ok(session)) => session,
        Some(Err(error)) => {
            return Err(discard_stage_after_error(stage, error));
        }
        None => unreachable!("open session capture follows successful history capture"),
    };
    let token = match token {
        Some(Ok(token)) => Some(token),
        Some(Err(error)) => {
            return Err(discard_stage_after_error(stage, error));
        }
        None => None,
    };
    if let Err(error) = rollback {
        return Err(discard_stage_after_error(stage, error));
    }
    if let Err(error) = stage.seal() {
        return Err(discard_stage_after_error(
            stage,
            SourceDbReaderError::Stage(error),
        ));
    }
    Ok((stage, token, open_session))
}

/// Capture the nine selected-car projections through bounded PostgreSQL
/// lanes. One exported repeatable-read snapshot is held by `owner`; every
/// lane imports that snapshot on its own connection. The channel is bounded
/// to two pages per lane, and only this coordinator owns the SQLite stage.
async fn capture_history_to_stage_parallel_with_legacy_token(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    imports_dir: &Path,
    include_legacy_token: bool,
) -> Result<
    (
        SourceDbStage,
        Option<LegacyOwnerTokenCiphertexts>,
        SourceDbOpenSession,
    ),
    SourceDbReaderError,
> {
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let mut stage = SourceDbStage::create(
        imports_dir,
        SourceDbStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;
    let (owner, _, _) =
        match open_exported_snapshot_lease(source, password, i64::from(selected_car_id), limits)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return Err(discard_stage_after_error(stage, error));
            }
        };

    let lane_count = limits
        .parallel_copy_lanes
        .min(SourceDbStageTable::ALL.len());
    let position_max_id = match source_max_id(
        owner.session.client(),
        SourceDbStageTable::Positions,
        selected_car_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = owner.finish().await;
            return Err(discard_stage_after_error(stage, error));
        }
    };
    let charge_max_id = match source_max_id(
        owner.session.client(),
        SourceDbStageTable::Charges,
        selected_car_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = owner.finish().await;
            return Err(discard_stage_after_error(stage, error));
        }
    };
    let lane_jobs = distribute_capture_jobs(lane_count, position_max_id, charge_max_id);
    let (sender, mut receiver) = mpsc::channel(lane_count.saturating_mul(2).max(1));
    let mut lanes = JoinSet::new();
    for jobs in lane_jobs {
        let sender = sender.clone();
        let source = source.clone();
        let password = password.clone();
        let snapshot_id = owner.snapshot_id().to_owned();
        lanes.spawn(async move {
            capture_snapshot_lane(
                &source,
                &password,
                &snapshot_id,
                selected_car_id,
                limits,
                jobs,
                sender,
            )
            .await
        });
    }
    drop(sender);

    let capture = coordinate_parallel_capture(&mut stage, &mut receiver, &mut lanes).await;
    let open_session = if capture.is_ok() {
        Some(read_open_session_in_client(owner.session.client(), selected_car_id, limits).await)
    } else {
        None
    };
    let token = if include_legacy_token && open_session.as_ref().is_some_and(Result::is_ok) {
        Some(read_legacy_token_ciphertexts_in_client(owner.session.client()).await)
    } else {
        None
    };
    let owner_result = owner.finish().await;
    let selected_car_seen = match capture {
        Ok(selected_car_seen) => selected_car_seen,
        Err(error) => {
            return Err(discard_stage_after_error(stage, error));
        }
    };
    let open_session = match open_session {
        Some(Ok(session)) => session,
        Some(Err(error)) => {
            return Err(discard_stage_after_error(stage, error));
        }
        None => unreachable!("open session capture follows successful history capture"),
    };
    if let Err(error) = owner_result {
        return Err(discard_stage_after_error(stage, error));
    }
    let token = match token {
        Some(Ok(token)) => Some(token),
        Some(Err(error)) => {
            return Err(discard_stage_after_error(stage, error));
        }
        None => None,
    };
    if !selected_car_seen {
        return Err(discard_stage_after_error(
            stage,
            SourceDbReaderError::SelectedCarMissing {
                selected_car_id: i64::from(selected_car_id),
            },
        ));
    }
    if let Err(error) = stage.seal() {
        return Err(discard_stage_after_error(
            stage,
            SourceDbReaderError::Stage(error),
        ));
    }
    Ok((stage, token, open_session))
}

async fn coordinate_parallel_capture(
    stage: &mut SourceDbStage,
    receiver: &mut mpsc::Receiver<RawStagePage>,
    lanes: &mut JoinSet<Result<(), SourceDbReaderError>>,
) -> Result<bool, SourceDbReaderError> {
    let mut receiver_closed = false;
    let mut selected_car_seen = false;
    let mut primary = None;

    while !receiver_closed || !lanes.is_empty() {
        tokio::select! {
            biased;
            lane = lanes.join_next(), if !lanes.is_empty() => {
                match lane {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => {
                        primary = Some(error);
                        break;
                    }
                    Some(Err(error)) => {
                        primary = Some(parallel_lane_join_error(error));
                        break;
                    }
                    None => {}
                }
            }
            page = receiver.recv(), if !receiver_closed => {
                match page {
                    Some(page) => {
                        if page.table == SourceDbStageTable::Cars && !page.rows.is_empty() {
                            selected_car_seen = true;
                        }
                        if let Err(error) = stage.insert_encoded_json_page(page.table, page.rows) {
                            primary = Some(SourceDbReaderError::Stage(error));
                            break;
                        }
                    }
                    None => receiver_closed = true,
                }
            }
        }
    }

    if let Some(primary) = primary {
        receiver.close();
        lanes.abort_all();
        while receiver.try_recv().is_ok() {}
        while lanes.join_next().await.is_some() {}
        return Err(primary);
    }

    while let Some(result) = lanes.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) => return Err(parallel_lane_join_error(error)),
        }
    }
    Ok(selected_car_seen)
}

fn parallel_lane_join_error(error: tokio::task::JoinError) -> SourceDbReaderError {
    if error.is_panic() {
        SourceDbReaderError::ParallelLanePanicked
    } else {
        SourceDbReaderError::ParallelLaneCancelled
    }
}

#[cfg(test)]
mod n1_src_import_source_db_reader_capture_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free};

    impl ErrorExample for SourceDbStageCleanupFailureKind {
        fn error_example() -> Self {
            SourceDbStageCleanupFailureKind::MissingOrChanged
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_stage_cleanup_failure_kind(error: SourceDbStageCleanupFailureKind) {
            match &error {
                SourceDbStageCleanupFailureKind::MissingOrChanged => (),
                SourceDbStageCleanupFailureKind::UnsafePath => (),
                SourceDbStageCleanupFailureKind::RemoveFailed => (),
                SourceDbStageCleanupFailureKind::Other => (),
            }
            assert_name_free(&error);
        }
        check_source_db_stage_cleanup_failure_kind(
            SourceDbStageCleanupFailureKind::MissingOrChanged,
        );
        check_source_db_stage_cleanup_failure_kind(SourceDbStageCleanupFailureKind::UnsafePath);
        check_source_db_stage_cleanup_failure_kind(SourceDbStageCleanupFailureKind::RemoveFailed);
        check_source_db_stage_cleanup_failure_kind(SourceDbStageCleanupFailureKind::Other);
    }
}
