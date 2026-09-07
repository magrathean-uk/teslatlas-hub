# SPDX-License-Identifier: AGPL-3.0-only
"""Managed external target state; this module never starts services."""

from __future__ import annotations

import json
import os
import stat
import uuid
from pathlib import Path
from typing import Any, Iterable, Mapping

from .core import BootstrapError, active_release_id
from .recipes import RecipeContext, _edge_toolchain_override, _sha256


def target_binding(components: Iterable[str], context: RecipeContext) -> dict[str, str]:
    selected = set(components)
    binding: dict[str, str] = {}
    if "home-assistant" in selected:
        if context.ha_config is None:
            raise BootstrapError("an explicit HA config path is required")
        binding["ha_config"] = str(context.ha_config.resolve())
    if "edge" in selected:
        if context.edge_target is None:
            raise BootstrapError("an explicit Edge target is required")
        binding["edge_target"] = context.edge_target
        if context.edge_go_binary is not None or context.edge_tool_root is not None:
            go, root = _edge_toolchain_override(context)
            binding["edge_go_binary"] = str(go)
            binding["edge_go_binary_sha256"] = _sha256(go)
            binding["edge_tool_root"] = str(root)
    return binding


def _ha_payload(prefix: Path, release_id: str) -> Path:
    return (
        prefix
        / "releases"
        / release_id
        / "components"
        / "home-assistant"
        / "output"
        / "custom_components"
        / "teslatlas_hub"
    )


def _ha_target(config: str) -> Path:
    return Path(config) / "custom_components" / "teslatlas_hub"


def _is_owned_ha_target(prefix: Path, raw: str) -> bool:
    legacy = str(
        prefix
        / "active"
        / "components"
        / "home-assistant"
        / "output"
        / "custom_components"
        / "teslatlas_hub"
    )
    if raw == legacy:
        return True
    candidate = Path(raw)
    if not candidate.is_absolute():
        return False
    try:
        parts = candidate.relative_to(prefix / "releases").parts
    except ValueError:
        return False
    return len(parts) == 6 and parts[1:] == (
        "components",
        "home-assistant",
        "output",
        "custom_components",
        "teslatlas_hub",
    )


def _is_owned_ha_link(prefix: Path, path: Path) -> bool:
    return path.is_symlink() and _is_owned_ha_target(prefix, os.readlink(path))


def validate_targets(
    prefix: Path, components: Iterable[str], context: RecipeContext
) -> dict[str, str]:
    """Validate requested bindings without changing the target filesystem."""
    binding = target_binding(components, context)
    if "ha_config" not in binding:
        return binding
    config = Path(binding["ha_config"])
    if not config.is_absolute() or not config.is_dir():
        raise BootstrapError("HA config path must be an existing absolute directory")
    target = _ha_target(binding["ha_config"])
    if target.is_symlink():
        if not _is_owned_ha_link(prefix.resolve(), target):
            raise BootstrapError("HA integration link belongs to another installation")
    elif target.exists():
        raise BootstrapError(
            "HA integration already exists and will not be overwritten"
        )
    if target.parent.exists() and not target.parent.is_dir():
        raise BootstrapError("HA custom_components path is not a directory")
    return binding


def _snapshot(path: Path, *, marker: bool = False) -> dict[str, Any]:
    if path.is_symlink():
        result = {"path": str(path), "type": "symlink", "target": os.readlink(path)}
        if marker:
            result["managed"] = "marker"
        return result
    if not path.exists():
        result = {"path": str(path), "type": "missing"}
        if marker:
            result["managed"] = "marker"
        return result
    if marker and stat.S_ISREG(path.lstat().st_mode):
        return {
            "path": str(path),
            "type": "file",
            "content": path.read_text(),
            "managed": "marker",
        }
    raise BootstrapError(f"managed target path is occupied: {path}")


def prepare_target_transition(
    prefix: Path,
    release_id: str,
    components: Iterable[str],
    binding: Mapping[str, str],
    previous_receipt: Mapping[str, Any] | None,
) -> dict[str, Any]:
    """Build a serializable before/after plan for installer-owned target state."""
    prefix = prefix.resolve()
    selected = set(components)
    previous_targets = (
        previous_receipt.get("targets", {})
        if isinstance(previous_receipt, dict)
        else {}
    )
    configs = {
        value
        for value in (previous_targets.get("ha_config"), binding.get("ha_config"))
        if isinstance(value, str)
    }
    before_entries: list[dict[str, Any]] = []
    after_entries: list[dict[str, Any]] = []
    for config in sorted(configs):
        path = _ha_target(config)
        before = _snapshot(path)
        before["managed"] = "ha-link"
        if before["type"] == "symlink" and not _is_owned_ha_link(prefix, path):
            raise BootstrapError("HA integration link belongs to another installation")
        before_entries.append(before)
        if "home-assistant" in selected and config == binding.get("ha_config"):
            payload = _ha_payload(prefix, release_id)
            if not payload.is_dir():
                raise BootstrapError("selected release has no Home Assistant payload")
            after_entries.append(
                {
                    "path": str(path),
                    "type": "symlink",
                    "target": str(payload),
                    "managed": "ha-link",
                }
            )
        else:
            after_entries.append(
                {"path": str(path), "type": "missing", "managed": "ha-link"}
            )

    actions: dict[str, Any] = {}
    if "home-assistant" in selected:
        path = _ha_target(binding["ha_config"])
        actions["home-assistant"] = {
            "status": "linked",
            "path": str(path),
            "restart_required": True,
        }
    if "edge" in selected:
        output = prefix / "releases" / release_id / "components" / "edge" / "output"
        if not output.is_dir() or not any(output.iterdir()):
            raise BootstrapError("selected release has no Edge payload")
        actions["edge"] = {
            "status": "prepared",
            "path": str(output),
            "service_install_required": True,
            "service_started": False,
        }
    marker = prefix / "releases" / release_id / "external-actions.json"
    before_entries.append(_snapshot(marker, marker=True))
    after_entries.append(
        {
            "path": str(marker),
            "type": "file",
            "managed": "marker",
            "content": json.dumps(
                {"schema_version": 2, "actions": actions}, indent=2, sort_keys=True
            )
            + "\n",
        }
    )
    return {"before": before_entries, "after": after_entries, "actions": actions}


def apply_external_state(
    entries: Iterable[Mapping[str, Any]], prefix: Path | None = None
) -> None:
    """Apply only states emitted by prepare_target_transition."""
    canonical_prefix = prefix.resolve() if prefix is not None else None
    validated = validate_external_state(entries, prefix)
    for entry in validated:
        path = Path(entry["path"])
        state = entry["type"]
        managed = entry.get("managed")
        if managed not in {"ha-link", "marker"}:
            raise BootstrapError("managed target journal entry is invalid")
        if state not in {"missing", "symlink", "file"}:
            raise BootstrapError("managed target journal state is invalid")
        if managed == "ha-link":
            if state == "file" or (
                state == "symlink"
                and (
                    canonical_prefix is None
                    or not _is_owned_ha_target(
                        canonical_prefix, entry.get("target", "")
                    )
                )
            ):
                raise BootstrapError("managed HA journal state is invalid")
        elif state == "symlink":
            raise BootstrapError("managed marker journal state is invalid")
        if managed == "marker":
            if canonical_prefix is None:
                raise BootstrapError("managed marker requires its installation prefix")
            try:
                relative = path.relative_to(canonical_prefix / "releases")
            except ValueError as error:
                raise BootstrapError("managed marker is outside its prefix") from error
            if len(relative.parts) != 2 or relative.parts[1] != "external-actions.json":
                raise BootstrapError("managed marker path is invalid")
        if path.is_symlink():
            if (
                managed == "ha-link"
                and canonical_prefix is not None
                and not _is_owned_ha_link(canonical_prefix, path)
            ):
                raise BootstrapError(
                    "managed HA link changed ownership during transition"
                )
            path.unlink()
        elif path.exists():
            if managed == "marker" and path.is_file():
                path.unlink()
            else:
                raise BootstrapError(
                    f"managed target path changed unexpectedly: {path}"
                )
        if state == "missing":
            if path.parent.is_dir():
                descriptor = os.open(path.parent, os.O_RDONLY)
                try:
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
        if state == "symlink":
            temporary.symlink_to(entry["target"])
        elif state == "file":
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                stream.write(entry["content"])
                stream.flush()
                os.fsync(stream.fileno())
        else:
            raise BootstrapError("managed target journal state is invalid")
        os.replace(temporary, path)
        descriptor = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def validate_external_state(
    entries: Iterable[Mapping[str, Any]], prefix: Path | None = None
) -> list[dict[str, Any]]:
    """Validate a complete journal target payload without changing the filesystem."""
    canonical_prefix = prefix.resolve() if prefix is not None else None
    validated: list[dict[str, Any]] = []
    seen: set[Path] = set()
    for raw in entries:
        if not isinstance(raw, Mapping):
            raise BootstrapError("managed target journal entry is invalid")
        entry = dict(raw)
        path_value = entry.get("path")
        state = entry.get("type")
        managed = entry.get("managed")
        if not isinstance(path_value, str) or not Path(path_value).is_absolute():
            raise BootstrapError("managed target journal path is invalid")
        path = Path(path_value)
        if path in seen:
            raise BootstrapError("managed target journal contains duplicate paths")
        seen.add(path)
        if managed not in {"ha-link", "marker"}:
            raise BootstrapError("managed target journal entry is invalid")
        if state not in {"missing", "symlink", "file"}:
            raise BootstrapError("managed target journal state is invalid")
        expected_fields = {"path", "type", "managed"}
        if state == "symlink":
            expected_fields.add("target")
            if not isinstance(entry.get("target"), str):
                raise BootstrapError("managed target journal link is invalid")
        elif state == "file":
            expected_fields.add("content")
            if not isinstance(entry.get("content"), str):
                raise BootstrapError("managed target journal file is invalid")
        if set(entry) != expected_fields:
            raise BootstrapError("managed target journal fields are invalid")
        if managed == "ha-link":
            if state == "file" or (
                state == "symlink"
                and (
                    canonical_prefix is None
                    or not _is_owned_ha_target(canonical_prefix, entry["target"])
                )
            ):
                raise BootstrapError("managed HA journal state is invalid")
            if path.is_symlink():
                if canonical_prefix is None or not _is_owned_ha_link(
                    canonical_prefix, path
                ):
                    raise BootstrapError(
                        "managed HA link changed ownership during transition"
                    )
            elif path.exists():
                raise BootstrapError(f"managed target path is occupied: {path}")
            if path.parent.exists() and not path.parent.is_dir():
                raise BootstrapError("HA custom_components path is not a directory")
        else:
            if state == "symlink":
                raise BootstrapError("managed marker journal state is invalid")
            if canonical_prefix is None:
                raise BootstrapError("managed marker requires its installation prefix")
            try:
                relative = path.relative_to(canonical_prefix / "releases")
            except ValueError as error:
                raise BootstrapError("managed marker is outside its prefix") from error
            if len(relative.parts) != 2 or relative.parts[1] != "external-actions.json":
                raise BootstrapError("managed marker path is invalid")
            if path.is_symlink() or (path.exists() and not path.is_file()):
                raise BootstrapError(f"managed target path is occupied: {path}")
            if state == "file":
                try:
                    marker = json.loads(entry["content"])
                except json.JSONDecodeError as error:
                    raise BootstrapError("managed marker content is invalid") from error
                if (
                    not isinstance(marker, dict)
                    or set(marker) != {"schema_version", "actions"}
                    or marker["schema_version"] != 2
                    or not isinstance(marker["actions"], dict)
                ):
                    raise BootstrapError("managed marker content is invalid")
        validated.append(entry)
    return validated


def validate_recovery_target_plan(
    prefix: Path,
    active: str | None,
    receipt: Mapping[str, Any] | None,
    entries: Iterable[Mapping[str, Any]],
    *,
    restoring_before: bool,
) -> list[dict[str, Any]]:
    """Bind committed links strictly while allowing a captured missing before-state."""
    validated = validate_external_state(entries, prefix)
    desired_links = [
        entry
        for entry in validated
        if entry["managed"] == "ha-link" and entry["type"] == "symlink"
    ]
    if active is None:
        if desired_links:
            raise BootstrapError("recovery target plan has no active release")
        return validated
    if not isinstance(receipt, Mapping):
        raise BootstrapError("recovery target plan has no verified receipt")
    components = receipt.get("components")
    targets = receipt.get("targets")
    if not isinstance(components, Mapping) or not isinstance(targets, Mapping):
        raise BootstrapError("recovery receipt target binding is invalid")
    if "home-assistant" not in components:
        if desired_links:
            raise BootstrapError("recovery target plan mixes component sets")
        return validated
    config = targets.get("ha_config")
    if not isinstance(config, str) or not Path(config).is_absolute():
        raise BootstrapError("recovery HA target binding is invalid")
    expected_path = str(_ha_target(config))
    expected_target = str(_ha_payload(prefix.resolve(), active))
    expected_link = {
        "path": expected_path,
        "type": "symlink",
        "target": expected_target,
        "managed": "ha-link",
    }
    expected_missing = {
        "path": expected_path,
        "type": "missing",
        "managed": "ha-link",
    }
    configured_entries = [
        entry
        for entry in validated
        if entry["managed"] == "ha-link" and entry["path"] == expected_path
    ]
    allowed_states = (
        (expected_missing, expected_link) if restoring_before else (expected_link,)
    )
    if (
        len(configured_entries) != 1
        or configured_entries[0] not in allowed_states
        or any(entry != expected_link for entry in desired_links)
    ):
        raise BootstrapError("recovery HA target plan does not match its receipt")
    return validated


def activate_targets(
    prefix: Path, components: Iterable[str], context: RecipeContext
) -> dict[str, Any]:
    """Repair targets through the same locked, journaled transition."""
    from .core import (
        PrefixLock,
        _commit_transition,
        _history,
        _recover_transaction,
        _verify_release,
    )

    with PrefixLock(prefix):
        _recover_transaction(prefix)
        current = active_release_id(prefix)
        if current is None:
            raise BootstrapError("cannot activate targets without an active release")
        receipt = _verify_release(prefix, current)
        binding = target_binding(components, context)
        plan = prepare_target_transition(prefix, current, components, binding, receipt)
        _commit_transition(prefix, current, _history(prefix), plan)
        return plan["actions"]
