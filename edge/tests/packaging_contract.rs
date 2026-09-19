#![forbid(unsafe_code)]

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use tempfile::TempDir;

    fn guard() -> String {
        format!(
            "{}/scripts/run-with-spool-format-guard.sh",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn launch_guard_blocks_pre_v2_binary_from_v2_spool() {
        let temp = TempDir::new().unwrap();
        let spool = temp.path().join("spool");
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("FORMAT"), b"2\n").unwrap();
        let launched = temp.path().join("launched");
        let legacy = temp.path().join("legacy-edge");
        fs::write(
            &legacy,
            b"#!/bin/sh\nif [ \"$1\" = storage-format ]; then exit 64; fi\nprintf launched > \"$1\"\n",
        )
        .unwrap();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();

        let output = Command::new(guard())
            .arg(&legacy)
            .arg(&spool)
            .arg("--")
            .arg(&launched)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!launched.exists());

        fs::remove_file(spool.join("FORMAT")).unwrap();
        let output = Command::new(guard())
            .arg(&legacy)
            .arg(&spool)
            .arg("--")
            .arg(&launched)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(fs::read(launched).unwrap(), b"launched");
    }

    #[test]
    fn current_binary_reports_and_allows_guarded_v2_migration() {
        let temp = TempDir::new().unwrap();
        let spool = temp.path().join("spool");
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("FORMAT"), b"2\n").unwrap();

        let output = Command::new(guard())
            .arg(env!("CARGO_BIN_EXE_teslatlas-edge"))
            .arg(&spool)
            .arg("--")
            .arg("storage-format")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"3\n");
        assert!(output.stderr.is_empty());

        fs::write(spool.join("FORMAT"), b"3\n").unwrap();
        let legacy = temp.path().join("legacy-edge");
        fs::write(
            &legacy,
            b"#!/bin/sh\nif [ \"$1\" = storage-format ]; then printf '2\\n'; exit 0; fi\n",
        )
        .unwrap();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(guard())
            .arg(&legacy)
            .arg(&spool)
            .arg("--")
            .arg("serve")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }

    #[test]
    fn native_supervisors_allow_application_drain_and_final_sync() {
        let edge_unit = fs::read_to_string(format!(
            "{}/packaging/linux/teslatlas-edge.service",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let receiver_unit = fs::read_to_string(format!(
            "{}/packaging/linux/teslatlas-fleet-telemetry.service",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert!(edge_unit.contains("TimeoutStopSec=10"));
        assert!(
            !edge_unit.contains("Wants=network-online.target teslatlas-fleet-telemetry.service")
        );
        assert!(receiver_unit.contains("TimeoutStopSec=10"));

        for path in [
            "packaging/macos/uk.co.magrathean.teslatlas-edge.plist",
            "packaging/macos/uk.co.magrathean.teslatlas-fleet-telemetry.plist",
        ] {
            let plist =
                fs::read_to_string(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), path)).unwrap();
            assert!(plist.contains("<key>ExitTimeOut</key>\n  <integer>10</integer>"));
        }
    }

    #[test]
    fn docker_packaging_keeps_role_mounts_and_secret_paths_separate() {
        let root = env!("CARGO_MANIFEST_DIR");
        let dockerfile = fs::read_to_string(format!("{root}/Dockerfile")).unwrap();
        assert!(dockerfile.contains("GO_BINARY=/usr/local/go/bin/go"));
        assert!(dockerfile.contains("RUNNER_TOOL_CACHE=/usr/local/go"));
        assert!(dockerfile.contains("ARG TARGETARCH"));
        assert!(dockerfile.contains(
            "GO_SHA256_ARM64=51798d2c42d0e1c6ed7fd9f48728b4193abac9e8aad6dbac2fe96a81f5909bda"
        ));
        assert!(dockerfile.contains("go${GO_VERSION}.linux-${GO_ARCH}.tar.gz"));
        assert!(dockerfile.contains("--target \"linux-${TARGETARCH}\""));
        assert!(dockerfile.contains("mkdir -p /out"));
        assert!(!dockerfile.contains("--target linux-amd64"));
        assert!(dockerfile.contains("useradd --uid 10001 --gid 10001"));
        assert!(dockerfile.contains("run-with-spool-format-guard.sh"));

        let compose = fs::read_to_string(format!("{root}/compose.yaml")).unwrap();
        assert!(compose.contains("network_mode: service:edge"));
        assert!(compose.contains("${EDGE_HUB_PORT:-8443}:8443"));
        assert!(compose.contains("${EDGE_RECEIVER_PORT:-443}:8444"));
        assert!(compose.contains("entrypoint: [\"/usr/bin/fleet-telemetry\"]"));
        assert!(
            compose.contains("command: [\"-config=/etc/teslatlas-edge/fleet-telemetry.json\"]")
        );
        assert!(compose.contains("TESLATLAS_FLEET_TELEMETRY_BEARER_FILE"));
        assert!(compose.contains("condition: service_healthy"));
        assert!(compose.contains("/var/lib/teslatlas-edge"));
        assert!(compose.contains("/run/teslatlas-edge-vehicle-tls:ro"));

        let edge_config =
            fs::read_to_string(format!("{root}/packaging/docker/config.toml.example")).unwrap();
        assert!(
            edge_config
                .contains("receiver_bearer_path = \"/run/teslatlas-edge-receiver/receiver-token\"")
        );
        assert!(edge_config.contains(
            "hub_server_private_key_path = \"/run/teslatlas-edge-hub-tls/hub-server.key\""
        ));
        let receiver_config = fs::read_to_string(format!(
            "{root}/packaging/docker/fleet-telemetry.json.example"
        ))
        .unwrap();
        assert!(receiver_config.contains("\"port\": 8444"));
        assert!(receiver_config.contains("/run/teslatlas-edge-vehicle-tls/vehicle-tls.crt"));
        assert!(receiver_config.contains("/run/teslatlas-edge-vehicle-tls/vehicle-tls.key"));
        assert!(receiver_config.contains("/run/teslatlas-edge-vehicle-tls/vehicle-client-ca.crt"));

        let native_receiver_config =
            fs::read_to_string(format!("{root}/packaging/fleet-telemetry.json.example")).unwrap();
        assert!(
            native_receiver_config
                .contains("\"ca_file\": \"/etc/teslatlas-edge/vehicle-client-ca.crt\"")
        );

        let docker_docs = fs::read_to_string(format!("{root}/docs/operations/docker.md")).unwrap();
        assert!(
            docker_docs.contains(
                "The receiver's mounted TLS files must be owned by UID/GID `10001:10001`."
            )
        );
        assert!(
            docker_docs.contains(
                "sudo chown 10001:10001 \"$EDGE_RUNTIME_DIR/vehicle-tls/vehicle-tls.key\""
            )
        );
        assert!(
            docker_docs
                .contains("sudo chmod 0600 \"$EDGE_RUNTIME_DIR/vehicle-tls/vehicle-tls.key\"")
        );
    }

    #[test]
    fn debian_package_source_contains_both_payloads_and_safe_lifecycle_hooks() {
        let root = env!("CARGO_MANIFEST_DIR");
        let build = fs::read_to_string(format!("{root}/scripts/build-deb.sh")).unwrap();
        assert!(build.contains("--binary"));
        assert!(build.contains("--receiver-binary"));
        assert!(build.contains("--architecture"));
        assert!(build.contains("readelf"));
        assert!(build.contains("dpkg-deb --root-owner-group --build"));
        assert!(build.contains("usr/bin/teslatlas-edge"));
        assert!(build.contains("usr/lib/teslatlas-edge/fleet-telemetry"));
        assert!(!build.contains("rm -rf"));

        let control = fs::read_to_string(format!("{root}/packaging/linux/control.in")).unwrap();
        assert!(control.contains("Package: teslatlas-edge"));
        assert!(control.contains("Architecture: @ARCHITECTURE@"));
        assert!(control.contains("Depends:"));

        for hook in ["preinst", "postinst", "prerm", "postrm"] {
            let script = fs::read_to_string(format!("{root}/packaging/linux/{hook}")).unwrap();
            assert!(script.starts_with("#!/bin/sh"));
            assert!(script.contains("teslatlas-fleet-telemetry.service"));
            assert!(script.contains("teslatlas-edge.service"));
            assert!(!script.contains("rm -rf"));
        }
        assert!(
            fs::read_to_string(format!(
                "{root}/packaging/linux/teslatlas-fleet-telemetry.service"
            ))
            .unwrap()
            .contains("ConditionPathExists=/etc/teslatlas-edge/vehicle-client-ca.crt")
        );
        let preinst = fs::read_to_string(format!("{root}/packaging/linux/preinst")).unwrap();
        assert!(
            preinst
                .find("if [ -d /run/systemd/system ] && command -v systemctl")
                .unwrap()
                < preinst.find("edge_state=$(service_state").unwrap()
        );
        assert!(preinst.contains(
            "if systemctl is-active --quiet \"$inspected\"; then\n        systemctl stop \"$inspected\"\n    fi"
        ));
        assert!(
            preinst.contains(
                "printf '%s %s\\n' \"$edge_state\" \"$receiver_state\" > \"$state_file\""
            )
        );
        let postinst = fs::read_to_string(format!("{root}/packaging/linux/postinst")).unwrap();
        assert!(postinst.contains(
            "systemctl is-enabled --quiet \"$selected\" || systemctl enable \"$selected\""
        ));
        assert!(postinst.contains(
            "if systemctl is-enabled --quiet \"$selected\"; then\n                systemctl disable \"$selected\""
        ));
    }

    #[test]
    fn macos_package_source_keeps_core_and_receiver_choices_separate() {
        let root = env!("CARGO_MANIFEST_DIR");
        let build = fs::read_to_string(format!("{root}/scripts/build-macos-pkg.sh")).unwrap();
        assert!(build.contains("--binary"));
        assert!(build.contains("--receiver-binary"));
        assert!(build.contains("pkgbuild"));
        assert!(build.contains("productbuild --distribution"));
        assert!(build.contains("teslatlas-edge.core"));
        assert!(build.contains("teslatlas-edge.receiver"));

        let distribution =
            fs::read_to_string(format!("{root}/packaging/macos/Distribution.xml")).unwrap();
        assert!(distribution.contains("choice id=\"teslatlas-edge.core\""));
        assert!(distribution.contains("choice id=\"teslatlas-edge.receiver\""));
        assert!(distribution.contains("customize=\"always\""));
        let welcome =
            fs::read_to_string(format!("{root}/packaging/macos/resources/Welcome.html")).unwrap();
        assert!(welcome.contains("Development only"));

        let service = fs::read_to_string(format!(
            "{root}/packaging/macos/scripts/teslatlas-edge-service.sh"
        ))
        .unwrap();
        assert!(service.contains("launchctl bootstrap"));
        assert!(service.contains("launchctl bootout"));
        assert!(service.contains("/Users/Shared/TeslatlasEdge"));

        let uninstall = fs::read_to_string(format!(
            "{root}/packaging/macos/scripts/uninstall-teslatlas-edge.sh"
        ))
        .unwrap();
        assert!(uninstall.contains("--delete-data"));
        assert!(uninstall.contains("launchctl bootout"));
        assert!(uninstall.contains("/Users/Shared/TeslatlasEdge"));
        assert!(uninstall.contains("safe_payload"));
        assert!(uninstall.contains("stat -f '%u:%g'"));
        assert!(!uninstall.contains("rm -rf"));

        let data_preflight = uninstall
            .find("if [ \"$delete_data\" -eq 1 ]; then")
            .unwrap();
        let payload_cleanup = uninstall.find("for path in \\").unwrap();
        assert!(
            data_preflight < payload_cleanup,
            "--delete-data must validate the state root before removing package payloads"
        );
    }
}
