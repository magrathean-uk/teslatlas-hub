# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed reviewed build recipes; catalog data cannot replace these commands."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

from .processes import ProcessFailure, run_process

RECIPE_REVISION = 3
HUB_PROFILE_SHA256 = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224"
SDK_TARBALL_SHA256 = "42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7"

KNOWN_REPOSITORIES = {
    "protocol": "https://github.com/magrathean-uk/teslatlas-protocol.git",
    "sdk-typescript": "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
    "sdk-swift": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
    "home-assistant": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
    "edge": "https://github.com/magrathean-uk/teslatlas-edge.git",
}

EXPECTED_PROFILES = {
    name: {
        "id": "hub-http-v1",
        "revision": "1.0.0",
        "sha256": HUB_PROFILE_SHA256,
    }
    for name in (
        "protocol",
        "sdk-typescript",
        "sdk-swift",
        "home-assistant",
    )
}
EXPECTED_PROFILES["edge"] = {
    "id": "edge-delivery-v2",
    "revision": "2.0.0",
    "sha256": EDGE_PROFILE_SHA256,
}


@dataclass(frozen=True)
class RecipeContext:
    ha_config: Optional[Path] = None
    edge_target: Optional[str] = None
    edge_go_binary: Optional[Path] = None
    edge_tool_root: Optional[Path] = None
    node_bin: Optional[Path] = None
    timeout_seconds: int = 300


def _bootstrap_error(message: str) -> RuntimeError:
    from .core import BootstrapError

    return BootstrapError(message)


def _tool(name: str, context: RecipeContext) -> str:
    if name in {"node", "npm"} and context.node_bin is not None:
        candidate = context.node_bin / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
        raise _bootstrap_error(f"required tool {name} is missing from --node-bin")
    if name == "go" and context.edge_go_binary is not None:
        go, _root = _edge_toolchain_override(context)
        return str(go)
    found = shutil.which(name)
    if found is None:
        raise _bootstrap_error(f"required tool {name} is missing")
    return found


def _edge_toolchain_override(context: RecipeContext) -> tuple[Path, Path]:
    go = context.edge_go_binary
    root = context.edge_tool_root
    if (go is None) != (root is None):
        raise _bootstrap_error(
            "--edge-go-binary and --edge-tool-root must be supplied together"
        )
    if go is None or root is None:
        raise _bootstrap_error("an explicit Edge Go toolchain override is missing")
    if not go.is_absolute() or not root.is_absolute():
        raise _bootstrap_error("Edge Go toolchain paths must be absolute")
    if root.is_symlink() or not root.is_dir():
        raise _bootstrap_error("Edge Go tool root is missing or unsafe")
    if go.is_symlink() or not go.is_file() or not os.access(go, os.X_OK):
        raise _bootstrap_error("Edge Go binary is missing or unsafe")
    canonical_root = root.resolve(strict=True)
    canonical_go = go.resolve(strict=True)
    if canonical_root != root or canonical_go != go:
        raise _bootstrap_error("Edge Go toolchain paths must be canonical")
    try:
        canonical_go.relative_to(canonical_root)
    except ValueError as error:
        raise _bootstrap_error("Edge Go binary is outside its tool root") from error
    return canonical_go, canonical_root


def _recipe_environment(context: RecipeContext) -> dict[str, str]:
    environment = dict(
        os.environ,
        GIT_TERMINAL_PROMPT="0",
        npm_config_audit="false",
        npm_config_fund="false",
    )
    if context.node_bin is not None:
        environment["PATH"] = (
            str(context.node_bin) + os.pathsep + environment.get("PATH", "")
        )
    if context.edge_go_binary is not None or context.edge_tool_root is not None:
        go, root = _edge_toolchain_override(context)
        environment["GO_BINARY"] = str(go)
        environment["RUNNER_TOOL_CACHE"] = str(root)
    return environment


def _version(command: list[str], context: RecipeContext) -> str:
    try:
        result = run_process(
            command,
            cwd=Path.cwd(),
            env=_recipe_environment(context),
            timeout_seconds=15,
            stderr=subprocess.STDOUT,
        )
        return (result.stdout or b"").decode().strip()
    except ProcessFailure as error:
        raise _bootstrap_error(f"tool version check failed: {command[0]}") from error


def check_recipe_environment(name: str, context: RecipeContext) -> dict[str, str]:
    """Fail before source execution when the selected target cannot run a recipe."""
    if name == "protocol":
        uv = _tool("uv", context)
        return {"uv": _version([uv, "--version"], context)}
    if name == "sdk-typescript":
        node = _tool("node", context)
        npm = _tool("npm", context)
        node_version = _version([node, "--version"], context).lstrip("v")
        npm_version = _version([npm, "--version"], context)
        if node_version != "26.7.0" or npm_version != "11.19.0":
            raise _bootstrap_error("Node 26.7.0 and npm 11.19.0 are required")
        return {"node": node_version, "npm": npm_version}
    if name == "sdk-swift":
        swift = _tool("swift", context)
        if platform.system() not in {"Darwin", "Linux"}:
            raise _bootstrap_error("Swift recipe supports only macOS or Linux")
        if platform.system() == "Linux":
            pkg_config = _tool("pkg-config", context)
            for package in ("libcurl", "openssl"):
                try:
                    run_process(
                        [pkg_config, "--exists", package],
                        cwd=Path.cwd(),
                        timeout_seconds=15,
                    )
                except ProcessFailure as error:
                    raise _bootstrap_error(
                        "Linux Swift requires libcurl and OpenSSL development files"
                    ) from error
        return {"swift": _version([swift, "--version"], context).splitlines()[0]}
    if name == "home-assistant":
        if context.ha_config is None:
            raise _bootstrap_error("an explicit HA config path is required")
        if not context.ha_config.is_absolute() or not context.ha_config.is_dir():
            raise _bootstrap_error(
                "HA config path must be an existing absolute directory"
            )
        python = _tool("python3", context)
        version = _version(
            [python, "-c", "import platform; print(platform.python_version())"], context
        )
        if tuple(int(item) for item in version.split(".")) < (3, 14, 2):
            raise _bootstrap_error(
                "Home Assistant recipe requires Python 3.14.2 or newer"
            )
        return {"python": version}
    if name == "edge":
        if context.edge_target != "local-linux" or platform.system() != "Linux":
            raise _bootstrap_error("Edge requires an explicit local Linux target")
        cargo = _tool("cargo", context)
        rustc = _tool("rustc", context)
        go = _tool("go", context)
        cargo_version = _version([cargo, "--version"], context)
        rust_version = _version([rustc, "--version"], context)
        go_version = _version([go, "version"], context)
        if not rust_version.startswith("rustc 1.98.0 ") or " go1.27.0 " not in (
            f" {go_version} "
        ):
            raise _bootstrap_error("Edge requires Rust 1.98.0 and Go 1.27.0")
        result = {"cargo": cargo_version, "rustc": rust_version, "go": go_version}
        if context.edge_go_binary is not None:
            binary, root = _edge_toolchain_override(context)
            result.update(
                {
                    "go_binary": str(binary),
                    "go_binary_sha256": _sha256(binary),
                    "go_tool_root": str(root),
                }
            )
        return result
    raise _bootstrap_error(f"unknown component recipe {name}")


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_component_set(components: Any) -> None:
    """Reject every deferred or unknown component before a recipe runs."""
    selected = set(components)
    unknown = selected - set(KNOWN_REPOSITORIES)
    if unknown:
        raise _bootstrap_error(f"unknown components: {sorted(unknown)}")


def validate_publication_status(source_status: Any, publication_status: str) -> None:
    """Prevent a moving catalog from promoting or demoting source metadata."""
    expected = "published" if publication_status == "published" else "candidate"
    if source_status != expected:
        raise _bootstrap_error(
            f"source status {source_status!r} does not match {expected} cohort status"
        )


def _artifacts(component: Any) -> dict[str, str]:
    value = getattr(component, "artifacts", None)
    if not isinstance(value, dict):
        value = dict(value or {})
    return value


def verify_built_output(
    name: str, source: Path, output: Path, component: Any
) -> dict[str, str]:
    """Admit only the accepted TypeScript SDK package bytes."""
    if name != "sdk-typescript":
        return {}
    packed = sorted(output.glob("*.tgz"))
    if len(packed) != 1:
        raise _bootstrap_error(f"{name} recipe did not produce exactly one package")
    artifacts = _artifacts(component)
    expected = artifacts["package_sha256"]
    if packed[0].name != artifacts["package_filename"]:
        raise _bootstrap_error(f"{name} recipe produced the wrong package filename")
    observed = _sha256(packed[0])
    if observed != expected:
        raise _bootstrap_error(
            f"{name} build does not match the accepted package: "
            f"observed sha256 {observed}, expected {expected}"
        )
    return {"package_sha256": observed}


def _product_version(name: str, source: Path) -> str:
    if name == "sdk-typescript":
        return json.loads((source / "package.json").read_text(encoding="utf-8"))[
            "version"
        ]
    if name in {"protocol", "home-assistant"}:
        text = (source / "pyproject.toml").read_text(encoding="utf-8")
        match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
        if match is None:
            raise _bootstrap_error("pyproject product version is missing")
        return match.group(1)
    compatibility = json.loads(
        (source / "compatibility" / "hub.json").read_text(encoding="utf-8")
    )
    return compatibility["product_version"]


def validate_source_metadata(
    name: str,
    source: Path,
    component: Any,
    publication_status: Optional[str] = None,
) -> dict[str, str]:
    """Bind product/profile metadata and dependency locks before commands run."""
    try:
        compatibility = json.loads(
            (source / "compatibility" / "hub.json").read_text(encoding="utf-8")
        )
    except (OSError, json.JSONDecodeError) as error:
        raise _bootstrap_error(
            f"{name} compatibility metadata is unreadable"
        ) from error
    if _product_version(name, source) != component.product_version:
        raise _bootstrap_error(f"{name} product version does not match the cohort")
    if publication_status is not None:
        validate_publication_status(compatibility.get("status"), publication_status)
    if compatibility.get("product_version") != component.product_version:
        raise _bootstrap_error(f"{name} compatibility product version mismatch")
    profile = compatibility.get("profile")
    expected = {
        "id": component.profile.id,
        "revision": component.profile.revision,
        "sha256": component.profile.sha256,
    }
    if (
        name == "protocol"
        and isinstance(profile, dict)
        and profile.get("sha256") is None
    ):
        manifest = source / "profiles" / "hub-http-v1" / "1.0.0" / "SHA256SUMS"
        profile = dict(profile, sha256=_sha256(manifest))
    if profile != expected:
        raise _bootstrap_error(f"{name} profile metadata mismatch")
    lock_paths = {
        "protocol": ("uv.lock",),
        "sdk-typescript": ("package-lock.json",),
        "sdk-swift": (),
        "home-assistant": ("uv.lock",),
        "edge": (
            "Cargo.lock",
            "packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json",
            "packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch",
        ),
    }[name]
    dependencies = {}
    for relative in lock_paths:
        path = source / relative
        if not path.is_file():
            raise _bootstrap_error(f"{name} dependency lock is missing: {relative}")
        dependencies[relative] = _sha256(path)
    return dependencies


def _run(
    command: list[str], source: Path, log: Any, context: RecipeContext
) -> dict[str, Any]:
    encoded = json.dumps(command, separators=(",", ":")).encode()
    command_hash = hashlib.sha256(encoded).hexdigest()
    environment = _recipe_environment(context)
    try:
        run_process(
            command,
            cwd=source,
            env=environment,
            timeout_seconds=context.timeout_seconds,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
    except ProcessFailure as error:
        if "timed out" in str(error):
            raise _bootstrap_error(
                f"fixed recipe command timed out: {command[0]}"
            ) from error
        raise _bootstrap_error(f"fixed recipe command failed: {command[0]}") from error
    return {"sha256": command_hash, "argv": command}


def fixed_commands(
    name: str,
    _source: Path,
    output: Path,
    context: RecipeContext,
    component: Any = None,
) -> list[list[str]]:
    """Return the reviewed command recipe; catalog data never reaches this list."""
    if name == "protocol":
        uv = _tool("uv", context)
        return [
            [uv, "sync", "--locked"],
            [
                uv,
                "run",
                "--locked",
                "python",
                "tools/build_hub_http_profile.py",
                "--check",
            ],
        ]
    if name == "sdk-typescript":
        npm = _tool("npm", context)
        node = _tool("node", context)
        package = output / _artifacts(component)["package_filename"]
        smoke = output / ".sdk-package-smoke"
        entrypoint = smoke / "node_modules" / "@teslatlas" / "sdk" / "dist" / "index.js"
        return [
            [npm, "ci"],
            [npm, "run", "build"],
            [npm, "pack", "--pack-destination", str(output)],
            [
                npm,
                "install",
                "--prefix",
                str(smoke),
                "--ignore-scripts",
                "--package-lock=false",
                "--no-save",
                str(package),
            ],
            [node, "-e", f"import({entrypoint.as_uri()!r})"],
        ]
    if name == "sdk-swift":
        return [[_tool("swift", context), "build", "-c", "release"]]
    if name == "home-assistant":
        return [
            [
                _tool("python3", context),
                "-m",
                "compileall",
                "-q",
                "custom_components/teslatlas_hub",
            ]
        ]
    if name == "edge":
        if platform.system() != "Linux":
            raise _bootstrap_error("Edge requires a Linux build host")
        architecture = {
            "x86_64": "linux-amd64",
            "amd64": "linux-amd64",
            "aarch64": "linux-arm64",
            "arm64": "linux-arm64",
        }.get(platform.machine().lower())
        if architecture is None:
            raise _bootstrap_error("Edge build host architecture is unsupported")
        return [
            [_tool("cargo", context), "build", "--locked", "--release"],
            ["./target/release/teslatlas-edge", "--help"],
            [
                "./scripts/build-fleet-telemetry-bridge.sh",
                "--target",
                architecture,
                "--output",
                str(output / "fleet-telemetry-bridge"),
            ],
        ]
    raise _bootstrap_error(f"unknown component recipe {name}")


def build_component(
    name: str,
    source: Path,
    output: Path,
    component: Any,
    context: RecipeContext,
    publication_status: Optional[str] = None,
) -> dict[str, Any]:
    """Run one fixed recipe and place only its selected install output."""
    tools = check_recipe_environment(name, context)
    dependencies = validate_source_metadata(name, source, component, publication_status)
    output.mkdir(parents=True, mode=0o700)
    log_path = output.parent / "build.log"
    fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    commands = []
    with os.fdopen(fd, "wb") as log:
        for command in fixed_commands(name, source, output, context, component):
            commands.append(_run(command, source, log, context))
    if name == "sdk-typescript":
        shutil.rmtree(output / ".sdk-package-smoke")
    if name == "protocol":
        (output / "source-path.txt").write_text("../source\n", encoding="utf-8")
    elif name == "sdk-swift":
        release_output = source / ".build" / "release"
        try:
            canonical = release_output.resolve(strict=True)
            canonical.relative_to((source / ".build").resolve(strict=True))
        except (OSError, ValueError) as error:
            raise _bootstrap_error(
                "Swift recipe produced no safe release output"
            ) from error
        if not canonical.is_dir() or not any(canonical.iterdir()):
            raise _bootstrap_error("Swift recipe produced no release output")
        shutil.copytree(canonical, output / "release")
        (output / "source-path.txt").write_text("../source\n", encoding="utf-8")
    elif name == "home-assistant":
        target = output / "custom_components" / "teslatlas_hub"
        shutil.copytree(
            source / "custom_components" / "teslatlas_hub",
            target,
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
    elif name == "edge":
        edge_binary = source / "target" / "release" / "teslatlas-edge"
        if not edge_binary.is_file():
            raise _bootstrap_error("Edge recipe produced no Edge binary")
        shutil.copy2(edge_binary, output / edge_binary.name)
        support = output / "support"
        support.mkdir()
        for relative in (
            "packaging/config.toml.example",
            "packaging/fleet-telemetry.json.example",
            "packaging/linux/teslatlas-edge.service",
            "packaging/linux/teslatlas-fleet-telemetry.service",
            "scripts/run-with-spool-format-guard.sh",
        ):
            source_file = source / relative
            if not source_file.is_file():
                raise _bootstrap_error(f"Edge support file is missing: {relative}")
            destination = support / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source_file, destination)
    verification = verify_built_output(name, source, output, component)
    return {
        "commands": commands,
        "dependencies": dependencies,
        "tools": tools,
        "verification": verification,
    }
