// SPDX-License-Identifier: AGPL-3.0-only

fn invalid(message: impl Into<String>) -> ProjectionPackError {
    ProjectionPackError::Invalid(message.into())
}

fn require_positive(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value <= 0 {
        return Err(invalid(format!("{field} must be positive")));
    }
    Ok(())
}

fn require_unique_positive(
    ids: &mut HashSet<i64>,
    value: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    require_positive(value, field)?;
    if !ids.insert(value) {
        return Err(invalid(format!("duplicate {field}")));
    }
    Ok(())
}

fn require_unique_signed_i32(
    ids: &mut HashSet<i64>,
    value: i32,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !ids.insert(i64::from(value)) {
        return Err(invalid(format!("duplicate {field}")));
    }
    Ok(())
}

fn require_same_car(value: i64, expected: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value != expected {
        return Err(invalid(format!("{field} does not match selected_car_id")));
    }
    Ok(())
}

fn validate_interval(start: i64, end: i64, field: &str) -> Result<(), ProjectionPackError> {
    require_positive(start, &format!("{field}.start_date_ms"))?;
    require_positive(end, &format!("{field}.end_date_ms"))?;
    if end < start {
        return Err(invalid(format!(
            "{field}.end_date_ms precedes start_date_ms"
        )));
    }
    Ok(())
}

fn validate_timestamp_0_pg_us(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    let is_infinity = matches!(value, i64::MIN | i64::MAX);
    let is_finite_second = (POSTGRES_TIMESTAMP_FINITE_MIN_US
        ..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
        .contains(&value)
        && value.rem_euclid(1_000_000) == 0;
    if !is_infinity && !is_finite_second {
        return Err(invalid(format!(
            "{field} is outside the PostgreSQL timestamp(0) source domain"
        )));
    }
    Ok(())
}

fn validate_optional_positive(value: Option<i64>, field: &str) -> Result<(), ProjectionPackError> {
    if let Some(value) = value {
        require_positive(value, field)?;
    }
    Ok(())
}

fn validate_bounded_i64(
    value: i64,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !(minimum..=maximum).contains(&value) {
        return Err(invalid(format!(
            "{field} is outside its pinned source range"
        )));
    }
    Ok(())
}

fn validate_fixed_numeric_v2_2(
    value: ProjectionFixedNumericV2_2,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if let ProjectionFixedNumericV2_2::Finite(value) = value {
        validate_bounded_i64(value, minimum, maximum, field)?;
    }
    Ok(())
}

fn validate_optional_fixed_numeric_v2_2(
    value: Option<ProjectionFixedNumericV2_2>,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if let Some(value) = value {
        validate_fixed_numeric_v2_2(value, minimum, maximum, field)?;
    }
    Ok(())
}

fn validate_optional_nonnegative(
    value: Option<f64>,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(invalid(format!("{field} must be finite and nonnegative")));
    }
    Ok(())
}

fn validate_optional_finite(value: Option<f64>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(invalid(format!("{field} must be finite")));
    }
    Ok(())
}

fn validate_optional_soc(value: Option<i64>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !(0..=100).contains(&value)) {
        return Err(invalid(format!("{field} must be between 0 and 100")));
    }
    Ok(())
}

fn validate_coordinate_pair(
    latitude: Option<f64>,
    longitude: Option<f64>,
    field: &str,
) -> Result<(), ProjectionPackError> {
    match (latitude, longitude) {
        (None, None) => Ok(()),
        (Some(latitude), Some(longitude)) => validate_coordinate(latitude, longitude, field),
        _ => Err(invalid(format!("{field} coordinate pair is incomplete"))),
    }
}

fn validate_coordinate(
    latitude: f64,
    longitude: f64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || (latitude == 0.0 && longitude == 0.0)
    {
        return Err(invalid(format!("{field} coordinates are invalid")));
    }
    Ok(())
}

fn validate_required_text(value: &str, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    validate_optional_text(Some(value), field)
}

fn validate_required_text_with_source_width(
    value: &str,
    maximum_characters: usize,
    field: &str,
) -> Result<(), ProjectionPackError> {
    // PostgreSQL `varchar(n) NOT NULL` accepts the empty string. The Rust
    // field itself represents the non-null part of the source contract.
    validate_optional_text(Some(value), field)?;
    if value.chars().count() > maximum_characters {
        return Err(invalid(format!("{field} exceeds its pinned source width")));
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| value.len() > MAX_TEXT_BYTES || value.as_bytes().contains(&0)) {
        return Err(invalid(format!("{field} is unsafe or too large")));
    }
    Ok(())
}

fn validate_optional_text_with_source_width(
    value: Option<&str>,
    maximum_characters: usize,
    field: &str,
) -> Result<(), ProjectionPackError> {
    validate_optional_text(value, field)?;
    if value.is_some_and(|value| value.chars().count() > maximum_characters) {
        return Err(invalid(format!("{field} exceeds its pinned source width")));
    }
    Ok(())
}

fn ensure_private_staging_directory(path: &Path) -> Result<(), ProjectionPackError> {
    fs::create_dir_all(path).map_err(|source| ProjectionPackError::CreateDirectory {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| ProjectionPackError::InspectStaging {
            path: path.to_path_buf(),
            source,
        })?;
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || mode & 0o022 != 0
    {
        return Err(ProjectionPackError::UnsafeStaging(path.to_path_buf()));
    }
    if mode != PRIVATE_STAGING_DIRECTORY_MODE {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(PRIVATE_STAGING_DIRECTORY_MODE),
        )
        .map_err(|source| ProjectionPackError::ProtectStaging {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn is_owned_staging_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    [
        ".projection.sqlite.tmp",
        ".projection.zst.tmp",
        ".projection-2-2.sqlite.tmp",
        ".projection-2-2.zst.tmp",
        ".projection-delta.sqlite.tmp",
        ".projection-delta.zst.tmp",
    ]
    .iter()
    .any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|uuid| uuid.len() == 36 && Uuid::parse_str(uuid).is_ok())
    })
}

/// Remove only exact, Hub-owned projection temporary files. The caller must
/// hold the publication gate so no active writer can still own these names.
pub(crate) fn cleanup_stale_pack_staging(
    packs_dir: &Path,
) -> Result<(usize, u64), ProjectionPackError> {
    let staging = packs_dir.join(".staging");
    match fs::symlink_metadata(&staging) {
        Ok(_) => ensure_private_staging_directory(&staging)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(source) => {
            return Err(ProjectionPackError::InspectStaging {
                path: staging,
                source,
            });
        }
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&staging).map_err(|source| ProjectionPackError::InspectStaging {
        path: staging.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| ProjectionPackError::InspectStaging {
            path: staging.clone(),
            source,
        })?;
        if !is_owned_staging_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| ProjectionPackError::InspectStaging {
                path: path.clone(),
                source,
            })?;
        let mode = metadata.permissions().mode() & 0o777;
        if !metadata.file_type().is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || !matches!(mode, 0o600 | SHARED_IMMUTABLE_PACK_MODE)
            || !(1..=2).contains(&metadata.nlink())
        {
            return Err(ProjectionPackError::UnsafeStaging(path));
        }
        candidates.push((path, metadata.len(), metadata.nlink()));
    }

    let mut removed = 0_usize;
    let mut freed_bytes = 0_u64;
    for (path, bytes, links) in candidates {
        match fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                if links == 1 {
                    freed_bytes = freed_bytes.saturating_add(bytes);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(ProjectionPackError::CleanupStaging { path, source }),
        }
    }
    if removed != 0 {
        File::open(&staging)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| ProjectionPackError::CleanupStaging {
                path: staging,
                source,
            })?;
    }
    Ok((removed, freed_bytes))
}
