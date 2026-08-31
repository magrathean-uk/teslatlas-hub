// SPDX-License-Identifier: AGPL-3.0-only

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = cli.config.unwrap_or_else(default_config_path);

    #[cfg(unix)]
    if let Command::TeslaMateCheck {
        source,
        car_id,
        postgres_password_file,
        acknowledge_v4_2_compatible_schema,
    } = &cli.command
    {
        return run_teslamate_check(
            source,
            *car_id,
            postgres_password_file,
            *acknowledge_v4_2_compatible_schema,
        )
        .await;
    }

    #[cfg(unix)]
    if matches!(
        &cli.command,
        Command::Migrate {
            acknowledge_v4_2_compatible_schema: false,
            ..
        }
    ) {
        return Err("TeslaMate migration requires --acknowledge-v4-2-compatible-schema after confirming TeslaMate 4.2.0 or newer".into());
    }

    #[cfg(unix)]
    if let Command::WriteBack {
        source,
        car_id,
        postgres_password_file,
        command,
    } = &cli.command
    {
        let source = ReadOnlySource::parse(source)?;
        let password = TeslaMatePostgresPassword::from_bytes(&read_migration_secret(
            postgres_password_file,
            MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES,
        )?)?;
        match command {
            WriteBackCommand::ChargeCost {
                charging_process_id,
                cost,
                apply,
            } => {
                let receipt = write_back_charge_cost(
                    &source,
                    &password,
                    *car_id,
                    *charging_process_id,
                    *cost,
                    *apply,
                )
                .await?;
                println!("{}", serde_json::to_string(&receipt)?);
            }
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    if let Command::Service { command } = &cli.command {
        match command {
            ServiceCommand::Status => {
                let loaded = teslatlas_hub::macos_launch_agent::service_is_loaded()?;
                println!(
                    "{}",
                    serde_json::json!({
                        "status": if loaded { "running" } else { "stopped" },
                        "loaded": loaded,
                    })
                );
            }
            ServiceCommand::Start => {
                let config = HubConfig::load(&config_path)?;
                teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                    &config.data_dir,
                    config.collector.provider,
                )?;
                teslatlas_hub::macos_launch_agent::start_installed(&config.data_dir)?;
                println!("{}", serde_json::json!({"status": "running"}));
            }
            ServiceCommand::Stop => {
                teslatlas_hub::macos_launch_agent::stop_installed()?;
                println!("{}", serde_json::json!({"status": "stopped"}));
            }
            ServiceCommand::Restart => {
                let config = HubConfig::load(&config_path)?;
                teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                    &config.data_dir,
                    config.collector.provider,
                )?;
                teslatlas_hub::macos_launch_agent::restart_installed(&config.data_dir)?;
                println!("{}", serde_json::json!({"status": "running"}));
            }
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    if let Command::Service { command } = &cli.command {
        let status = match command {
            ServiceCommand::Status => teslatlas_hub::linux_systemd::status()?,
            ServiceCommand::Start => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Start,
            )?,
            ServiceCommand::Stop => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Stop,
            )?,
            ServiceCommand::Restart => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Restart,
            )?,
        };
        println!(
            "{}",
            serde_json::json!({
                "status": status.status(),
                "unit": status.unit,
                "loadState": status.load_state,
                "activeState": status.active_state,
                "subState": status.sub_state,
            })
        );
        return Ok(());
    }

    if let Command::Control {
        vehicle_id,
        command,
    } = &cli.command
    {
        return run_control(&config_path, *vehicle_id, command).await;
    }

    #[cfg(not(unix))]
    if matches!(&cli.command, Command::Serve) {
        return Err("serve requires a Unix platform".into());
    }

    #[cfg(target_os = "macos")]
    if matches!(&cli.command, Command::Install) {
        let config = HubConfig::load(&config_path)?;
        let admission = AdmittedUserHub::admit(&config.data_dir)?;
        teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
            &config.data_dir,
            config.collector.provider,
        )?;
        let installed =
            teslatlas_hub::macos_launch_agent::prepare_install(&config.data_dir, &config_path)?;
        drop(admission);
        teslatlas_hub::macos_launch_agent::start_prepared(&installed)?;
        println!("installed {}; launch requested", installed.binary.display());
        return Ok(());
    }

    // Long-lived service, import, credential, and recovery commands take the
    // local instance lock. `control` uses short SQLite transactions so it can
    // intentionally operate while the collector is running.
    #[cfg(unix)]
    let admitted_user_hub = if command_requires_user_hub_admission(&cli.command) {
        let config = HubConfig::load(&config_path)?;
        Some(AdmittedUserHub::admit(&config.data_dir)?)
    } else {
        None
    };

    #[cfg(unix)]
    if let Command::Migrate {
        source,
        car_id,
        postgres_password_file,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        online_snapshot,
        acknowledge_v4_2_compatible_schema: _,
    } = &cli.command
    {
        let start_hub = run_macos_migration(
            admitted_user_hub
                .as_ref()
                .ok_or("migration reached runtime without user admission")?,
            MacMigrationInput {
                config_path: &config_path,
                source_url: source,
                car_id: *car_id,
                postgres_password_file,
                encryption_key_file: encryption_key_file.as_deref(),
                access_token_file: access_token_file.as_deref(),
                refresh_token_file: refresh_token_file.as_deref(),
                online_snapshot: *online_snapshot,
            },
        )
        .await?;
        drop(admitted_user_hub);
        #[cfg(target_os = "macos")]
        if start_hub {
            let config = HubConfig::load(&config_path)?;
            teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                &config.data_dir,
                config.collector.provider,
            )?;
            let installed =
                teslatlas_hub::macos_launch_agent::prepare_install(&config.data_dir, &config_path)?;
            teslatlas_hub::macos_launch_agent::start_prepared(&installed)?;
            println!("installed {}; launch requested", installed.binary.display());
        }
        #[cfg(target_os = "linux")]
        if start_hub {
            println!(
                "{}",
                serde_json::json!({
                    "serviceStartRequested": true,
                    "next": "sudo systemctl start teslatlas-hub.service",
                })
            );
        }
        return Ok(());
    }

    match &cli.command {
        Command::VerifyBackup { source } => {
            let report = verify_data_backup(source)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::RestoreData {
            source,
            destination,
        } => {
            let report = restore_data_backup(source, destination)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        _ => {}
    }

    match &cli.command {
        Command::Legal => {
            println!("{}", teslatlas_hub::legal_notice());
            return Ok(());
        }
        Command::Source => {
            println!("{}", teslatlas_hub::corresponding_source_url());
            return Ok(());
        }
        Command::Doctor => {
            let config = HubConfig::load(&config_path)?;
            let report = run_immutable_diagnostic(&config.data_dir, |store| {
                Ok(inspect_hub(store, &config)?)
            })?;
            report.log();
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_ok() {
                return Err(std::io::Error::other("doctor found failures; see JSON report").into());
            }
            return Ok(());
        }
        Command::Status => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let vehicles = store.published_vehicles()?;
            let configured = store.configured_tesla_vehicles()?;
            let mut vehicle_summaries = Vec::with_capacity(vehicles.len());
            for vehicle in &vehicles {
                let binding = store.v2_projection_binding(vehicle.vehicle_id)?;
                let latest =
                    store.latest_current_observation_metadata_for_vehicle(vehicle.vehicle_id)?;
                let tesla_eid = configured.iter().find_map(|(vehicle_id, eid, _)| {
                    (*vehicle_id == vehicle.vehicle_id).then_some(*eid)
                });
                vehicle_summaries.push(serde_json::json!({
                    "vehicleId": vehicle.vehicle_id,
                    "displayName": vehicle.display_name,
                    "sourceCarId": binding.selected_car_id,
                    "teslaEid": tesla_eid,
                    "latestObservationId": latest.as_ref().map_or(0, |observation| observation.observation_id),
                    "latestObservedAtMs": latest.as_ref().map(|observation| observation.observed_at_ms),
                    "latestReceivedAtMs": latest.as_ref().map(|observation| observation.received_at_ms),
                }));
            }
            let vehicle = (vehicle_summaries.len() == 1).then(|| vehicle_summaries[0].clone());
            let legacy_credentials = store.load_teslamate_legacy_tokens()?;
            let fleet_credentials = store.load_fleet_tokens()?;
            let (fleet_scope_summary, fleet_scope_status) = if fleet_credentials.is_some() {
                match stored_fleet_scope_summary(&store, &config.data_dir) {
                    Ok(summary) => (summary, Some("ready")),
                    Err(error) => {
                        let status = match error {
                            FleetCredentialError::MissingCollectionScopes => {
                                "missing_collection_scopes"
                            }
                            FleetCredentialError::InvalidAccessTokenClaims => {
                                "invalid_access_token_claims"
                            }
                            FleetCredentialError::MigrationRequired => "migration_required",
                            _ => "unavailable",
                        };
                        (None, Some(status))
                    }
                }
            } else {
                (None, None)
            };
            let selected_credentials_present = match config.collector.provider {
                CollectorProvider::Legacy => legacy_credentials.is_some(),
                CollectorProvider::Fleet => fleet_credentials.is_some(),
            };
            let readiness = store
                .service_readiness_at(config.collector.interval_seconds > 0, current_epoch_ms()?);
            let database_bytes = fs::metadata(store.database_path())?.len();
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "database": {
                        "path": store.database_path(),
                        "bytes": database_bytes,
                    },
                    "ready": readiness.is_ok(),
                    "readinessReason": readiness.err().map(|failure| failure.code),
                    "provider": config.collector.provider,
                    "vehicle": vehicle,
                    "vehicles": vehicle_summaries,
                    "credentials": {
                        "present": selected_credentials_present,
                    },
                    "legacyCredentials": {
                        "present": legacy_credentials.is_some(),
                        "expiresAt": legacy_credentials.as_ref().map(TeslaMateLegacyTokenStore::expires_at),
                        "nextRefreshAt": legacy_credentials
                            .as_ref()
                            .map(TeslaMateLegacyTokenStore::next_refresh_at),
                    },
                    "fleetCredentials": {
                        "present": fleet_credentials.is_some(),
                        "expiresAt": fleet_credentials.as_ref().map(|credentials| credentials.expires_at()),
                        "nextRefreshAt": fleet_credentials.as_ref().map(|credentials| credentials.next_refresh_at()),
                        "scopes": fleet_scope_summary,
                        "scopeStatus": fleet_scope_status,
                    },
                    "fleetTelemetry": {
                        "enabled": config.collector.fleet_telemetry.is_some(),
                        "configured": config.collector.fleet_telemetry.is_some(),
                        "mode": if config.collector.fleet_telemetry.is_some() {
                            "native_push_configured"
                        } else {
                            "disabled"
                        },
                        "operationalState": if config.collector.fleet_telemetry.is_some() {
                            "requires_receiver_and_vehicle_receipt_proof"
                        } else {
                            "disabled"
                        },
                        "paidVehicleDataPolling": config.collector.provider == CollectorProvider::Fleet
                            && config.collector.fleet_telemetry.is_none(),
                        "deliveryPolicy": config.collector.fleet_telemetry.as_ref().map(|_| "latest"),
                    },
                })
            );
            return Ok(());
        }
        #[cfg(unix)]
        Command::Preflight => {
            let config = HubConfig::load(&config_path)?;
            run_immutable_diagnostic(&config.data_dir, |store| {
                store.catalogue_check()?;
                let configured = store.configured_tesla_vehicles()?;
                if configured.is_empty() {
                    return Err("at least one configured vehicle is required".into());
                }
                match config.collector.provider {
                    CollectorProvider::Legacy => {
                        let tokens = store
                            .load_teslamate_legacy_tokens()?
                            .ok_or("legacy Owner API credentials are required")?;
                        teslatlas_hub::teslamate_credentials::load_key_for_tokens(
                            &config.data_dir,
                            &tokens,
                        )?;
                    }
                    CollectorProvider::Fleet => {
                        validate_stored_fleet_credentials(store, &config.data_dir)?;
                    }
                }
                Ok(())
            })?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "ready",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "provider": config.collector.provider,
                })
            );
            return Ok(());
        }
        Command::ObservationWatermark { car_id } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let watermark = match store.observation_watermark(*car_id) {
                Ok(watermark) => watermark,
                Err(error) => {
                    return Err(observation_command_error(
                        "observation-watermark",
                        *car_id,
                        error,
                    ));
                }
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "captured",
                    "command": "observation-watermark",
                    "carId": watermark.source_car_id,
                    "sourceId": watermark.source_id,
                    "vehicleId": watermark.vehicle_id,
                    "watermark": watermark.observation_id,
                    "observedAtMs": watermark.observed_at_ms,
                    "receivedAtMs": watermark.received_at_ms,
                })
            );
            return Ok(());
        }
        Command::VerifyObservation { car_id, watermark } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let verification = match store.verify_observation_after(*car_id, *watermark) {
                Ok(verification) => verification,
                Err(error) => {
                    return Err(observation_command_error(
                        "verify-observation",
                        *car_id,
                        error,
                    ));
                }
            };
            let verified = verification.verified();
            println!(
                "{}",
                serde_json::json!({
                    "status": if verified { "verified" } else { "not_verified" },
                    "command": "verify-observation",
                    "verified": verified,
                    "carId": verification.source_car_id,
                    "sourceId": verification.source_id,
                    "vehicleId": verification.vehicle_id,
                    "afterWatermark": verification.after_observation_id,
                    "latestObservationId": verification.latest_observation_id,
                    "latestObservedAtMs": verification.latest_observed_at_ms,
                    "latestReceivedAtMs": verification.latest_received_at_ms,
                })
            );
            if !verified {
                return Err("no strictly newer durable observation".into());
            }
            return Ok(());
        }
        _ => {}
    }

    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_sensitive_access()?;
    }
    let (config, config_sha256) = HubConfig::load_with_digest(&config_path)?;
    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_store_path(&config.data_dir)?;
    }
    let store = HubStore::initialize(&config.data_dir)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    match cli.command {
        Command::Init => {
            println!("initialized {}", store.database_path().display());
        }
        #[cfg(unix)]
        Command::Bootstrap => {
            migrate_legacy_fleet_credentials(&store, &config.data_dir)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "bootstrapped",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "database": store.database_path(),
                })
            );
        }
        #[cfg(unix)]
        Command::Setup {
            access_token_file,
            refresh_token_file,
            tokens_stdin,
            vehicle_id,
            all_vehicles,
        } => {
            validate_legacy_setup_provider(config.collector.provider)?;
            let tokens = if tokens_stdin {
                read_setup_tokens_from_stdin()?
            } else {
                OwnerTokens::from_file_bytes(
                    read_migration_secret(
                        access_token_file
                            .as_deref()
                            .ok_or("setup access-token file is missing")?,
                        MAX_MIGRATION_TOKEN_FILE_BYTES,
                    )?,
                    read_migration_secret(
                        refresh_token_file
                            .as_deref()
                            .ok_or("setup refresh-token file is missing")?,
                        MAX_MIGRATION_TOKEN_FILE_BYTES,
                    )?,
                )?
            };
            let report = if all_vehicles {
                let report = collector::setup_native_vehicles(&store, &config, &tokens).await?;
                serde_json::json!({
                    "status": "configured",
                    "vehicles": report.vehicles,
                    "snapshotsPublished": report.snapshots_published,
                })
            } else {
                let report =
                    collector::setup_native_vehicle(&store, &config, &tokens, vehicle_id).await?;
                serde_json::json!({
                    "status": "configured",
                    "selectedVehicleId": report.selected_vehicle_id,
                    "displayName": report.display_name,
                    "snapshotsPublished": report.snapshots_published,
                })
            };
            persist_legacy_setup_and_drop_fleet(&config.data_dir, &store, &tokens)?;
            catalogue_checkpoint.finish().map_err(|error| {
                provider_switch_outcome_ambiguous("checkpointing Legacy setup", error)
            })?;
            println!("{report}");
            return Ok(());
        }
        #[cfg(unix)]
        Command::SetupFleet {
            vehicle_id,
            all_vehicles,
        } => {
            if config.collector.provider != CollectorProvider::Fleet {
                return Err("setup-fleet requires collector.provider = \"fleet\"".into());
            }
            let credentials = read_setup_fleet_from_stdin()?;
            let admission = admitted_user_hub
                .as_deref()
                .ok_or("Fleet setup reached runtime without user admission")?;
            let report = if all_vehicles {
                let report =
                    collector::setup_fleet_vehicles(&store, &config, &credentials, admission)
                        .await?;
                serde_json::json!({
                    "status": "configured",
                    "provider": "fleet",
                    "vehicles": report.vehicles,
                    "snapshotsPublished": report.snapshots_published,
                })
            } else {
                let report = collector::setup_fleet_vehicle(
                    &store,
                    &config,
                    &credentials,
                    admission,
                    vehicle_id,
                )
                .await?;
                serde_json::json!({
                    "status": "configured",
                    "provider": "fleet",
                    "selectedVehicleId": report.selected_vehicle_id,
                    "displayName": report.display_name,
                    "snapshotsPublished": report.snapshots_published,
                })
            };
            persist_fleet_setup_and_drop_legacy(
                &config.data_dir,
                &store,
                &credentials,
                SystemTime::now(),
            )?;
            catalogue_checkpoint.finish().map_err(|error| {
                provider_switch_outcome_ambiguous("checkpointing Fleet setup", error)
            })?;
            println!("{report}");
            return Ok(());
        }
        #[cfg(unix)]
        Command::ConfigureFleetTelemetry => {
            if config.collector.provider != CollectorProvider::Fleet
                || config.collector.fleet_telemetry.is_none()
            {
                return Err(
                    "configure-fleet-telemetry requires collector.provider = \"fleet\" and collector.fleet_telemetry"
                        .into(),
                );
            }
            let admission = admitted_user_hub
                .as_ref()
                .cloned()
                .ok_or("Fleet Telemetry setup reached runtime without user admission")?;
            let report =
                collector::configure_fleet_telemetry_for_admitted_user(&store, &config, admission)
                    .await?;
            catalogue_checkpoint.finish()?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::Legal
        | Command::Source
        | Command::Doctor
        | Command::Status
        | Command::TeslaMateCheck { .. }
        | Command::Control { .. }
        | Command::ObservationWatermark { .. }
        | Command::VerifyObservation { .. } => {
            unreachable!("read-only commands return before opening writable Hub state")
        }
        #[cfg(unix)]
        Command::Service { .. } => {
            unreachable!("service control returns before opening writable Hub state")
        }
        #[cfg(unix)]
        Command::Preflight => {
            unreachable!("preflight returns before opening writable Hub state")
        }
        Command::Serve => {
            #[cfg(unix)]
            {
                #[cfg(target_os = "macos")]
                {
                    store.checkpoint_catalogue_for_immutable_read()?;
                    teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                        &config.data_dir,
                        config.collector.provider,
                    )?;
                }
                let admission =
                    admitted_user_hub.ok_or("Serve reached runtime without user admission")?;
                admission.assert_sensitive_access()?;
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm = signal(SignalKind::terminate())?;

                let collector_enabled = collector_can_start(&store, &config)?;
                log_runtime_inventory(&store, &config);
                tracing::info!(
                    collector_enabled,
                    provider = ?config.collector.provider,
                    interval_seconds = config.collector.interval_seconds,
                    bind = %config.bind,
                    "Hub serve starting (TeslaMate is not opened; stored tokens are not deleted)"
                );
                let collector_store = store.clone();
                let collector_config = config.clone();
                let collector_admission = std::sync::Arc::clone(&admission);
                let server_config = config;
                let server_admission = std::sync::Arc::clone(&admission);
                let control_admission = std::sync::Arc::clone(&admission);
                #[cfg(target_os = "macos")]
                let command_proxy = mac_command_proxy_spec(&server_config)?;
                #[cfg(not(target_os = "macos"))]
                let command_proxy = None;
                let serve_result = run_macos_serve_with_optional_proxy(
                    command_proxy,
                    collector_enabled,
                    move |ready, shutdown| async move {
                        collector::run_supervised_for_admitted_user(
                            &collector_store,
                            &collector_config,
                            collector_admission,
                            ready,
                            async move {
                                let _ = shutdown.await;
                            },
                        )
                        .await
                        .map_err(std::io::Error::other)
                    },
                    move |cursor_key, shutdown| async move {
                        server::serve_for_admitted_user(
                            store,
                            &server_config,
                            config_sha256,
                            server_admission,
                            cursor_key,
                            async move {
                                let _ = shutdown.await;
                            },
                        )
                        .await
                    },
                    async move {
                        tokio::select! {
                            error = control_admission.wait_until_invalid() => {
                                MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                            }
                            _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                            _ = sigterm.recv() => MacServeControl::Shutdown,
                        }
                    },
                )
                .await;
                match &serve_result {
                    Ok(()) => tracing::info!("Hub serve stopped cleanly"),
                    Err(error) => tracing::error!(%error, "Hub serve stopped unexpectedly"),
                }
                serve_result?;
            }
        }
        #[cfg(unix)]
        Command::Observe { duration_seconds } => {
            let admission =
                admitted_user_hub.ok_or("Observe reached runtime without user admission")?;
            admission.assert_sensitive_access()?;
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = signal(SignalKind::terminate())?;

            let collector_store = store.clone();
            let collector_config = config.clone();
            let collector_admission = std::sync::Arc::clone(&admission);
            let server_config = config;
            let server_admission = std::sync::Arc::clone(&admission);
            let control_admission = std::sync::Arc::clone(&admission);
            tracing::info!(
                duration_seconds,
                provider = ?collector_config.collector.provider,
                "Hub bounded observation starting"
            );
            let observe_result = run_macos_serve_supervisor(
                true,
                move |ready, shutdown| async move {
                    collector::run_observer_for_admitted_user(
                        &collector_store,
                        &collector_config,
                        collector_admission,
                        ready,
                        async move {
                            let _ = shutdown.await;
                        },
                    )
                    .await
                    .map_err(std::io::Error::other)
                },
                move |cursor_key, shutdown| async move {
                    server::serve_for_admitted_user(
                        store,
                        &server_config,
                        config_sha256,
                        server_admission,
                        cursor_key,
                        async move {
                            let _ = shutdown.await;
                        },
                    )
                    .await
                },
                async move {
                    tokio::select! {
                        error = control_admission.wait_until_invalid() => {
                            MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                        }
                        _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                        _ = sigterm.recv() => MacServeControl::Shutdown,
                        _ = tokio::time::sleep(Duration::from_secs(duration_seconds)) => MacServeControl::Shutdown,
                    }
                },
            )
            .await;
            match &observe_result {
                Ok(()) => tracing::info!("Hub bounded observation stopped cleanly"),
                Err(error) => {
                    tracing::error!(%error, "Hub bounded observation stopped unexpectedly")
                }
            }
            observe_result?;
        }
        #[cfg(target_os = "macos")]
        Command::Install => unreachable!("install returns before opening Hub state"),
        #[cfg(unix)]
        Command::Migrate { .. } => {
            unreachable!("migration returns before opening common Hub state")
        }
        #[cfg(unix)]
        Command::WriteBack { .. } => {
            unreachable!("write-back returns before opening common Hub state")
        }
        Command::Pair {
            label,
            expires_in_seconds,
            json,
        } => {
            let tls = config
                .tls
                .as_ref()
                .ok_or("device pairing requires configured TLS")?;
            let mut stdout = std::io::stdout().lock();
            let created_at_ms = current_epoch_ms()?;
            execute_pairing_at(
                &store,
                PairingCommandInput {
                    label: &label,
                    expires_in_seconds,
                    json,
                    public_url: &tls.public_url,
                    certificate_path: &tls.certificate_path,
                    private_key_path: &tls.private_key_path,
                    created_at_ms,
                },
                &mut stdout,
            )
            .await?;
        }
        Command::Repair => {
            let report = store.repair()?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Backup { destination } => {
            let report = create_data_backup(&store, &destination)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::ExportRecoveryCredentials {
            destination,
            recovery_key_file,
        } => {
            let recovery_key = read_recovery_encryption_key(&recovery_key_file)?;
            let report =
                export_credentials(&store, &config.data_dir, &destination, &recovery_key[..])?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::RestoreRecoveryCredentials {
            source,
            recovery_key_file,
        } => {
            let recovery_key = read_recovery_encryption_key(&recovery_key_file)?;
            let report = restore_credentials(&store, &config.data_dir, &source, &recovery_key[..])?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::VerifyBackup { .. } | Command::RestoreData { .. } => {
            unreachable!("immutable data-recovery commands return before writable Hub state")
        }
    }
    catalogue_checkpoint.finish()?;
    Ok(())
}
