//! Conservative, host-local import tuning.
//!
//! This first profile slice only lowers PostgreSQL COPY parallelism. It never
//! changes import safety limits or performs benchmark writes.

use std::{fmt, num::NonZeroUsize, path::Path};

use crate::config::PerformanceProfileConfig;

pub const MIN_PARALLEL_COPY_LANES: usize = 1;
pub const MAX_PARALLEL_COPY_LANES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceProfileError {
    InvalidConfiguredParallelCopyLanes(usize),
    InvalidMaximumParallelCopyLanes(usize),
}

impl fmt::Display for PerformanceProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguredParallelCopyLanes(value) => write!(
                formatter,
                "configured parallel COPY lanes must be within {MIN_PARALLEL_COPY_LANES}..={MAX_PARALLEL_COPY_LANES}, got {value}"
            ),
            Self::InvalidMaximumParallelCopyLanes(value) => write!(
                formatter,
                "performance-profile maximum parallel COPY lanes must be within {MIN_PARALLEL_COPY_LANES}..={MAX_PARALLEL_COPY_LANES}, got {value}"
            ),
        }
    }
}

impl std::error::Error for PerformanceProfileError {}

#[derive(Debug, Clone, Copy)]
pub struct HostCapabilities {
    pub available_parallelism: Option<NonZeroUsize>,
    pub filesystem_free_bytes: Option<u64>,
    pub filesystem_free_inodes: Option<u64>,
}

impl HostCapabilities {
    pub fn measure(data_dir: &Path) -> Self {
        let filesystem = rustix::fs::statvfs(data_dir).ok();
        Self {
            available_parallelism: std::thread::available_parallelism().ok(),
            filesystem_free_bytes: filesystem
                .as_ref()
                .map(|stats| stats.f_bavail.saturating_mul(stats.f_frsize)),
            filesystem_free_inodes: filesystem.as_ref().map(|stats| stats.f_favail),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileReason {
    Disabled,
    HostCpu,
    ExplicitMaximum,
    ExplicitMaximumAndHostCpu,
    CpuMeasurementUnavailable,
}

impl ProfileReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::HostCpu => "host_cpu",
            Self::ExplicitMaximum => "explicit_maximum",
            Self::ExplicitMaximumAndHostCpu => "explicit_maximum_and_host_cpu",
            Self::CpuMeasurementUnavailable => "cpu_measurement_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectiveImportProfile {
    pub version: u8,
    pub capabilities: HostCapabilities,
    pub parallel_copy_lanes: usize,
    pub reason: ProfileReason,
}

pub fn derive_effective_import_profile(
    configured_parallel_copy_lanes: usize,
    config: &PerformanceProfileConfig,
    data_dir: &Path,
) -> Result<EffectiveImportProfile, PerformanceProfileError> {
    derive_effective_import_profile_with_capabilities(
        configured_parallel_copy_lanes,
        config,
        HostCapabilities::measure(data_dir),
    )
}

pub fn derive_effective_import_profile_with_capabilities(
    configured_parallel_copy_lanes: usize,
    config: &PerformanceProfileConfig,
    capabilities: HostCapabilities,
) -> Result<EffectiveImportProfile, PerformanceProfileError> {
    if !(MIN_PARALLEL_COPY_LANES..=MAX_PARALLEL_COPY_LANES)
        .contains(&configured_parallel_copy_lanes)
    {
        return Err(PerformanceProfileError::InvalidConfiguredParallelCopyLanes(
            configured_parallel_copy_lanes,
        ));
    }
    if let Some(max_parallel_copy_lanes) = config.max_parallel_copy_lanes
        && !(MIN_PARALLEL_COPY_LANES..=MAX_PARALLEL_COPY_LANES).contains(&max_parallel_copy_lanes)
    {
        return Err(PerformanceProfileError::InvalidMaximumParallelCopyLanes(
            max_parallel_copy_lanes,
        ));
    }

    if !config.enabled {
        return Ok(EffectiveImportProfile {
            version: 1,
            capabilities,
            parallel_copy_lanes: configured_parallel_copy_lanes,
            reason: ProfileReason::Disabled,
        });
    }

    // `configured_parallel_copy_lanes` was validated by TeslaMateReadLimits,
    // so it is already within the reader's protocol hard maximum. Taking only
    // minima below cannot weaken that ceiling or raise a configured limit.
    let cpu_bound = capabilities
        .available_parallelism
        .map(NonZeroUsize::get)
        .unwrap_or(1);
    let parallel_copy_lanes = configured_parallel_copy_lanes
        .min(config.max_parallel_copy_lanes.unwrap_or(usize::MAX))
        .min(cpu_bound);
    let reason = match (
        config.max_parallel_copy_lanes.is_some(),
        capabilities.available_parallelism.is_some(),
    ) {
        (_, false) => ProfileReason::CpuMeasurementUnavailable,
        (true, true) => ProfileReason::ExplicitMaximumAndHostCpu,
        (false, true) => ProfileReason::HostCpu,
    };
    Ok(EffectiveImportProfile {
        version: 1,
        capabilities,
        parallel_copy_lanes,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;

    #[test]
    fn injected_capabilities_only_lower_configured_copy_lanes() {
        let profile = derive_effective_import_profile_with_capabilities(
            6,
            &PerformanceProfileConfig {
                enabled: true,
                max_parallel_copy_lanes: Some(4),
            },
            HostCapabilities {
                available_parallelism: NonZeroUsize::new(2),
                filesystem_free_bytes: None,
                filesystem_free_inodes: None,
            },
        )
        .expect("valid profile");
        assert_eq!(profile.parallel_copy_lanes, 2);
        assert_eq!(profile.reason, ProfileReason::ExplicitMaximumAndHostCpu);
    }

    #[test]
    fn disabled_profile_keeps_the_configured_lane_count() {
        let profile = derive_effective_import_profile_with_capabilities(
            3,
            &PerformanceProfileConfig {
                enabled: false,
                max_parallel_copy_lanes: Some(1),
            },
            HostCapabilities {
                available_parallelism: NonZeroUsize::new(1),
                filesystem_free_bytes: None,
                filesystem_free_inodes: None,
            },
        )
        .expect("valid profile");
        assert_eq!(profile.parallel_copy_lanes, 3);
        assert_eq!(profile.reason, ProfileReason::Disabled);
    }
}
