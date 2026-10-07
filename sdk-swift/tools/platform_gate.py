#!/usr/bin/env python3
"""Verify and invoke the bounded Swift platform-floor harnesses."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import uuid
from typing import Any

import source_handoff
import owned_command


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "tools/platform-gates.json"
PUBLIC_PRODUCTS = list(source_handoff.PUBLIC_PRODUCTS)
LINUX_CONSUMER_OUTPUT = ",".join(PUBLIC_PRODUCTS)
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
        for relative in _accepted_members(root, contract):
            completed = _checked_capture(
                ["git", "show", f"{contract['source_handoff']['source_commit']}:{relative}"], root
            )
            destination = snapshot / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(completed.stdout)
        # This older accepted identity predates NOTICE packaging. Never refresh
        # its member list or pin as a side effect of current source changes.
        return source_handoff.prepare(snapshot, handoff, root_files=source_handoff.LEGACY_ROOT_FILES)


def _accepted_tree(root: Path, contract: dict[str, Any]) -> list[str]:
    completed = _checked_capture(
        ["git", "ls-tree", "-r", "-z", contract["source_handoff"]["source_commit"]], root
    )
    result = []
    for entry in completed.stdout.split(b"\0"):
        if not entry:
            continue
        try:
            metadata, raw_path = entry.split(b"\t", 1)
            mode, kind, _ = metadata.split(b" ")
            relative = raw_path.decode("utf-8")
            source_handoff._validate_relative_path(relative)
        except (ValueError, UnicodeError, source_handoff.HandoffError) as error:
            raise PlatformGateError("accepted tree contains an invalid member") from error
        if mode not in (b"100644", b"100755") or kind != b"blob":
            raise PlatformGateError("accepted tree contains a non-regular member")
        result.append(relative)
    if len(result) != len(set(result)):
        raise PlatformGateError("accepted tree contains duplicate members")
    return sorted(result)


def _accepted_members(root: Path, contract: dict[str, Any]) -> list[str]:
    mandatory = set(source_handoff.LEGACY_ROOT_FILES + source_handoff.PUBLIC_DOCUMENTS)
    members = _accepted_tree(root, contract)
    if not mandatory.issubset(members):
        raise PlatformGateError("accepted source snapshot is missing required members")
    return [relative for relative in members if relative in mandatory or (
        any(relative.startswith(directory + "/") for directory in source_handoff.SOURCE_DIRECTORIES)
        and not set(Path(relative).parts) & source_handoff.IGNORED_NAMES
    )]


def _checked_capture(command, cwd=ROOT, *, timeout=owned_command.INSPECTION_SECONDS, text=False):
    completed = owned_command.run(command, cwd=cwd, timeout=timeout, text=text)
    if completed.returncode != 0:
        raise PlatformGateError(f"command failed with exit {completed.returncode}: {command[0]}")
    return completed


def _prepare_ios_host(root: Path, scratch: Path, contract: dict[str, Any]) -> None:
    """Stage historical host/test inputs and explicitly bind the accepted package."""
    ios_root = scratch / "ios-root"
    host_members = [path for path in _accepted_tree(root, contract) if path.startswith("iOSRuntimeHost/")]
    if not host_members:
        raise PlatformGateError("accepted iOS host is missing")
    for relative in host_members:
        data = _checked_capture(
            ["git", "show", f"{contract['source_handoff']['source_commit']}:{relative}"], root
        ).stdout
        target = ios_root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    shutil.copytree(scratch / "handoff" / source_handoff.CANONICAL_ROOT / "Tests", ios_root / "Tests")
    package = (scratch / "handoff" / source_handoff.CANONICAL_ROOT).resolve()
    project = ios_root / IOS_PROJECT / "project.pbxproj"
    contents = project.read_text(encoding="utf-8")
    marker = "relativePath = ..;"
    if contents.count(marker) != 1:
        raise PlatformGateError("accepted iOS host package binding is ambiguous")
    contents = contents.replace(marker, "relativePath = " + json.dumps(str(package)) + ";")
    project.write_text(contents, encoding="utf-8")


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
            _checked_capture(
                [
                    "swift",
                    "package",
                    "dump-package",
                    "--package-path",
                    str(handoff / source_handoff.CONSUMER_ROOT),
                ],
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
            "COPY --chown=swiftuser:swiftuser teslatlas-sdk-swift/",
            "COPY --chown=swiftuser:swiftuser external-four-library-consumer/",
            "RUN chmod -R a-w /workspace/teslatlas-sdk-swift /workspace/external-four-library-consumer",
            "ENV HOME=/home/swiftuser",
            'CMD ["swift", "run",',
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
            str(Path(scratch_text) / "ios-root" / ios["project"]),
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
    completed = owned_command.run(command, cwd=cwd, timeout=owned_command.BUILD_SECONDS, text=True)
    sys.stdout.write(completed.stdout)
    sys.stderr.write(completed.stderr)
    if completed.returncode != 0:
        raise PlatformGateError(
            f"command failed with exit {completed.returncode}: {' '.join(command)}"
        )


def _run_capture(command: list[str], cwd: Path = ROOT) -> str:
    completed = owned_command.run(command, cwd=cwd, timeout=owned_command.BUILD_SECONDS, text=True)
    sys.stdout.write(completed.stdout)
    sys.stderr.write(completed.stderr)
    if completed.returncode != 0:
        raise PlatformGateError(
            f"command failed with exit {completed.returncode}: {' '.join(command)}"
        )
    return completed.stdout


def _require_linux_consumer_output(output: str) -> None:
    """Check import/build smoke output; it supplies no SDK behaviour evidence."""
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if not lines or lines[-1] != LINUX_CONSUMER_OUTPUT:
        raise PlatformGateError("Linux consumer is missing all four public product import/build markers")


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
    completed = owned_command.run(
        ["xcrun", "simctl", "list", "devices", "available", "--json"],
        timeout=owned_command.INSPECTION_SECONDS,
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


def run_gate(gate: str, root: Path = ROOT) -> dict[str, Any]:
    contract = load_contract(root / "tools/platform-gates.json")
    # Reject unavailable lanes before any execution-bearing verification.
    if gate == "macos14":
        _require_native_darwin(contract["macos14"]["required_major"])
    elif gate == "ios17":
        _require_apple_silicon()
    elif gate == "linux-arm64":
        if platform.system() != "Linux" or platform.machine().lower() not in contract["linux_arm64"]["native_arches"]:
            raise PlatformGateError("Linux gate requires a native Linux ARM64 host")
    else:
        raise PlatformGateError(f"unknown gate: {gate}")
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
        return {"gate": gate, "passed": True}
    if gate == "ios17":
        _require_apple_silicon()
        if not shutil.which("xcodebuild"):
            raise PlatformGateError("xcodebuild is required")
        _require_ios_simulator(
            contract["ios17"]["runtime_version"],
            contract["ios17"]["simulator_name"],
        )
        with tempfile.TemporaryDirectory(prefix="teslatlas-ios17-gate-") as temporary:
            scratch = Path(temporary)
            _prepare_accepted_handoff(root, scratch / "handoff", contract)
            _prepare_ios_host(root, scratch, contract)
            _run(command_for(gate, root, scratch), root)
        return {"gate": gate, "passed": True, "source_package_identity_sha256": SOURCE_IDENTITY,
                "host_source_commit": SOURCE_COMMIT, "host_package_binding": "accepted handoff"}
    if gate == "linux-arm64":
        native_arches = contract["linux_arm64"]["native_arches"]
        if platform.system() != "Linux" or platform.machine().lower() not in native_arches:
            raise PlatformGateError("Linux gate requires a native Linux ARM64 host")
        if not shutil.which("docker"):
            raise PlatformGateError("docker is required")
        server = owned_command.run(
            ["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"],
            text=True,
            timeout=owned_command.INSPECTION_SECONDS,
        )
        if server.returncode != 0 or server.stdout.strip() not in {
            "linux/arm64",
            "linux/aarch64",
        }:
            raise PlatformGateError("Docker Engine must itself be native Linux ARM64")
        ownership = uuid.uuid4().hex
        image = "teslatlas-swift-platform-gate:local-arm64-" + ownership
        container = "teslatlas-swift-gate-" + ownership
        existing = owned_command.run(
            ["docker", "image", "inspect", image],
            timeout=owned_command.INSPECTION_SECONDS,
        )
        if existing.returncode == 0:
            raise PlatformGateError(f"owned Linux gate image tag already exists: {image}")
        result: dict[str, Any] | None = None
        try:
            with tempfile.TemporaryDirectory(prefix="teslatlas-linux-arm64-gate-") as temporary:
                handoff = Path(temporary) / "handoff"
                _prepare_accepted_handoff(root, handoff, contract)
                build_command = command_for(gate, root, handoff)
                build_command[build_command.index("--tag") + 1] = image
                _run(build_command, root)
                image_details = owned_command.run(
                    [
                        "docker",
                        "image",
                        "inspect",
                        "--format",
                        "{{.Id}}|{{.Os}}/{{.Architecture}}|{{.Config.User}}",
                        image,
                    ],
                    cwd=root,
                    text=True,
                    timeout=owned_command.INSPECTION_SECONDS,
                )
                fields = image_details.stdout.strip().split("|")
                if (
                    image_details.returncode != 0
                    or len(fields) != 3
                    or not fields[0].startswith("sha256:")
                    or len(fields[0]) != 71
                    or fields[1] not in {"linux/arm64", "linux/aarch64"}
                    or fields[2] != "swiftuser"
                ):
                    raise PlatformGateError("built Linux image identity is invalid")
                runtime_output = _run_capture(
                    [
                        "docker",
                        "run",
                        "--rm",
                        "--name",
                        container,
                        "--platform",
                        LINUX_PLATFORM,
                        image,
                    ],
                    root,
                )
                _require_linux_consumer_output(runtime_output)
                result = {
                    "gate": gate,
                    "passed": True,
                    "docker_server_platform": server.stdout.strip(),
                    "image_id": fields[0],
                    "owned_image_tag": image,
                    "owned_container_name": container,
                    "image_platform": fields[1],
                    "image_user": fields[2],
                    "swift_image_index_digest": contract["linux_arm64"][
                        "parent_index_digest"
                    ],
                    "swift_image_arm64_digest": contract["linux_arm64"][
                        "arm64_child_digest"
                    ],
                    "consumer_output": LINUX_CONSUMER_OUTPUT,
                }
        finally:
            original_error = sys.exception()
            try:
                _cleanup_linux(container, image, root)
            except BaseException as cleanup_error:
                if original_error is None:
                    raise
                original_error.add_note(f"owned Docker cleanup failed: {cleanup_error}")
        if result is None:
            raise PlatformGateError("Linux gate produced no result")
        result["cleanup"] = "owned image tag and temporary handoff removed"
        return result
    raise PlatformGateError(f"unknown gate: {gate}")


def _cleanup_linux(container: str, image: str, root: Path) -> None:
    errors = []
    # Cleanup has its own finite budgets. Docker daemon work is addressed by
    # the owned name; killing the docker CLI alone cannot settle it.
    for kind, name in (("container", container), ("image", image)):
        try:
            owned_command.run(["docker", kind, "rm", "--force", name], cwd=root,
                              timeout=owned_command.CLEANUP_SECONDS)
            selection = "name=^/" + name + "$" if kind == "container" else "reference=" + name
            query = ["docker", kind, "ls"] + (["--all"] if kind == "container" else [])
            query += ["--filter", selection, "--format", "{{.ID}}"]
            remaining = owned_command.run(query, cwd=root, text=True,
                                          timeout=owned_command.CLEANUP_SECONDS)
            # An inspect exit 1 also represents daemon failure. Only a
            # successful exact-name listing can establish absence.
            if remaining.returncode != 0:
                errors.append(f"owned {kind} absence could not be established")
            elif remaining.stdout.strip():
                errors.append(f"owned {kind} remains")
        except BaseException as error:
            errors.append(f"owned {kind} cleanup: {error}")
    if errors:
        raise PlatformGateError("; ".join(errors))


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
            output = run_gate(arguments.gate, ROOT)
    except (OSError, owned_command.CommandError, PlatformGateError, source_handoff.HandoffError) as error:
        raise SystemExit(f"platform gate failed: {error}") from error
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
