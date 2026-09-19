# SPDX-License-Identifier: AGPL-3.0-only
from __future__ import annotations

import http.client
import io
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[3] / "scripts" / "bootstrap-companions.py"
TOOLS = SCRIPT.parents[1] / "tools"
sys.path.insert(0, str(TOOLS))

from companions.cli import (
    _catalog_selection,
    _fetch_canonical_catalog,
    main,
    _operate,
    _parser,
    _promote_catalog,
    _refresh_catalog,
)
from companions.core import (
    BootstrapError,
    active_release_id,
    install_cohort,
    parse_catalog,
    select_cohort,
    source_manifest_for,
)


VALID_CATALOG_A = b'{"schema_version":1,"cohorts":[]}\n'
VALID_CATALOG_B = b'{"cohorts":[],"schema_version":1}\n'
PROTOCOL_REPOSITORY = "https://github.com/magrathean-uk/teslatlas-protocol.git"
PROFILE_SHA = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"


class _FixtureServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    block_on_close = False


@contextmanager
def _http_fixture(write_response):
    class Handler(socketserver.BaseRequestHandler):
        def handle(self) -> None:
            request = b""
            while b"\r\n\r\n" not in request:
                chunk = self.request.recv(4096)
                if not chunk:
                    return
                request += chunk
            try:
                write_response(self.request)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = _FixtureServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address[1]
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=1)


def _local_http_connection(port: int):
    def connect(_host: str, **options: object) -> http.client.HTTPConnection:
        return http.client.HTTPConnection(
            "127.0.0.1", port, timeout=options.get("timeout")
        )

    return connect


def _catalog_for_source(source: Path, commit: str) -> tuple[dict, dict]:
    record = source_manifest_for("protocol", source, PROTOCOL_REPOSITORY, commit)
    components = {
        "protocol": {
            "repository": PROTOCOL_REPOSITORY,
            "commit": commit,
            "source_sha256": record["source_sha256"],
            "product_version": "2026.36.2",
            "profile": {
                "id": "hub-http-v1",
                "revision": "1.0.0",
                "sha256": PROFILE_SHA,
            },
        },
        "sdk-typescript": {
            "repository": "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
            "commit": "2" * 40,
            "source_sha256": "2" * 64,
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0", "sha256": PROFILE_SHA},
            "artifacts": {
                "package_filename": "teslatlas-sdk-2026.36.2.tgz",
                "package_sha256": "070906b5e3ead04a32223ca996d88ebf6f22be252821e56ef1839da3a13e23d7",
            },
        },
        "sdk-swift": {
            "repository": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
            "commit": "3" * 40,
            "source_sha256": "3" * 64,
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0", "sha256": PROFILE_SHA},
        },
        "home-assistant": {
            "repository": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
            "commit": "4" * 40,
            "source_sha256": "4" * 64,
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0", "sha256": PROFILE_SHA},
            "artifacts": {
                "payload_manifest_sha256": "4" * 64,
                "selection_receipt_sha256": "5" * 64,
            },
        },
        "edge": {
            "repository": "https://github.com/magrathean-uk/teslatlas-edge.git",
            "commit": "5" * 40,
            "source_sha256": "5" * 64,
            "product_version": "2026.36.2",
            "profile": {
                "id": "edge-delivery-v2",
                "revision": "2.0.0",
                "sha256": "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224",
            },
        },
    }
    catalog = {
        "schema_version": 1,
        "cohorts": [
            {
                "product_version": "2026.36.2",
                "publication_status": "local-unpublished",
                "admitted_hub_versions": [],
                "components": components,
            }
        ],
    }
    return catalog, record


class CliTests(unittest.TestCase):
    def run_cli(
        self, *arguments: str, environment: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )

    def test_status_is_machine_readable_and_does_not_require_a_catalog(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_cli("status", "--prefix", temporary)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                json.loads(result.stdout),
                {"active_release": None, "status": "not-installed"},
            )
            self.assertEqual(result.stderr, "")

    def test_packaged_wrapper_imports_sibling_companion_modules(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            wrapper = package / "bootstrap-companions.py"
            shutil.copy2(SCRIPT, wrapper)
            source_modules = SCRIPT.parents[1] / "tools" / "companions"
            installed_modules = package / "companions"
            installed_modules.mkdir()
            for source in source_modules.glob("*.py"):
                shutil.copy2(source, installed_modules / source.name)

            result = subprocess.run(
                [sys.executable, str(wrapper), "status", "--prefix", str(package / "prefix")],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["status"], "not-installed")
            self.assertEqual(result.stderr, "")

    def test_install_requires_explicit_components_hub_catalog_and_prefix(self) -> None:
        result = self.run_cli("install", "--prefix", "/tmp/example")
        self.assertEqual(result.returncode, 2)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["status"], "error")
        self.assertIn("--components", payload["error"])
        self.assertNotIn("Traceback", result.stderr)

    def test_d1_plan_binds_only_the_accepted_home_assistant_arm64_selector(self) -> None:
        manifest = SCRIPT.parents[1] / "packaging" / "components.json"

        result = self.run_cli(
            "d1-plan",
            "--components",
            "home-assistant",
            "--selector",
            "debian13-arm64-container",
            "--component-manifest",
            str(manifest),
        )

        self.assertEqual(result.returncode, 0, result.stdout)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["status"], "historical_source_only")
        self.assertEqual(
            plan["scope"],
            "historical-d1-selection-not-an-active-catalog-identity",
        )
        self.assertFalse(plan["active_catalog_identity"])
        self.assertEqual(plan["component_ids"], ["home-assistant"])
        self.assertEqual(plan["selector_id"], "debian13-arm64-container")
        self.assertEqual(
            plan["candidate_source_and_artifact_identities"],
            {
                "source_repository": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
                "source_head": "f650331a1af0cf33cec271bc7eefc0f1201ebd4e",
                "payload_manifest_sha256": "73d702a85e0d79116c6a82f936080a2e393ac0b92e3ab0afa86d38f405a90940",
                "selection_receipt_sha256": "2f7b2b933f1f970fad49786530286fe3dadd463d3b063c4a6ffa50327e4f2be2",
            },
        )
        self.assertTrue(plan["runtime_receipt_required"])
        self.assertFalse(plan["activation_authorized"])
        self.assertNotIn("would_activate", plan)

    def test_d1_plan_rejects_unbound_hub_core_before_manifest_use(self) -> None:
        result = self.run_cli(
            "d1-plan",
            "--components",
            "hub-core",
            "--selector",
            "debian13-arm64-container",
            "--component-manifest",
            "/deliberately-absent/components.json",
        )

        self.assertEqual(result.returncode, 2)
        self.assertEqual(
            json.loads(result.stdout),
            {
                "status": "error",
                "error": "D1 selection is blocked until candidate identities are admitted for hub-core",
            },
        )

    def test_d1_plan_rejects_unbound_fleet_helpers_before_manifest_use(self) -> None:
        result = self.run_cli(
            "d1-plan",
            "--components",
            "fleet-helpers",
            "--selector",
            "debian13-arm64-container",
            "--component-manifest",
            "/deliberately-absent/components.json",
        )

        self.assertEqual(result.returncode, 2)
        self.assertEqual(
            json.loads(result.stdout),
            {
                "status": "error",
                "error": "D1 selection is blocked until candidate identities are admitted for fleet-helpers",
            },
        )

    def test_d1_install_fails_closed_as_historical_not_active_catalog(self) -> None:
        manifest_path = SCRIPT.parents[1] / "packaging" / "components.json"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / "ha-config"
            prefix = root / "companion-prefix"
            result = self.run_cli(
                "d1-install",
                "--components",
                "home-assistant",
                "--selector",
                "debian13-arm64-container",
                "--component-manifest",
                str(manifest_path),
                "--prefix",
                str(prefix),
                "--ha-config",
                str(config),
                "--hub-version",
                "2026.36.2",
                "--mode",
                "local-candidate",
                "--local-sources",
                str(root / "not-needed.json"),
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "historical D1 selection cannot be installed",
                json.loads(result.stdout)["error"],
            )
            self.assertFalse(prefix.exists())
            self.assertFalse((config / "custom_components" / "teslatlas_hub").exists())

    def test_d1_install_rejects_an_unadmitted_payload_digest_before_source_use(self) -> None:
        source_manifest = SCRIPT.parents[1] / "packaging" / "components.json"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / "ha-config"
            marker = config / ".storage" / "core.config_entries"
            marker.parent.mkdir(parents=True)
            marker.write_text("preserve\n", encoding="utf-8")
            manifest = root / "components.json"
            prefix = root / "companion-prefix"
            document = json.loads(source_manifest.read_text(encoding="utf-8"))
            component = next(
                value for value in document["components"] if value["id"] == "home-assistant"
            )
            component["payload"]["handoff_manifest_sha256"] = "0" * 64
            manifest.write_text(json.dumps(document), encoding="utf-8")
            result = self.run_cli(
                "d1-install",
                "--components",
                "home-assistant",
                "--selector",
                "debian13-arm64-container",
                "--component-manifest",
                str(manifest),
                "--prefix",
                str(prefix),
                "--ha-config",
                str(config),
                "--hub-version",
                "2026.36.2",
                "--mode",
                "local-candidate",
                "--local-sources",
                str(root / "not-needed.json"),
            )

            self.assertEqual(result.returncode, 2)
            self.assertEqual(
                json.loads(result.stdout),
                {"status": "error", "error": "D1 Home Assistant selector is not admitted"},
            )
            self.assertFalse(prefix.exists())
            self.assertFalse((config / "custom_components" / "teslatlas_hub").exists())
            self.assertEqual(marker.read_text(encoding="utf-8"), "preserve\n")

    def test_d1_plan_rejects_a_non_allowlisted_aggregate_source_repository(self) -> None:
        source_manifest = SCRIPT.parents[1] / "packaging" / "components.json"
        with tempfile.TemporaryDirectory() as temporary:
            manifest = Path(temporary) / "components.json"
            document = json.loads(source_manifest.read_text(encoding="utf-8"))
            component = next(
                value for value in document["components"] if value["id"] == "home-assistant"
            )
            component["source"]["repository"] = "untrusted-home-assistant"
            manifest.write_text(json.dumps(document), encoding="utf-8")

            result = self.run_cli(
                "d1-plan",
                "--components",
                "home-assistant",
                "--selector",
                "debian13-arm64-container",
                "--component-manifest",
                str(manifest),
            )

            self.assertEqual(result.returncode, 2)
            self.assertEqual(
                json.loads(result.stdout),
                {"status": "error", "error": "D1 Home Assistant selector is not admitted"},
            )

    def test_d1_plan_rejects_an_aggregate_source_product_version_mismatch(self) -> None:
        source_manifest = SCRIPT.parents[1] / "packaging" / "components.json"
        with tempfile.TemporaryDirectory() as temporary:
            manifest = Path(temporary) / "components.json"
            document = json.loads(source_manifest.read_text(encoding="utf-8"))
            component = next(
                value for value in document["components"] if value["id"] == "home-assistant"
            )
            component["source"]["product_version"] = "2026.36.3"
            manifest.write_text(json.dumps(document), encoding="utf-8")

            result = self.run_cli(
                "d1-plan",
                "--components",
                "home-assistant",
                "--selector",
                "debian13-arm64-container",
                "--component-manifest",
                str(manifest),
            )

            self.assertEqual(result.returncode, 2)
            self.assertEqual(
                json.loads(result.stdout),
                {"status": "error", "error": "D1 Home Assistant selector is not admitted"},
            )

    def test_manifest_writes_owner_only_content_bound_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("candidate\n")
            subprocess.run(["git", "init", "-q"], cwd=source, check=True)
            subprocess.run(
                [
                    "git",
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/magrathean-uk/teslatlas-protocol.git",
                ],
                cwd=source,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.email", "fixture@example.invalid"],
                cwd=source,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Fixture"], cwd=source, check=True
            )
            subprocess.run(["git", "add", "payload.txt"], cwd=source, check=True)
            subprocess.run(["git", "commit", "-qm", "fixture"], cwd=source, check=True)
            output = root / "sources.json"
            result = self.run_cli(
                "manifest",
                "--components",
                "protocol",
                "--source",
                f"protocol={source}",
                "--output",
                str(output),
            )
            self.assertEqual(result.returncode, 0, result.stdout)
            payload = json.loads(result.stdout)
            self.assertEqual(payload["status"], "manifest-created")
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            manifest = json.loads(output.read_text())
            self.assertEqual(set(manifest["components"]), {"protocol"})
            self.assertEqual(
                manifest["components"]["protocol"]["commit"],
                payload["commits"]["protocol"],
            )

    def test_filesystem_failures_remain_machine_readable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent_file = Path(temporary) / "not-a-directory"
            parent_file.write_text("occupied\n")
            result = self.run_cli("rollback", "--prefix", str(parent_file / "prefix"))
            self.assertEqual(result.returncode, 2)
            self.assertEqual(json.loads(result.stdout)["status"], "error")
            self.assertNotIn("Traceback", result.stderr)

    def test_edge_toolchain_override_requires_two_absolute_existing_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            catalog = Path(temporary) / "catalog.json"
            catalog.write_text("{}\n")
            result = self.run_cli(
                "install",
                "--components",
                "edge",
                "--prefix",
                str(Path(temporary) / "prefix"),
                "--hub-version",
                "2026.36.2",
                "--catalog",
                str(catalog),
                "--edge-target",
                "local-linux",
                "--edge-go-binary",
                "/tmp/go",
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "--edge-go-binary and --edge-tool-root must be supplied together",
                json.loads(result.stdout)["error"],
            )


class CatalogCacheTests(unittest.TestCase):
    def run_main(self, *arguments: str) -> tuple[int, str, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            result = main(list(arguments))
        return result, stdout.getvalue(), stderr.getvalue()

    def test_canonical_transport_is_fixed_bounded_and_does_not_follow_redirects(self) -> None:
        class Socket:
            def __init__(self) -> None:
                self.timeouts: list[float] = []

            def settimeout(self, value: float) -> None:
                self.timeouts.append(value)

        class Response:
            def __init__(self, status: int) -> None:
                self.status = status
                self.payloads = [VALID_CATALOG_A, b""]

            def getheader(self, _name: str) -> str:
                return str(len(VALID_CATALOG_A))

            def read(self, _length: int) -> bytes:
                return self.payloads.pop(0)

        class Connection:
            instances: list["Connection"] = []
            response_status = 200

            def __init__(self, host: str, **options: object) -> None:
                self.host = host
                self.options = options
                self.sock = Socket()
                self.requested: tuple[str, str, dict[str, str]] | None = None
                self.closed = False
                self.instances.append(self)

            def request(self, method: str, path: str, headers: dict[str, str]) -> None:
                self.requested = (method, path, headers)

            def getresponse(self) -> Response:
                return Response(self.response_status)

            def close(self) -> None:
                self.closed = True

        with mock.patch(
            "companions.cli.http.client.HTTPSConnection", Connection
        ), mock.patch("companions.cli.ssl.create_default_context", return_value=object()):
            self.assertEqual(_fetch_canonical_catalog(), VALID_CATALOG_A)
        connection = Connection.instances[-1]
        self.assertEqual(connection.host, "raw.githubusercontent.com")
        self.assertEqual(connection.options["timeout"], 15)
        self.assertEqual(
            connection.requested,
            (
                "GET",
                "/magrathean-uk/teslatlas-hub/refs/heads/main/tools/companions/catalog-current.json",
                {"Accept": "application/json"},
            ),
        )
        self.assertTrue(connection.sock.timeouts)
        self.assertTrue(connection.closed)

        Connection.response_status = 302
        with mock.patch(
            "companions.cli.http.client.HTTPSConnection", Connection
        ), mock.patch("companions.cli.ssl.create_default_context", return_value=object()):
            with self.assertRaisesRegex(BootstrapError, "response is invalid"):
                _fetch_canonical_catalog()
        self.assertTrue(Connection.instances[-1].closed)

    def test_total_deadline_covers_headers_chunk_trickle_and_connection_close(self) -> None:
        def delayed_headers(stream) -> None:
            time.sleep(0.2)
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: 35\r\n\r\n"
                + VALID_CATALOG_A
            )

        def trickled_chunks(stream) -> None:
            stream.sendall(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            for _ in range(20):
                stream.sendall(b"1\r\nx\r\n")
                time.sleep(0.02)
            stream.sendall(b"0\r\n\r\n")

        def connection_close(stream) -> None:
            stream.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            for _ in range(20):
                stream.sendall(b"x")
                time.sleep(0.02)

        for writer in (delayed_headers, trickled_chunks, connection_close):
            with self.subTest(writer=writer.__name__), _http_fixture(writer) as port:
                started = time.monotonic()
                with mock.patch(
                    "companions.cli.http.client.HTTPSConnection",
                    _local_http_connection(port),
                ), mock.patch("companions.cli.CATALOG_TIMEOUT_SECONDS", 0.05):
                    with self.assertRaisesRegex(BootstrapError, "deadline"):
                        _fetch_canonical_catalog()
                self.assertLess(time.monotonic() - started, 0.5)

    def test_complete_declared_chunked_and_connection_close_bodies_are_accepted(self) -> None:
        def declared_length(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: "
                + str(len(VALID_CATALOG_A)).encode()
                + b"\r\n\r\n"
                + VALID_CATALOG_A
            )

        def chunked(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
                + f"{len(VALID_CATALOG_A):x}\r\n".encode()
                + VALID_CATALOG_A
                + b"\r\n0\r\n\r\n"
            )

        def connection_close(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"
                + VALID_CATALOG_A
            )

        for writer in (declared_length, chunked, connection_close):
            with self.subTest(writer=writer.__name__), _http_fixture(writer) as port:
                with mock.patch(
                    "companions.cli.http.client.HTTPSConnection",
                    _local_http_connection(port),
                ):
                    self.assertEqual(_fetch_canonical_catalog(), VALID_CATALOG_A)

    def test_http_protocol_failures_emit_one_json_and_preserve_selected_state(self) -> None:
        def truncated_chunk(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab"
            )

        def truncated_body(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: "
                + str(len(VALID_CATALOG_B) + 1).encode()
                + b"\r\n\r\n"
                + VALID_CATALOG_B
            )

        def malformed_length(stream) -> None:
            stream.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: +35\r\n\r\n"
                + VALID_CATALOG_A
            )

        def malformed_status(stream) -> None:
            stream.sendall(b"NOT-HTTP\r\n\r\n")

        def oversized_header(stream) -> None:
            stream.sendall(b"HTTP/1.1 200 OK\r\nX-Large: " + b"x" * 70000 + b"\r\n\r\n")

        def ordinary_timeout(_stream) -> None:
            time.sleep(0.2)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            shipped = root / "shipped.json"
            shipped.write_bytes(VALID_CATALOG_A)
            source = root / "source"
            source.mkdir()
            source.joinpath("payload.txt").write_text("active\n")
            active_catalog, active_record = _catalog_for_source(source, "1" * 40)
            active_cohort = select_cohort(
                parse_catalog(active_catalog),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )

            def build(_name: str, source_path: Path, output_path: Path) -> dict:
                output_path.mkdir()
                output_path.joinpath("installed.txt").write_text(
                    source_path.joinpath("payload.txt").read_text()
                )
                return {"commands": ["fixture-build"], "dependencies": {}}

            installed = install_cohort(
                prefix,
                active_cohort,
                {"protocol": active_record},
                build,
                hub_version="2026.36.2",
            )
            active_before = installed["active_release"]
            selected = _promote_catalog(prefix, VALID_CATALOG_A)
            selector_before = (prefix / "catalog-cache/current.json").read_bytes()
            generation_before = selected.read_bytes()

            for writer in (
                truncated_chunk,
                truncated_body,
                malformed_length,
                malformed_status,
                oversized_header,
                ordinary_timeout,
            ):
                with self.subTest(writer=writer.__name__), _http_fixture(writer) as port:
                    with mock.patch(
                        "companions.cli.http.client.HTTPSConnection",
                        _local_http_connection(port),
                    ), mock.patch(
                        "companions.cli.CATALOG_TIMEOUT_SECONDS", 0.05
                    ), mock.patch("companions.cli.os.geteuid", return_value=501):
                        result, stdout, stderr = self.run_main(
                            "update",
                            "--components",
                            "protocol",
                            "--prefix",
                            str(prefix),
                            "--hub-version",
                            "2026.36.2",
                            "--catalog",
                            str(shipped),
                        )
                    self.assertEqual(result, 2)
                    self.assertEqual(len(stdout.splitlines()), 1)
                    payload = json.loads(stdout)
                    self.assertEqual(payload["status"], "error")
                    self.assertIn("catalog", payload["error"])
                    self.assertNotIn("ab", payload["error"])
                    self.assertEqual(stderr, "")
                    self.assertEqual(
                        (prefix / "catalog-cache/current.json").read_bytes(),
                        selector_before,
                    )
                    self.assertEqual(selected.read_bytes(), generation_before)
                    self.assertEqual(active_release_id(prefix), active_before)

    def test_root_and_invalid_options_are_rejected_before_transport_or_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            catalog = root / "catalog.json"
            catalog.write_bytes(VALID_CATALOG_A)
            root_arguments = _parser().parse_args(
                [
                    "update",
                    "--components",
                    "protocol",
                    "--prefix",
                    str(prefix),
                    "--hub-version",
                    "2026.36.2",
                    "--catalog",
                    str(catalog),
                ]
            )
            with mock.patch("companions.cli.os.geteuid", return_value=0), mock.patch(
                "companions.cli._refresh_catalog"
            ) as refresh:
                with self.assertRaisesRegex(BootstrapError, "root"):
                    _operate(root_arguments)
            refresh.assert_not_called()
            self.assertFalse(prefix.exists())

            invalid_arguments = _parser().parse_args(
                [
                    "update",
                    "--components",
                    "protocol",
                    "--prefix",
                    str(prefix),
                    "--hub-version",
                    "2026.36.2",
                    "--catalog",
                    str(catalog),
                    "--local-sources",
                    str(root / "forbidden.json"),
                ]
            )
            with mock.patch("companions.cli.os.geteuid", return_value=501), mock.patch(
                "companions.cli._refresh_catalog"
            ) as refresh:
                with self.assertRaisesRegex(BootstrapError, "does not accept"):
                    _operate(invalid_arguments)
            refresh.assert_not_called()
            self.assertFalse(prefix.exists())

    def test_symlink_prefix_and_cache_reject_before_fetch_without_touching_targets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / "outside"
            outside.mkdir()
            marker = outside / "marker"
            marker.write_text("unchanged\n")
            prefix_link = root / "prefix-link"
            prefix_link.symlink_to(outside, target_is_directory=True)
            calls = []

            with self.assertRaises(BootstrapError):
                _refresh_catalog(prefix_link, fetch=lambda: calls.append(1) or VALID_CATALOG_A)
            self.assertEqual(calls, [])
            self.assertEqual(marker.read_text(), "unchanged\n")
            self.assertEqual(sorted(path.name for path in outside.iterdir()), ["marker"])

            prefix = root / "prefix"
            prefix.mkdir()
            cache_target = root / "cache-target"
            cache_target.mkdir()
            cache_marker = cache_target / "marker"
            cache_marker.write_text("unchanged\n")
            (prefix / "catalog-cache").symlink_to(cache_target, target_is_directory=True)
            with self.assertRaises(BootstrapError):
                _refresh_catalog(prefix, fetch=lambda: calls.append(2) or VALID_CATALOG_A)
            self.assertEqual(calls, [])
            self.assertEqual(cache_marker.read_text(), "unchanged\n")
            self.assertEqual(sorted(path.name for path in cache_target.iterdir()), ["marker"])

    def test_failed_and_interrupted_refreshes_keep_prior_verified_selection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            shipped = root / "shipped.json"
            shipped.write_bytes(VALID_CATALOG_A)
            selected_a = _promote_catalog(prefix, VALID_CATALOG_A)
            self.assertEqual(_catalog_selection(prefix, shipped), selected_a)

            invalid = b'{"schema_version":1,"cohorts":[{}]}\n'
            with self.assertRaises(BootstrapError):
                _refresh_catalog(prefix, fetch=lambda: invalid)
            self.assertEqual(_catalog_selection(prefix, shipped), selected_a)

            for phase in ("generation-persisted", "selection-persisted"):
                def interrupt(observed: str, expected: str = phase) -> None:
                    if observed == expected:
                        raise RuntimeError(f"interrupted at {expected}")

                with self.assertRaisesRegex(RuntimeError, phase):
                    _refresh_catalog(
                        prefix, fetch=lambda: VALID_CATALOG_B, checkpoint=interrupt
                    )
                self.assertEqual(_catalog_selection(prefix, shipped), selected_a)

    def test_hard_interruption_during_generation_write_allows_identical_retry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            shipped = root / "shipped.json"
            shipped.write_bytes(VALID_CATALOG_A)
            selected_a = _promote_catalog(prefix, VALID_CATALOG_A)
            ready = root / "ready"
            script = """
import sys, time
from pathlib import Path
from companions.cli import _refresh_catalog
prefix, ready = map(Path, sys.argv[1:])
payload = b'{"cohorts":[],"schema_version":1}\\n'
def checkpoint(phase):
    if phase == 'generation-write-opened':
        ready.write_text('ready\\n')
        while True:
            time.sleep(1)
_refresh_catalog(prefix, fetch=lambda: payload, checkpoint=checkpoint)
"""
            child = subprocess.Popen(
                [sys.executable, "-c", script, str(prefix), str(ready)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=dict(os.environ, PYTHONPATH=str(TOOLS)),
            )
            deadline = time.monotonic() + 3
            while not ready.exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(
                ready.exists(), child.stderr.read() if child.poll() is not None else ""
            )
            child.kill()
            child.communicate(timeout=3)
            self.assertEqual(_catalog_selection(prefix, shipped), selected_a)

            selected_b = _refresh_catalog(prefix, fetch=lambda: VALID_CATALOG_B)
            self.assertEqual(selected_b.read_bytes(), VALID_CATALOG_B)
            self.assertEqual(_catalog_selection(prefix, shipped), selected_b)

    def test_status_rollback_and_local_candidate_ignore_damaged_production_cache(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()

            def build(_name: str, source_path: Path, output_path: Path) -> dict:
                output_path.mkdir()
                (output_path / "installed.txt").write_text(
                    (source_path / "payload.txt").read_text()
                )
                return {"commands": ["fixture-build"], "dependencies": {}}

            source.joinpath("payload.txt").write_text("first\n")
            first_catalog, first_record = _catalog_for_source(source, "1" * 40)
            first_cohort = select_cohort(
                parse_catalog(first_catalog),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            first = install_cohort(
                prefix,
                first_cohort,
                {"protocol": first_record},
                build,
                hub_version="2026.36.2",
            )
            first_id = first["active_release"]

            source.joinpath("payload.txt").write_text("second\n")
            second_catalog, second_record = _catalog_for_source(source, "2" * 40)
            second_cohort = select_cohort(
                parse_catalog(second_catalog),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            second = install_cohort(
                prefix,
                second_cohort,
                {"protocol": second_record},
                build,
                hub_version="2026.36.2",
            )
            second_id = second["active_release"]
            self.assertNotEqual(first_id, second_id)

            outside = root / "outside-cache"
            outside.mkdir()
            marker = outside / "marker"
            marker.write_text("unchanged\n")
            (prefix / "catalog-cache").symlink_to(outside, target_is_directory=True)
            catalog_path = root / "local-catalog.json"
            catalog_path.write_text(json.dumps(second_catalog) + "\n")
            cache_before = marker.read_bytes()

            status_result = self.run_main("status", "--prefix", str(prefix))
            self.assertEqual(status_result[0], 0, status_result[2])
            self.assertEqual(json.loads(status_result[1])["active_release"], second_id)

            local_result = self.run_main(
                "install",
                "--components",
                "protocol",
                "--prefix",
                str(prefix),
                "--hub-version",
                "2026.36.2",
                "--catalog",
                str(catalog_path),
                "--mode",
                "local-candidate",
                "--local-sources",
                str(root / "deliberately-absent.json"),
            )
            self.assertEqual(local_result[0], 0, local_result[2])
            self.assertEqual(json.loads(local_result[1])["status"], "no-op")
            self.assertEqual(active_release_id(prefix), second_id)

            rollback_result = self.run_main("rollback", "--prefix", str(prefix))
            self.assertEqual(rollback_result[0], 0, rollback_result[2])
            self.assertEqual(json.loads(rollback_result[1])["active_release"], first_id)
            self.assertEqual(active_release_id(prefix), first_id)
            self.assertEqual(marker.read_bytes(), cache_before)
            self.assertEqual(sorted(path.name for path in outside.iterdir()), ["marker"])
            self.assertTrue((prefix / "catalog-cache").is_symlink())
            self.assertEqual(os.readlink(prefix / "catalog-cache"), str(outside))

    def test_nonregular_cache_selector_is_rejected_without_blocking(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            cache = prefix / "catalog-cache"
            cache.mkdir(parents=True)
            os.mkfifo(cache / "current.json")
            shipped = root / "shipped.json"
            shipped.write_bytes(VALID_CATALOG_A)
            with self.assertRaisesRegex(BootstrapError, "unsafe"):
                _catalog_selection(prefix, shipped)

    def test_concurrent_refresh_is_serialized_and_selection_never_splits(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            shipped = root / "shipped.json"
            shipped.write_bytes(VALID_CATALOG_A)
            selected_a = _promote_catalog(prefix, VALID_CATALOG_A)
            ready = root / "ready"
            release = root / "release"
            script = """
import sys, time
from pathlib import Path
from companions.cli import _refresh_catalog
prefix, ready, release = map(Path, sys.argv[1:])
payload = b'{"cohorts":[],"schema_version":1}\\n'
def checkpoint(phase):
    if phase == 'generation-persisted':
        ready.write_text('ready\\n')
        while not release.exists():
            time.sleep(0.01)
_refresh_catalog(prefix, fetch=lambda: payload, checkpoint=checkpoint)
"""
            environment = dict(os.environ, PYTHONPATH=str(TOOLS))
            first = subprocess.Popen(
                [sys.executable, "-c", script, str(prefix), str(ready), str(release)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=environment,
            )
            deadline = time.monotonic() + 10
            while not ready.exists() and first.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            try:
                self.assertTrue(
                    ready.exists(), first.stderr.read() if first.poll() is not None else ""
                )
                self.assertEqual(_catalog_selection(prefix, shipped), selected_a)
                with self.assertRaisesRegex(BootstrapError, "busy"):
                    _refresh_catalog(prefix, fetch=lambda: VALID_CATALOG_A + b" ")
                self.assertEqual(_catalog_selection(prefix, shipped), selected_a)
            finally:
                release.write_text("release\n")
            stdout, stderr = first.communicate(timeout=10)
            self.assertEqual(first.returncode, 0, f"{stdout}\n{stderr}")
            selected_b = _catalog_selection(prefix, shipped)
            self.assertEqual(selected_b.read_bytes(), VALID_CATALOG_B)
            self.assertNotEqual(selected_b, selected_a)


if __name__ == "__main__":
    unittest.main()
