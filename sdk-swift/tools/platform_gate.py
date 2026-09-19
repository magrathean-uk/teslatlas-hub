#!/usr/bin/env python3
"""Verify and invoke the bounded Swift platform-floor harnesses."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
from typing import Any

import source_handoff


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "tools/platform-gates.json"
PUBLIC_PRODUCTS = list(source_handoff.PUBLIC_PRODUCTS)
PROTOCOL_SOURCE_COMMIT = source_handoff.PROTOCOL_SOURCE_COMMIT
SOURCE_IDENTITY = "733071fcd8c7db0e64b38547dab90dd354ad728dc2d9efee7fd2de438a14876e"
SOURCE_COMMIT = "d7ac4488fc5908015e8de55cd57983ea87172266"
SOURCE_INPUT_FILES = 124
SOURCE_INPUT_BYTES = 769736
IOS_PROJECT = "iOSRuntimeHost/CurrentHubRuntimeHost.xcodeproj"
IOS_SCHEME = "CurrentHubRuntimeHost"
LINUX_TAG = "swift:6.0.3-jammy"
LINUX_PLATFORM = "linux/arm64/v8"
LINUX_INDEX_DIGEST = "sha256:e2b0410500126d7f569d387b5817426cef5c38cc02dc494c3dc5edc8e10304d6"
LINUX_ARM64_DIGEST = "sha256:c84da0197afcc90ef90a64194d4d451be7c090a845bcbf632755f9c16334ba8f"
OFFICIAL_CATALOG_URL = "https://github.com/docker-library/official-images/blob/master/library/swift"
DOCKERFILE_SOURCE_COMMIT = "f44060cdf224436060d2df98a5c3f63f2600de63"


class PlatformGateError(RuntimeError):
    """A platform gate contract or preflight condition failed."""


def _strict_json_loads(payload: bytes | str, label: str) -> Any:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise PlatformGateError(f"{label} contains duplicate key: {key}")
            result[key] = value
        return result

    try:
        return json.loads(payload, object_pairs_hook=reject_duplicates)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise PlatformGateError(f"invalid {label}: {error}") from error


def _exact_keys(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PlatformGateError(f"{label} must be an object")
    if set(value) != expected:
        raise PlatformGateError(
            f"{label} keys differ: expected {sorted(expected)}, got {sorted(value)}"
        )
    return value


def load_contract(path: Path = CONTRACT_PATH) -> dict[str, Any]:
    contract = _strict_json_loads(path.read_bytes(), "platform gate contract")
    contract = _exact_keys(
        contract,
        {
            "schema_version",
            "public_products",
            "protocol_runtime_source_commit",
            "source_handoff",
            "ios17",
            "macos14",
            "linux_arm64",
        },
        "platform gate contract",
    )
    source = _exact_keys(
        contract["source_handoff"],
        {"source_commit", "canonical_root", "identity_sha256", "input_files", "input_bytes"},
        "source_handoff",
    )
    ios = _exact_keys(
        contract["ios17"],
        {"project", "scheme", "runtime_version", "simulator_name"},
        "ios17",
    )
    macos = _exact_keys(
        contract["macos14"],
        {"required_major", "required_arch", "consumer_root"},
        "macos14",
    )
    linux = _exact_keys(
        contract["linux_arm64"],
        {
            "tag",
            "platform",
            "parent_index_digest",
            "arm64_child_digest",
            "official_catalog_url",
            "dockerfile_source_git_commit",
            "native_arches",
            "emulation_allowed",
            "forbidden_inputs",
        },
        "linux_arm64",
    )
    expected = (
        contract["schema_version"] == 1
        and contract["public_products"] == PUBLIC_PRODUCTS
        and contract["protocol_runtime_source_commit"] == PROTOCOL_SOURCE_COMMIT
        and source
        == {
            "source_commit": SOURCE_COMMIT,
            "canonical_root": source_handoff.CANONICAL_ROOT,
            "identity_sha256": SOURCE_IDENTITY,
            "input_files": SOURCE_INPUT_FILES,
            "input_bytes": SOURCE_INPUT_BYTES,
        }
        and ios
        == {
            "project": IOS_PROJECT,
            "scheme": IOS_SCHEME,
            "runtime_version": "17.0",
            "simulator_name": "iPhone 15",
        }
        and macos
        == {
            "required_major": 14,
            "required_arch": "arm64",
            "consumer_root": source_handoff.CONSUMER_ROOT,
        }
        and linux["tag"] == LINUX_TAG
        and linux["platform"] == LINUX_PLATFORM
        and linux["parent_index_digest"] == LINUX_INDEX_DIGEST
        and linux["arm64_child_digest"] == LINUX_ARM64_DIGEST
        and linux["official_catalog_url"] == OFFICIAL_CATALOG_URL
        and linux["dockerfile_source_git_commit"] == DOCKERFILE_SOURCE_COMMIT
        and linux["native_arches"] == ["aarch64", "arm64"]
        and linux["emulation_allowed"] is False
        and linux["forbidden_inputs"] == ["matrix_wire.py", "linux/amd64", "qemu"]
    )
    if not expected:
        raise PlatformGateError("platform gate contract differs from the accepted inputs")
    return contract


def _require_markers(path: Path, markers: list[str], label: str) -> str:
    contents = path.read_text(encoding="utf-8")
    for marker in markers:
        if marker not in contents:
            raise PlatformGateError(f"{label} is missing {marker!r}")
    return contents


def _prepare_accepted_handoff(
    root: Path, handoff: Path, contract: dict[str, Any]
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="teslatlas-accepted-source-") as temporary:
        snapshot = Path(temporary) / source_handoff.CANONICAL_ROOT
        snapshot.mkdir()
        for directory in source_handoff.SOURCE_DIRECTORIES:
            (snapshot / directory).mkdir(parents=True, exist_ok=True)
        for relative in source_handoff._selected_source_files(root):
            completed = subprocess.run(
                ["git", "show", f"{contract['source_handoff']['source_commit']}:{relative}"],
                cwd=root,
                check=False,
                capture_output=True,
            )
            if completed.returncode != 0:
                raise PlatformGateError(f"accepted source snapshot is missing {relative}")
            destination = snapshot / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(completed.stdout)
        return source_handoff.prepare(snapshot, handoff)


def _verify_source_identity(root: Path, contract: dict[str, Any]) -> None:
    with tempfile.TemporaryDirectory(prefix="teslatlas-platform-gate-source-") as temporary:
        handoff = Path(temporary) / "handoff"
        manifest = _prepare_accepted_handoff(root, handoff, contract)
        source = manifest["source_package"]
        actual_bytes = sum(record["size"] for record in source["files"])
        expected = contract["source_handoff"]
        if (
            source["canonical_root"] != expected["canonical_root"]
            or source["identity_sha256"] != expected["identity_sha256"]
            or len(source["files"]) != expected["input_files"]
            or actual_bytes != expected["input_bytes"]
            or manifest["protocol_runtime_source_commit"]
            != contract["protocol_runtime_source_commit"]
        ):
            raise PlatformGateError("prepared source handoff differs from accepted inputs")
        dumped = _strict_json_loads(
            subprocess.run(
                [
                    "swift",
                    "package",
                    "dump-package",
                    "--package-path",
                    str(handoff / source_handoff.CONSUMER_ROOT),
                ],
                check=True,
                capture_output=True,
            ).stdout,
            "external consumer manifest",
        )
        dependencies = dumped.get("dependencies") if isinstance(dumped, dict) else None
        if not isinstance(dependencies, list) or "teslatlas-sdk-swift" not in str(dependencies):
            raise PlatformGateError("external consumer is not bound to canonical handoff package")


def verify_repository(root: Path = ROOT) -> dict[str, Any]:
    root = root.resolve(strict=True)
    contract = load_contract(root / "tools/platform-gates.json")
    _verify_source_identity(root, contract)

    products = contract["public_products"]
    spec = _require_markers(
        root / "iOSRuntimeHost/project.yml",
        [
            'iOS: "17.0"',
            "schemes:",
            f"  {IOS_SCHEME}:",
            "CurrentHubRuntimeTests:",
        ]
        + [f"product: {product}" for product in products],
        "iOS host spec",
    )
    for product in products:
        if spec.count(f"product: {product}") != 2:
            raise PlatformGateError(
                f"iOS host spec must bind {product} to app and tests exactly once each"
            )
    _require_markers(
        root
        / "iOSRuntimeHost/CurrentHubRuntimeHost.xcodeproj/xcshareddata/xcschemes"
        / f"{IOS_SCHEME}.xcscheme",
        ["BuildAction", "TestAction", "CurrentHubRuntimeTests"],
        "shared iOS scheme",
    )
    _require_markers(
        root / "iOSRuntimeHost/Sources/PlatformSurfaceProbe.swift",
        [f"import {product}" for product in products],
        "iOS product probe",
    )
    dockerfile = _require_markers(
        root / "Dockerfile",
        [
            f"FROM --platform={LINUX_PLATFORM} {LINUX_TAG}@{LINUX_ARM64_DIGEST}",
            f"Parent OCI index: {LINUX_INDEX_DIGEST}",
            "COPY teslatlas-sdk-swift/",
            "COPY external-four-library-consumer/",
        ],
        "Dockerfile",
    )
    for forbidden in contract["linux_arm64"]["forbidden_inputs"]:
        if forbidden in dockerfile.lower():
            raise PlatformGateError(f"Dockerfile contains forbidden Linux input: {forbidden}")

    return {
        "verified": True,
        "runtime_executed": False,
        "protocol_runtime_source_commit": PROTOCOL_SOURCE_COMMIT,
        "source_package_identity_sha256": SOURCE_IDENTITY,
        "source_input_files": SOURCE_INPUT_FILES,
        "source_input_bytes": SOURCE_INPUT_BYTES,
        "public_products": products,
    }


def command_for(gate: str, root: Path = ROOT, scratch: Path | None = None) -> list[str]:
    contract = load_contract(root / "tools/platform-gates.json")
    scratch_text = str(scratch or Path("<isolated-scratch>"))
    if gate == "ios17":
        ios = contract["ios17"]
        return [
            "xcodebuild",
            "-project",
            str(root / ios["project"]),
            "-scheme",
            ios["scheme"],
            "-destination",
            f"platform=iOS Simulator,OS={ios['runtime_version']},name={ios['simulator_name']}",
            "-derivedDataPath",
            scratch_text,
            "test",
        ]
    if gate == "macos14":
        return [
            "swift",
            "build",
            "--package-path",
            str(Path(scratch_text) / source_handoff.CONSUMER_ROOT),
            "--scratch-path",
            str(Path(scratch_text) / "swift-build"),
        ]
    if gate == "linux-arm64":
        context = str(scratch or Path("<verified-source-handoff>"))
        return [
            "docker",
            "build",
            "--platform",
            LINUX_PLATFORM,
            "--tag",
            "teslatlas-swift-platform-gate:local-arm64",
            "--file",
            str(root / "Dockerfile"),
            context,
        ]
    raise PlatformGateError(f"unknown gate: {gate}")


def _run(command: list[str], cwd: Path = ROOT) -> None:
    completed = subprocess.run(command, cwd=cwd, check=False)
    if completed.returncode != 0:
        raise PlatformGateError(
            f"command failed with exit {completed.returncode}: {' '.join(command)}"
        )


def _require_native_darwin(required_major: int) -> None:
    actual_version = platform.mac_ver()[0]
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise PlatformGateError("gate requires a native Apple-silicon macOS host")
    if not actual_version or int(actual_version.split(".", 1)[0]) != required_major:
        raise PlatformGateError(
            f"gate requires macOS {required_major}; current host is {actual_version or 'unknown'}"
        )


def _require_apple_silicon() -> None:
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise PlatformGateError("gate requires a native Apple-silicon host")


def _require_ios_simulator(runtime_version: str, simulator_name: str) -> None:
    completed = subprocess.run(
        ["xcrun", "simctl", "list", "devices", "available", "--json"],
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise PlatformGateError("could not inventory available iOS simulators")
    inventory = _strict_json_loads(completed.stdout, "simulator inventory")
    if not isinstance(inventory, dict) or not isinstance(inventory.get("devices"), dict):
        raise PlatformGateError("simulator inventory has an unexpected shape")
    runtime = "com.apple.CoreSimulator.SimRuntime.iOS-" + runtime_version.replace(".", "-")
    devices = inventory["devices"].get(runtime)
    if not isinstance(devices, list) or not any(
        isinstance(device, dict)
        and device.get("name") == simulator_name
        and device.get("isAvailable") is True
        for device in devices
    ):
        raise PlatformGateError(
            f"required iOS {runtime_version} {simulator_name} simulator is unavailable"
        )


def run_gate(gate: str, root: Path = ROOT) -> None:
    contract = load_contract(root / "tools/platform-gates.json")
    verify_repository(root)
    if gate == "macos14":
        _require_native_darwin(contract["macos14"]["required_major"])
        with tempfile.TemporaryDirectory(prefix="teslatlas-macos14-gate-") as temporary:
            scratch = Path(temporary)
            _prepare_accepted_handoff(root, scratch / "handoff", contract)
            prepared_consumer = scratch / "handoff" / source_handoff.CONSUMER_ROOT
            _run(
                [
                    "swift",
                    "build",
                    "--package-path",
                    str(prepared_consumer),
                    "--scratch-path",
                    str(scratch / "swift-build"),
                ],
                root,
            )
        return
    if gate == "ios17":
        _require_apple_silicon()
        if not shutil.which("xcodebuild"):
            raise PlatformGateError("xcodebuild is required")
        _require_ios_simulator(
            contract["ios17"]["runtime_version"],
            contract["ios17"]["simulator_name"],
        )
        with tempfile.TemporaryDirectory(prefix="teslatlas-ios17-gate-") as temporary:
            _run(command_for(gate, root, Path(temporary)), root)
        return
    if gate == "linux-arm64":
        native_arches = contract["linux_arm64"]["native_arches"]
        if platform.system() != "Linux" or platform.machine().lower() not in native_arches:
            raise PlatformGateError("Linux gate requires a native Linux ARM64 host")
        if not shutil.which("docker"):
            raise PlatformGateError("docker is required")
        server = subprocess.run(
            ["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"],
            check=False,
            capture_output=True,
            text=True,
        )
        if server.returncode != 0 or server.stdout.strip() not in {
            "linux/arm64",
            "linux/aarch64",
        }:
            raise PlatformGateError("Docker Engine must itself be native Linux ARM64")
        image = "teslatlas-swift-platform-gate:local-arm64"
        existing = subprocess.run(
            ["docker", "image", "inspect", image],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if existing.returncode == 0:
            raise PlatformGateError(f"owned Linux gate image tag already exists: {image}")
        try:
            with tempfile.TemporaryDirectory(prefix="teslatlas-linux-arm64-gate-") as temporary:
                handoff = Path(temporary) / "handoff"
                _prepare_accepted_handoff(root, handoff, contract)
                _run(command_for(gate, root, handoff), root)
                _run(
                    [
                        "docker",
                        "run",
                        "--rm",
                        "--platform",
                        LINUX_PLATFORM,
                        image,
                    ],
                    root,
                )
        finally:
            subprocess.run(
                ["docker", "image", "rm", "--force", image],
                cwd=root,
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        return
    raise PlatformGateError(f"unknown gate: {gate}")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("verify")
    command_parser = subparsers.add_parser("command")
    command_parser.add_argument("gate", choices=("ios17", "macos14", "linux-arm64"))
    run_parser = subparsers.add_parser("run")
    run_parser.add_argument("gate", choices=("ios17", "macos14", "linux-arm64"))
    return parser


def main() -> int:
    arguments = _parser().parse_args()
    try:
        if arguments.command == "verify":
            output: Any = verify_repository(ROOT)
        elif arguments.command == "command":
            output = {"gate": arguments.gate, "command": command_for(arguments.gate)}
        else:
            run_gate(arguments.gate, ROOT)
            output = {"gate": arguments.gate, "passed": True}
    except (OSError, PlatformGateError, source_handoff.HandoffError) as error:
        raise SystemExit(f"platform gate failed: {error}") from error
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
