# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed Home Assistant installed-adapter seam.

The reviewed sibling checkout supplies contract and launcher bytes. Runtime JSON
may select evidence inputs, but it cannot select code, validators, argv, broker
paths, or output locations. The launch hook remains fail-closed until a reviewed
installed-container registration exists.
"""

from __future__ import annotations

import hashlib
import json
import os
import ssl
import stat
import tarfile
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    read_bound_file,
    strict_json,
)

WORKSPACE = Path(__file__).resolve().parents[4]
HA_ROOT = WORKSPACE / "teslatlas-home-assistant"
COMPONENT_ROOT = HA_ROOT / "custom_components" / "teslatlas_hub"


@dataclass(frozen=True, slots=True)
class ReviewedFile:
    path: Path
    sha256: str


HANDOFF = ReviewedFile(
    HA_ROOT / "docs/hub-admission-handoff-2026-09-08.json",
    "6ab5147162a34c61dd8f1fb38361b2dd7db2dc4684864b1c3ac6a3c1076413a6",
)
SOURCE_MANIFEST = ReviewedFile(
    HA_ROOT / "docs/ha-admission-source-manifest-2026-09-08.json",
    "e74a955374003be9820a450174715412b4a9c1d504b47eb33d1f93e6b2173832",
)

CONTRACT = ReviewedContract(
    str(HA_ROOT / "tools/matrix-contract.json"),
    "6b3f8a19bba579790bd554a589a82f273facdbdf5515feba2c83b9eda2091820",
    str(HA_ROOT / "tools/matrix_contract.py"),
    "aa0c27880a59855e110a8a43c540c1e6127e449b63e1f834140c8c681014185f",
)

RAW_SCHEMAS = MappingProxyType(
    {
        "ha-runtime-v1": (
            HA_ROOT / "tools/ha-runtime-v1.schema.json",
            "a49e3212ac07a64c8f27de6baa06418013f4ef0f5a3515436973c9969095f31c",
        ),
        "ha-initial-v1": (
            HA_ROOT / "tools/ha-initial-v1.schema.json",
            "5e3525d5523e4162320bfdc2cdd6fc13f0203187d3fe2e141b9cdfe33f37f8e6",
        ),
        "ha-client-v1": (
            HA_ROOT / "tools/ha-client-v1.schema.json",
            "dcf05f837e37fc6541d8d6d0e3c5ce068a0b0eb662d8e6b00cb723171b9b3b92",
        ),
        "ha-flow-v1": (
            HA_ROOT / "tools/ha-flow-v1.schema.json",
            "dd4bd1c39e00e35b11550c20a5f660489cfcd93c15269e3b44799101e84ea189",
        ),
    }
)

REVIEWED_SOURCES = MappingProxyType(
    {
        "tests/integration/test_live_hub.py": "d42efba1341dfe3ae9db2f974189aa97e812caa58663b4d52de450010accf728",
        "tests/test_matrix_live.py": "5935c634766b397162fccfa74348a535bbcb8b70c69bb66223bd9e69f5cce108",
        "tools/ha-client-v1.schema.json": "dcf05f837e37fc6541d8d6d0e3c5ce068a0b0eb662d8e6b00cb723171b9b3b92",
        "tools/ha-flow-v1.schema.json": "dd4bd1c39e00e35b11550c20a5f660489cfcd93c15269e3b44799101e84ea189",
        "tools/ha-initial-v1.schema.json": "5e3525d5523e4162320bfdc2cdd6fc13f0203187d3fe2e141b9cdfe33f37f8e6",
        "tools/ha-runtime-v1.schema.json": "a49e3212ac07a64c8f27de6baa06418013f4ef0f5a3515436973c9969095f31c",
        "tools/matrix-contract.json": "6b3f8a19bba579790bd554a589a82f273facdbdf5515feba2c83b9eda2091820",
        "tools/matrix_contract.py": "aa0c27880a59855e110a8a43c540c1e6127e449b63e1f834140c8c681014185f",
        "tools/matrix_live.py": "cb9dfad5ffe5c86ec2c33f5160bbdd81593262f9f9985ace01d83463f9645b55",
        "tools/matrix_wire.py": "cd7f4a620f5aaab46bd4e311f7e350063d5f6db39da72e5b0e8129f46e8d1085",
    }
)
COMPONENT_ROWS = (
    {
        "path": "__init__.py",
        "bytes": 3101,
        "mode": 420,
        "sha256": "326c8243acb150e5eb557670527be2054348596e659bd1cb81b0e4604c5db5ad",
    },
    {
        "path": "client.py",
        "bytes": 3557,
        "mode": 420,
        "sha256": "bcc65d205124b65ba22ca51b1ea3c0e8009648af6aa1f66dbf01034d53058b82",
    },
    {
        "path": "config_flow.py",
        "bytes": 12540,
        "mode": 420,
        "sha256": "96a78e8563dd49ca914ebd67d77089b7aca52b5f5db2798b9c33e38e6fd80208",
    },
    {
        "path": "const.py",
        "bytes": 702,
        "mode": 420,
        "sha256": "05ed51cd8081eb53062e5156bfbcaa419a138625b342f4b14bb1aab5e635229c",
    },
    {
        "path": "coordinator.py",
        "bytes": 2780,
        "mode": 420,
        "sha256": "64aa33b34c23437fa0efdaf3d1355e368a4cf88e6ca349b11ec40cd8628b0858",
    },
    {
        "path": "current_hub_client.py",
        "bytes": 18349,
        "mode": 420,
        "sha256": "4b60538e8c40ab0c1f764e0cd20c1c991cb625c254115cfdd07f333652115d53",
    },
    {
        "path": "diagnostics.py",
        "bytes": 1955,
        "mode": 420,
        "sha256": "ef779a20632b3a04abaf5d088f41601f1bc47062ff6a09bf2a320331d7716657",
    },
    {
        "path": "entity.py",
        "bytes": 385,
        "mode": 420,
        "sha256": "0567abb72c9d4c41a31f8336d475288eaadcbca41094bd2cbcffcdc905e837ff",
    },
    {
        "path": "manifest.json",
        "bytes": 391,
        "mode": 420,
        "sha256": "61e4a3143a392fe681908d1edff9db52a25ce5ac4bbd12e327ab36bcd9140449",
    },
    {
        "path": "models.py",
        "bytes": 3439,
        "mode": 420,
        "sha256": "7772c2751eb74d4a012d9e523d77c75248ef99f2d282544ac41682125e3c2fe6",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/SHA256SUMS",
        "bytes": 1457,
        "mode": 420,
        "sha256": "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/auth.schema.json",
        "bytes": 2183,
        "mode": 420,
        "sha256": "27891c9cd96b57b5fa47db36b0d7caf22600b172a9150dcdaf735c64a8297b19",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/cases.json",
        "bytes": 3380,
        "mode": 420,
        "sha256": "7743aa276b4c027abbd357304bfa065db79edec20cafac59b4311364e0d28201",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/discovery.schema.json",
        "bytes": 1332,
        "mode": 420,
        "sha256": "3d45864e4296fdc14b0a3ef0ef5acdbfcca94cacda1ec60fccca396271851e18",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/errors.schema.json",
        "bytes": 737,
        "mode": 420,
        "sha256": "835fe4a83f3c894bc706e5f575e4abd5d473c4b8406fc0d9b0cbf0e95172a5b4",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/claim.json",
        "bytes": 178,
        "mode": 420,
        "sha256": "8eb1ac46f72762321575d457d6bbd1ed7b1def904de9c07448b67d0659b15e1d",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/current.json",
        "bytes": 2407,
        "mode": 420,
        "sha256": "2246f7a4b98e559c4040877ecf9474d081df5f7ab8fdfb9bcde2a3fbceacf911",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/discovery.json",
        "bytes": 357,
        "mode": 420,
        "sha256": "e673ea82496110f6cbbe1c108b40eb098418bb95d781723753bdff4792c84136",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/drives.json",
        "bytes": 890,
        "mode": 420,
        "sha256": "fd326bd9476115bba05bbbeb61d40361e16d164984de7817e5cba58b7d29954f",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/health.json",
        "bytes": 47,
        "mode": 420,
        "sha256": "27cd5e17b843fd061cdec83140a87f30365f09f1d27704aa5ae98c58d303347b",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/invitation.json",
        "bytes": 563,
        "mode": 420,
        "sha256": "9ecf1cf49edac1fed91a0e92729d8d77c4ff13634f212555973a7003f1d4e926",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/ready.json",
        "bytes": 24,
        "mode": 420,
        "sha256": "9994e13386b29bde4071dd062d8c4ab8dbb6b4b877f968b4a34fb2c7b0ead65e",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/examples/vehicles.json",
        "bytes": 123,
        "mode": 420,
        "sha256": "36b77389bac75fb247c6e0da2baf284511f40a808227b38d7543b2d199437150",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/field-semantics.json",
        "bytes": 18972,
        "mode": 420,
        "sha256": "bd9b7162c94efbd3e570ffd785fe543dfb98299379ea81cf8b2e028496bd3ee7",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/openapi.json",
        "bytes": 11524,
        "mode": 420,
        "sha256": "af46fba327341fa016c1b25e3f45d69a56f75740f5d0feda3714a89b3c3e9bf7",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/profile.json",
        "bytes": 4543,
        "mode": 420,
        "sha256": "5b939b06ffb9354cc5227734b85a3f2d7edf46d509dcdda603bccd961b839d71",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/resources.schema.json",
        "bytes": 44373,
        "mode": 420,
        "sha256": "6b2523de42f5860ddaaf83b0db78b6f172e3f15e1dab8048e221c1fa6337a4be",
    },
    {
        "path": "profile/hub-http-v1/1.0.0/sync-regression.json",
        "bytes": 1427,
        "mode": 420,
        "sha256": "6aebb1a5d5e250447435876d4cb64934b916e8cd61c2c8b148d2111c7f5fc691",
    },
    {
        "path": "sensor.py",
        "bytes": 9029,
        "mode": 420,
        "sha256": "7459e21bee5811307648f7fc1fcc4d516218e971e3f2cbe3e71a4d94e1e21724",
    },
    {
        "path": "strings.json",
        "bytes": 3356,
        "mode": 420,
        "sha256": "e29e2c01759348c66c9f91cd5ad1d97080f6508fdc11fd714a48ef8c9da29bfe",
    },
    {
        "path": "translations/en.json",
        "bytes": 3356,
        "mode": 420,
        "sha256": "e29e2c01759348c66c9f91cd5ad1d97080f6508fdc11fd714a48ef8c9da29bfe",
    },
)
COMPONENT_MANIFEST_SHA256 = (
    "0a4ec2d743a3f181423c24bbf7c6d9d7e0feef5dc0f0fc3e5d3d69150945e896"
)
PROFILE_MEMBERS = (
    "SHA256SUMS",
    "auth.schema.json",
    "cases.json",
    "discovery.schema.json",
    "errors.schema.json",
    "examples/claim.json",
    "examples/current.json",
    "examples/discovery.json",
    "examples/drives.json",
    "examples/health.json",
    "examples/invitation.json",
    "examples/ready.json",
    "examples/vehicles.json",
    "field-semantics.json",
    "openapi.json",
    "profile.json",
    "resources.schema.json",
    "sync-regression.json",
)
_FORBIDDEN_JOB_AUTHORITY = frozenset(
    {
        "executable",
        "validator_path",
        "output_path",
        "broker_socket",
    }
)


class HomeAssistantInstalledPending(RuntimeError):
    """The fixed HA seam cannot admit the supplied source/runtime inputs."""


def execution_by_target():
    return (
        ("macos_arm64", "docker_exec_pipe"),
        ("debian13_amd64", "docker_exec_pipe"),
        ("debian13_arm64", "docker_exec_pipe"),
    )


def broker_kind_by_target():
    return (
        ("macos_arm64", "stdio"),
        ("debian13_amd64", "stdio"),
        ("debian13_arm64", "stdio"),
    )


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise HomeAssistantInstalledPending(label + " path is not canonical")
    return path


def _read_regular(
    path: Path, label: str, maximum: int = 8_388_608, *, private: bool = True
) -> bytes:
    path = _canonical(path, label)
    try:
        before = path.lstat()
    except OSError as error:
        raise HomeAssistantInstalledPending(label + " is unavailable") from error
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_uid != os.getuid()
        or before.st_size > maximum
        or private
        and stat.S_IMODE(before.st_mode) & 0o077
    ):
        raise HomeAssistantInstalledPending(
            label + " is not an admissible regular file"
        )
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(descriptor)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise HomeAssistantInstalledPending(label + " changed before reading")
            chunks = bytearray()
            while len(chunks) <= maximum:
                part = os.read(descriptor, min(65_536, maximum + 1 - len(chunks)))
                if not part:
                    break
                chunks.extend(part)
            after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        final = path.lstat()
    except HomeAssistantInstalledPending:
        raise
    except OSError as error:
        raise HomeAssistantInstalledPending(label + " cannot be read") from error
    identity = (before.st_dev, before.st_ino, before.st_mtime_ns, before.st_size)
    if (
        len(chunks) > maximum
        or len(chunks) != before.st_size
        or identity != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size)
        or identity != (final.st_dev, final.st_ino, final.st_mtime_ns, final.st_size)
    ):
        raise HomeAssistantInstalledPending(label + " changed while reading")
    return bytes(chunks)


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
        + "\n"
    ).encode("utf-8")


def _write_bytes(path: Path, raw: bytes) -> Mapping[str, str]:
    path = _canonical(path, "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as error:
        raise HomeAssistantInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise
    return {"path": str(path), "sha256": _digest(raw)}


def _stage(
    root: Path,
    identifier: str,
    raw: bytes,
    suffix: str = ".bin",
    relative: Path | None = None,
):
    leaf = (
        relative
        if relative is not None
        else Path(identifier.replace("/", "_") + suffix)
    )
    return {
        "id": identifier,
        "root": _write_bytes(root / "root" / leaf, raw),
        "local": _write_bytes(root / "local" / leaf, raw),
    }


def _bound_json(binding: Mapping[str, Any], label: str) -> Mapping[str, Any]:
    try:
        value = strict_json(read_bound_file(binding, label=label, maximum=1_048_576))
    except Exception as error:
        raise HomeAssistantInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise HomeAssistantInstalledPending(label + " is not an object")
    return value


def _validate_reviewed_sources() -> None:
    for relative, expected in REVIEWED_SOURCES.items():
        raw = _read_regular(
            HA_ROOT / relative, "reviewed HA source", 2_097_152, private=False
        )
        if _digest(raw) != expected:
            raise HomeAssistantInstalledPending("pending: reviewed HA source changed")
    for path, expected in RAW_SCHEMAS.values():
        if (
            _digest(
                _read_regular(path, "reviewed HA raw schema", 1_048_576, private=False)
            )
            != expected
        ):
            raise HomeAssistantInstalledPending(
                "pending: reviewed HA raw schema changed"
            )


def _reviewed_json(reviewed: ReviewedFile, label: str) -> Mapping[str, Any]:
    raw = _read_regular(reviewed.path, label, 1_048_576, private=False)
    if _digest(raw) != reviewed.sha256:
        raise HomeAssistantInstalledPending("pending: " + label + " changed")
    try:
        value = strict_json(raw)
    except Exception as error:
        raise HomeAssistantInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise HomeAssistantInstalledPending(label + " is not an object")
    return value


def _validate_handoff() -> None:
    handoff = _reviewed_json(HANDOFF, "reviewed HA handoff")
    manifest = _reviewed_json(SOURCE_MANIFEST, "reviewed HA source manifest")
    adapter = handoff.get("adapter")
    source_binding = adapter.get("source_manifest") if isinstance(adapter, Mapping) else None
    refresh = adapter.get("source_manifest_refresh") if isinstance(adapter, Mapping) else None
    if (
        handoff.get("schema_version") != 1
        or handoff.get("kind") != "teslatlas-home-assistant-hub-admission-handoff"
        or not isinstance(source_binding, Mapping)
        or source_binding.get("path") != str(SOURCE_MANIFEST.path)
        or source_binding.get("sha256") != SOURCE_MANIFEST.sha256
        or source_binding.get("file_count") != len(REVIEWED_SOURCES)
        or not isinstance(refresh, Mapping)
        or refresh.get("changed_file") != "teslatlas-home-assistant/tests/test_matrix_live.py"
        or refresh.get("current_file_sha256")
        != REVIEWED_SOURCES["tests/test_matrix_live.py"]
    ):
        raise HomeAssistantInstalledPending("reviewed HA handoff binding is invalid")
    rows = manifest.get("files")
    expected_paths = {"teslatlas-home-assistant/" + path for path in REVIEWED_SOURCES}
    if (
        manifest.get("schema_version") != 1
        or not isinstance(rows, list)
        or {row.get("path") for row in rows if isinstance(row, Mapping)}
        != expected_paths
        or len(rows) != len(expected_paths)
    ):
        raise HomeAssistantInstalledPending("reviewed HA source manifest is invalid")
    for row in rows:
        relative = str(row["path"]).removeprefix("teslatlas-home-assistant/")
        path = HA_ROOT / relative
        raw = _read_regular(path, "reviewed HA source", 2_097_152, private=False)
        if row != {
            "path": "teslatlas-home-assistant/" + relative,
            "bytes": len(raw),
            "mode": f"{stat.S_IMODE(path.lstat().st_mode):04o}",
            "sha256": REVIEWED_SOURCES[relative],
        }:
            raise HomeAssistantInstalledPending(
                "reviewed HA source manifest differs from source"
            )


def _environment(job: Mapping[str, Any]) -> Mapping[str, str]:
    path = _canonical(Path(str(job.get("environment_file", ""))), "environment file")
    value = _bound_json(
        {"path": str(path), "sha256": _digest(_read_regular(path, "environment file"))},
        "environment file",
    )
    if set(value) != {"TESLATLAS_HA_INSTALLED_ROOT"} or not isinstance(
        value["TESLATLAS_HA_INSTALLED_ROOT"], str
    ):
        raise HomeAssistantInstalledPending("HA installed environment is not closed")
    return value


def _profile_inputs(profile: Mapping[str, Any], stage: Path):
    root = _canonical(Path(str(profile.get("path", ""))), "profile root")
    manifest_raw = _read_regular(
        root / "SHA256SUMS", "profile manifest", 1_048_576, private=False
    )
    if (
        _digest(manifest_raw) != profile.get("sha256")
        or profile.get("sha256")
        != "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
    ):
        raise HomeAssistantInstalledPending("profile manifest digest changed")
    try:
        checksums = {
            name: value
            for value, name in (
                line.split("  ", 1)
                for line in manifest_raw.decode("ascii").splitlines()
            )
        }
    except (UnicodeError, ValueError) as error:
        raise HomeAssistantInstalledPending(
            "profile manifest syntax is invalid"
        ) from error
    if set(checksums) != set(PROFILE_MEMBERS) - {"SHA256SUMS"}:
        raise HomeAssistantInstalledPending("profile member set is incomplete")
    members = []
    for name in PROFILE_MEMBERS:
        raw = (
            manifest_raw
            if name == "SHA256SUMS"
            else _read_regular(root / name, "profile member", 1_048_576, private=False)
        )
        if name != "SHA256SUMS" and _digest(raw) != checksums[name]:
            raise HomeAssistantInstalledPending(
                "profile member digest differs from manifest"
            )
        members.append(
            _stage(
                stage,
                "profile_" + name.replace("/", "_"),
                raw,
                relative=Path("hub-http-v1") / "1.0.0" / name,
            )
        )
    return members[0], members


def _artifact(job: Mapping[str, Any]) -> tuple[Mapping[str, Any], bytes]:
    artifacts = job.get("artifacts")
    if not isinstance(artifacts, list):
        raise HomeAssistantInstalledPending("HA artifact inventory is incomplete")
    selected = [
        row
        for row in artifacts
        if isinstance(row, Mapping)
        and row.get("role") == "home_assistant_integration_archive"
    ]
    if len(selected) != 1 or selected[0].get("embedded_version") != "2026.36.2":
        raise HomeAssistantInstalledPending("HA artifact inventory is incomplete")
    artifact = selected[0]
    raw = _read_regular(
        Path(str(artifact.get("path", ""))), "HA integration archive", 1_073_741_824
    )
    if _digest(raw) != artifact.get("sha256"):
        raise HomeAssistantInstalledPending("HA integration archive digest changed")
    expected = {
        str(Path("custom_components/teslatlas_hub") / row["path"]): row
        for row in COMPONENT_ROWS
    }
    try:
        with tarfile.open(artifact["path"], "r:gz") as archive:
            observed = {}
            for member in archive.getmembers():
                if member.isdir():
                    continue
                if (
                    not member.isfile()
                    or member.name not in expected
                    or member.name in observed
                ):
                    raise HomeAssistantInstalledPending(
                        "HA integration archive inventory is foreign"
                    )
                stream = archive.extractfile(member)
                member_raw = stream.read(member.size + 1) if stream is not None else b""
                row = expected[member.name]
                if (
                    len(member_raw) != row["bytes"]
                    or _digest(member_raw) != row["sha256"]
                ):
                    raise HomeAssistantInstalledPending(
                        "HA integration archive member changed"
                    )
                observed[member.name] = True
    except (OSError, tarfile.TarError) as error:
        raise HomeAssistantInstalledPending(
            "HA integration archive is invalid"
        ) from error
    if set(observed) != set(expected):
        raise HomeAssistantInstalledPending(
            "HA integration archive inventory is incomplete"
        )
    return artifact, raw


def _installed_manifest(root: Path, artifact: Mapping[str, Any]) -> Mapping[str, Any]:
    root = _canonical(root, "HA installed root")
    if root.name != "teslatlas_hub" or root.parent.name != "custom_components":
        raise HomeAssistantInstalledPending("HA installed root layout is invalid")
    actual = []
    expected = {row["path"]: row for row in COMPONENT_ROWS}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise HomeAssistantInstalledPending("HA installed root contains a symlink")
        if not path.is_file():
            continue
        relative = str(path.relative_to(root))
        raw = _read_regular(path, "HA installed member", 2_097_152, private=False)
        row = expected.get(relative)
        mode = stat.S_IMODE(path.lstat().st_mode)
        if (
            row is None
            or len(raw) != row["bytes"]
            or _digest(raw) != row["sha256"]
            or mode != row["mode"]
        ):
            raise HomeAssistantInstalledPending(
                "HA installed member differs from reviewed component"
            )
        actual.append(dict(row))
    if [row["path"] for row in actual] != [row["path"] for row in COMPONENT_ROWS]:
        raise HomeAssistantInstalledPending("HA installed inventory is incomplete")
    return {"schema_version": 1, "artifact_sha256": artifact["sha256"], "files": actual}


def _private_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    broker = _canonical(Path(str(descriptor.get("broker_socket", ""))), "broker socket")
    session_id = descriptor.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise HomeAssistantInstalledPending("installed session identity is unavailable")
    root = broker.parent / ("home-assistant-" + cell_id + "-" + session_id)
    root.mkdir(mode=0o700, exist_ok=False)
    return root


def build_session_input(
    job, cell, config, matrix, contract: LoadedContract, descriptor, running
):
    del matrix
    if _FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise HomeAssistantInstalledPending(
            "job attempts to select Home Assistant adapter authority"
        )
    _validate_reviewed_sources()
    _validate_handoff()
    if (
        contract.manifest.get("adapter_id") != "home_assistant"
        or not isinstance(running, Mapping)
        or not isinstance(running.get("descriptor"), Mapping)
    ):
        raise HomeAssistantInstalledPending("installed Hub descriptor is unavailable")
    roles = {
        row.get("role"): row
        for row in job.get("source_identities", [])
        if isinstance(row, Mapping)
    }
    if set(roles) != {"hub_source", "protocol_source", "home_assistant_source"}:
        raise HomeAssistantInstalledPending("HA source inventory is incomplete")
    expected_repos = {
        "hub_source": WORKSPACE / "hub",
        "protocol_source": WORKSPACE / "teslatlas-protocol",
        "home_assistant_source": HA_ROOT,
    }
    if any(
        Path(str(roles[role].get("repo", ""))).resolve() != root
        for role, root in expected_repos.items()
    ):
        raise HomeAssistantInstalledPending("HA source inventory is foreign")
    session_config = _bound_json(
        job["installed_session"]["config"], "installed session config"
    )
    environment = _environment(job)
    stage = _private_root(descriptor, cell["id"])
    profile_manifest, profile_members = _profile_inputs(
        config["profile"], stage / "inputs"
    )
    scenario_raw = read_bound_file(
        session_config["scenario"], label="scenario", maximum=1_048_576
    )
    scenario = _stage(stage / "inputs", "scenario", scenario_raw, ".json")
    certificate_path = _canonical(
        Path(running["descriptor"]["certificate_path"]), "certificate"
    )
    certificate_raw = _read_regular(certificate_path, "certificate", 1_048_576)
    certificate = _stage(stage / "inputs", "certificate", certificate_raw, ".pem")
    try:
        certificate_der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
        certificate_der_sha256 = _digest(
            bytes.fromhex(certificate_der)
            if isinstance(certificate_der, str)
            else certificate_der
        )
    except (UnicodeError, ValueError, ssl.SSLError) as error:
        raise HomeAssistantInstalledPending("certificate is not PEM") from error
    artifact, archive_raw = _artifact(job)
    product = _stage(
        stage / "products", "home_assistant_integration_archive", archive_raw, ".tar.gz"
    )
    installed_root = _canonical(
        Path(environment["TESLATLAS_HA_INSTALLED_ROOT"]), "HA installed root"
    )
    installed_manifest_value = _installed_manifest(installed_root, artifact)
    installed_manifest = _stage(
        stage / "products",
        "home_assistant_installed_manifest",
        _json_bytes(installed_manifest_value),
        ".json",
    )
    actors = []
    for actor_id, kind, entrypoint in (
        ("ha_flow", "installed_ha_flow", "ha_matrix_flow"),
        ("ha_client", "installed_ha_client", "ha_matrix_client"),
    ):
        actor_manifest = _stage(
            stage / "actors",
            actor_id + "_inputs",
            _json_bytes(installed_manifest_value),
            ".json",
        )
        actors.append(
            {
                "id": actor_id,
                "kind": kind,
                "execution": "coordinator",
                "runtime_ref": "ha_container",
                "artifact_roles": ["home_assistant_integration_archive"],
                "source_roles": ["home_assistant_source"],
                "entrypoint_ref": entrypoint,
                "input_manifest": actor_manifest,
                "phase_contract": None,
            }
        )
    header = {
        "schema_version": 1,
        "execution_kind": "actual_hub_acceptance",
        "adapter": job["adapter"],
        "cell_id": job["cell_id"],
        "product_version": config["product_version"],
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
        "source_identities": job["source_identities"],
        "artifacts": job["artifacts"],
        "runtime": job["runtime"],
    }
    header_stage = _stage(stage / "inputs", "header", _json_bytes(header), ".json")
    contract_raw = _read_regular(
        Path(CONTRACT.manifest_path), "reviewed HA contract", 1_048_576, private=False
    )
    if _digest(contract_raw) != CONTRACT.manifest_sha256:
        raise HomeAssistantInstalledPending("pending: reviewed HA contract changed")
    contract_stage = _stage(stage / "inputs", "case_contract", contract_raw, ".json")
    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.log"),
    }
    value = {
        "schema_version": 1,
        "kind": "matrix-adapter-session",
        "run_id": session_config["run_id"],
        "cell_id": cell["id"],
        "adapter_id": "home_assistant",
        "client_id": "home_assistant",
        "session_id": descriptor["session_id"],
        "instance_nonce": os.urandom(32).hex(),
        "header": header_stage,
        "case_contract": contract_stage,
        "host_session": descriptor,
        "broker": {"kind": "stdio", "socket_path": None},
        "inputs": {
            "profile_manifest": profile_manifest,
            "profile_members": profile_members,
            "scenario": scenario,
            "certificate": certificate,
            "certificate_der_sha256": certificate_der_sha256,
            "product_inputs": [
                {
                    "artifact_role": "home_assistant_integration_archive",
                    "staged": product,
                    "installed_manifest": installed_manifest,
                    "local_root": str(installed_root),
                }
            ],
        },
        "actors": actors,
        "outputs": outputs,
        "bounds": {
            "cell_timeout_ms": job["timeout_seconds"] * 1000,
            "cleanup_timeout_ms": 45000,
            "frame_bytes": 1048576,
            "evidence_bytes": 8388608,
            "framework_log_bytes": 8388608,
        },
    }
    path = stage / "session-input.json"
    _write_bytes(path, _json_bytes(value))
    return path, value


def launch_adapter(
    job, cell, config, contract, session_input_path, session_input, **_kwargs
):
    del cell, config, contract, session_input_path, session_input
    _validate_reviewed_sources()
    _validate_handoff()
    if _FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise HomeAssistantInstalledPending(
            "job attempts to select Home Assistant adapter authority"
        )
    # The reviewed handoff explicitly records no installed container. It does
    # not define a hash-bound Docker executable, image digest/container
    # registration, or host-to-container path map, so inventing argv here would
    # transfer executable authority back to the job.
    raise HomeAssistantInstalledPending(
        "reviewed Home Assistant runtime registration is unavailable"
    )


def _require_runtime_registration() -> None:
    _validate_reviewed_sources()
    _validate_handoff()
    raise HomeAssistantInstalledPending(
        "reviewed Home Assistant runtime registration is unavailable"
    )


def runtime_inventory(
    job=None, cell=None, config=None, contract=None, session_input=None, deadline=None
):
    """Keep expected runtime metadata separate from an observed container."""
    del job, cell, config, contract, session_input
    if deadline is not None:
        deadline.remaining()
    _require_runtime_registration()


def admit(
    job=None,
    cell=None,
    config=None,
    matrix=None,
    contract=None,
    normalized=None,
    actors=None,
    *,
    admission_views=None,
    runtime_context=None,
    deadline=None,
):
    """Do not evaluate source evidence as installed runtime evidence."""
    del (
        job,
        cell,
        config,
        matrix,
        contract,
        normalized,
        actors,
        admission_views,
        runtime_context,
    )
    if deadline is not None:
        deadline.remaining()
    _require_runtime_registration()


def build_supplement(
    job=None,
    cell=None,
    config=None,
    matrix=None,
    contract=None,
    result=None,
    *,
    runtime_context=None,
):
    """Refuse a receipt until an observed runtime can bind its evidence."""
    del job, cell, config, matrix, contract, result, runtime_context
    _require_runtime_registration()


def execution_logs(job, cell, config, contract, result):
    """Hash the reviewed launcher's bounded stdout and stderr streams."""
    del job, cell, config, contract
    _validate_reviewed_sources()
    _validate_handoff()
    try:
        framework = _canonical(
            Path(result.session_input["outputs"]["framework_log"]),
            "HA framework log",
        )
    except (AttributeError, KeyError, TypeError) as error:
        raise HomeAssistantInstalledPending("HA framework log is unavailable") from error
    output = {}
    for name, path in (
        ("stdout", framework),
        ("stderr", Path(str(framework) + ".stderr.log")),
    ):
        raw = _read_regular(path, "HA " + name + " log", 8_388_608)
        output[name] = {
            "sha256": _digest(raw),
            "bytes": len(raw),
            "truncated": False,
        }
    output["duration_ms"] = 0
    return output


def source_entry(adapter_id: str):
    """Return the sole source-fixed HA contract after its handoff recheck."""
    if adapter_id != "home_assistant":
        raise HomeAssistantInstalledPending(
            "Home Assistant adapter identity is unavailable"
        )
    _validate_reviewed_sources()
    _validate_handoff()
    return CONTRACT
