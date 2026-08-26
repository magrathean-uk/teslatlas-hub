#!/usr/bin/env python3
"""Create local, candidate-bound release evidence without building anything."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone


SCHEMA = "teslatlas.release-evidence/v1"
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
FINGERPRINT_RE = re.compile(r"^(?:[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$")


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class ArtifactWitness:
    path: Path
    relative_path: str
    size: int
    digest: str


def run(args: list[str], cwd: Path, *, env: dict[str, str] | None = None,
        capture: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args, cwd=cwd, env=env, text=True, capture_output=capture, check=True
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = ""
        if isinstance(exc, subprocess.CalledProcessError):
            detail = (exc.stderr or exc.stdout or "").strip()
        raise GateError(f"command failed: {' '.join(args)}{': ' + detail if detail else ''}") from exc


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def capture_artifact(repo: Path, path: Path) -> ArtifactWitness:
    regular_file(path, "artifact")
    size = path.stat().st_size
    digest = sha256(path)
    regular_file(path, "artifact")
    if path.stat().st_size != size:
        raise GateError(f"artifact changed during evidence generation: {path}")
    return ArtifactWitness(path, relative(repo, path), size, digest)


def verify_artifact_unchanged(repo: Path, expected: ArtifactWitness) -> None:
    try:
        current = capture_artifact(repo, expected.path)
    except GateError as exc:
        raise GateError(
            f"artifact changed during evidence generation: {expected.path}"
        ) from exc
    if current != expected:
        raise GateError(f"artifact changed during evidence generation: {expected.path}")


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


def relative(repo: Path, path: Path) -> str:
    return Path(os.path.relpath(path, repo)).as_posix()


def regular_file(path: Path, label: str) -> None:
    if not path.is_file() or path.is_symlink():
        raise GateError(f"{label} must be a regular, non-symlink file: {path}")


def private_signing_key(path: Path) -> None:
    regular_file(path, "signing key")
    metadata = path.stat()
    if metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
        raise GateError("signing key must be owned by the current user and mode 0600 or stricter")


def requires_external_proxy_notice(path: Path) -> bool:
    name = path.name.lower()
    return path.suffix.lower() in {".pkg", ".zip"} or "macos" in name


def clean_and_tag(
    repo: Path,
    tag: str,
    artifacts: list[Path],
    expected_signer: str,
) -> tuple[str, str, str]:
    exclusions = []
    for artifact in artifacts:
        artifact_path = relative(repo, artifact)
        exclusions.append(f":(top,exclude,literal){artifact_path}")
        tracked = subprocess.run(
            ["git", "ls-files", "--error-unmatch", "--", f":(top,literal){artifact_path}"],
            cwd=repo, text=True, capture_output=True,
        )
        if tracked.returncode == 0:
            raise GateError(f"artifact must not be a tracked source file: {artifact}")
    status_args = ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."] + exclusions
    status = run(status_args, repo).stdout
    if status:
        raise GateError("candidate checkout is not clean")
    commit = run(["git", "rev-parse", f"{tag}^{{commit}}"], repo).stdout.strip()
    head = run(["git", "rev-parse", "HEAD"], repo).stdout.strip()
    if head != commit:
        raise GateError("candidate HEAD does not match the signed tag commit")
    exact = run(["git", "describe", "--exact-match", "--tags", commit], repo).stdout.strip()
    if exact != tag:
        raise GateError(f"tag is not an exact commit tag: {tag}")
    try:
        verification = run(["git", "verify-tag", "--raw", tag], repo)
    except GateError as exc:
        raise GateError(f"tag is not cryptographically verified: {tag}") from exc
    status = f"{verification.stdout}\n{verification.stderr}"
    signers = {
        match.group(1).upper()
        for match in re.finditer(r"^\[GNUPG:\] VALIDSIG ([0-9A-F]+)\b", status, re.MULTILINE)
    }
    if signers != {expected_signer.upper()}:
        raise GateError("tag signer does not match the pinned maintainer fingerprint")
    timestamp = run(["git", "show", "-s", "--format=%cI", commit], repo).stdout.strip()
    try:
        created = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError as exc:
        raise GateError(f"invalid commit timestamp: {timestamp}") from exc
    return (
        commit,
        created.astimezone(timezone.utc).isoformat().replace("+00:00", "Z"),
        expected_signer.upper(),
    )


def assert_candidate_unchanged(
    repo: Path,
    tag: str,
    commit: str,
    artifacts: list[Path],
    stage: Path,
) -> None:
    exclusions = [f":(top,exclude,literal){relative(repo, path)}" for path in artifacts]
    try:
        stage.relative_to(repo)
    except ValueError:
        pass
    else:
        exclusions.append(f":(top,exclude,literal){relative(repo, stage)}")
    status = run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."]
        + exclusions,
        repo,
    ).stdout
    if status:
        raise GateError("candidate checkout changed during evidence generation")
    head = run(["git", "rev-parse", "HEAD"], repo).stdout.strip()
    tag_commit = run(["git", "rev-parse", f"{tag}^{{commit}}"], repo).stdout.strip()
    if head != commit or tag_commit != commit:
        raise GateError("candidate HEAD or signed tag changed during evidence generation")


def publish_evidence_directory(stage: Path, output: Path) -> None:
    if os.path.lexists(output):
        raise GateError(f"output directory already exists: {output}")
    try:
        stage.rename(output)
    except OSError as exc:
        raise GateError(f"cannot atomically publish evidence directory: {output}") from exc


def archive(repo: Path, commit: str, tag: str, destination: Path) -> None:
    tar = subprocess.Popen(
        ["git", "archive", "--format=tar", f"--prefix=teslatlas-hub-{tag}/", commit],
        cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert tar.stdout is not None
    try:
        compressed = subprocess.run(
            ["gzip", "-n"], stdin=tar.stdout, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        tar.kill()
        tar.wait()
        raise GateError("deterministic source archive failed") from exc
    finally:
        tar.stdout.close()
    error = tar.stderr.read().decode(errors="replace")
    tar_rc = tar.wait()
    if tar_rc != 0:
        raise GateError(f"git archive failed: {error.strip()}")
    destination.write_bytes(compressed.stdout)


def cargo_metadata(repo: Path) -> dict:
    env = os.environ.copy()
    env["CARGO_NET_OFFLINE"] = "true"
    result = run(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
         "--manifest-path", str(repo / "Cargo.toml")], repo, env=env,
    )
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise GateError("cargo metadata did not return JSON") from exc

    def scrub(value: object) -> object:
        if isinstance(value, dict):
            return {key: scrub(item) for key, item in value.items()}
        if isinstance(value, list):
            return [scrub(item) for item in value]
        if isinstance(value, str):
            repo_text = str(repo)
            if value == repo_text:
                return "."
            if value.startswith(repo_text + os.sep):
                return "./" + relative(repo, Path(value))
        return value

    return scrub(metadata)  # type: ignore[return-value]


def package_sort(package: dict) -> tuple[str, str, str]:
    return (package.get("name", ""), package.get("version", ""), package.get("source", ""))


def package_license_path(package: dict, repo: Path) -> Path | None:
    manifest = package.get("manifest_path")
    if not manifest:
        return None
    manifest_path = Path(manifest)
    if not manifest_path.is_absolute():
        manifest_path = repo / manifest_path
    root = manifest_path.parent
    declared = package.get("license_file")
    candidates: list[Path] = []
    if declared:
        candidates.append(root / declared)
    try:
        candidates.extend(sorted(
            path for path in root.iterdir()
            if path.is_file() and re.match(r"(?i)^(license|licence|copying|notice)([-_.].*)?$", path.name)
        ))
    except OSError:
        return None
    for candidate in candidates:
        if candidate.is_file() and not candidate.is_symlink():
            return candidate
    return None


def sbom_and_notices(metadata: dict, repo: Path) -> tuple[dict, dict, str]:
    project_notices = repo / "THIRD_PARTY_NOTICES.md"
    regular_file(project_notices, "project notices")
    packages = sorted(metadata.get("packages", []), key=package_sort)
    if not packages:
        raise GateError("cargo metadata contains no packages")
    ids = {package["id"]: f"SPDXRef-Package-{index:04d}" for index, package in enumerate(packages, 1)}
    spdx_packages = []
    inventory = []
    license_texts: dict[str, tuple[str, str]] = {}
    for package in packages:
        license_expression = package.get("license") or "NOASSERTION"
        if license_expression == "NOASSERTION" and not package.get("license_file"):
            raise GateError(f"dependency has no declared license: {package.get('name')}")
        source = package.get("source") or "NOASSERTION"
        checksum = package.get("checksum")
        if package.get("source") and not checksum and not package["source"].startswith("git+"):
            raise GateError(f"dependency has no Cargo checksum: {package.get('name')}")
        license_path = package_license_path(package, repo)
        if license_path is None:
            raise GateError(f"dependency license text is unavailable: {package.get('name')}")
        text = license_path.read_text(encoding="utf-8", errors="strict")
        text_hash = hashlib.sha256(text.encode()).hexdigest()
        license_texts.setdefault(text_hash, (license_path.name, text))
        package_id = ids[package["id"]]
        spdx_package = {
            "SPDXID": package_id,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": source,
            "licenseConcluded": license_expression,
            "licenseDeclared": license_expression,
            "copyrightText": "NOASSERTION",
            "filesAnalyzed": False,
            "annotations": [{"annotationType": "OTHER", "annotator": "Tool: teslatlas-release-evidence",
                              "annotationDate": "1970-01-01T00:00:00Z",
                              "comment": f"license-text-sha256={text_hash}"}],
        }
        if checksum:
            spdx_package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        if package.get("repository"):
            spdx_package["externalRefs"] = [{
                "referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl",
                "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
            }]
        spdx_packages.append(spdx_package)
        inventory.append({
            "name": package["name"], "version": package["version"], "source": source,
            "checksum": checksum, "license": license_expression, "license_text_sha256": text_hash,
            "package_id": package_id,
        })

    relationships = []
    resolve_root = (metadata.get("resolve") or {}).get("root")
    root_package_id = resolve_root if resolve_root in ids else None
    workspace_members = metadata.get("workspace_members") or []
    if root_package_id is None:
        root_package_id = next((item for item in workspace_members if item in ids), None)
    if root_package_id is None:
        root_package_id = next((package["id"] for package in packages if package.get("name") == "teslatlas-hub"), None)
    if root_package_id is None:
        raise GateError("cargo metadata has no workspace root package")
    root_id = ids[root_package_id]
    relationships.append({"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES",
                          "relatedSpdxElement": root_id})
    resolve = metadata.get("resolve") or {}
    for node in resolve.get("nodes", []):
        source_id = ids.get(node.get("id"))
        if not source_id:
            continue
        for dependency in node.get("deps", []):
            target_id = ids.get(dependency.get("pkg"))
            if target_id:
                relationships.append({"spdxElementId": source_id, "relationshipType": "DEPENDS_ON",
                                      "relatedSpdxElement": target_id})
    relationships.sort(key=lambda item: (item["spdxElementId"], item["relatedSpdxElement"]))
    spdx = {
        "spdxVersion": "SPDX-2.3", "dataLicense": "CC0-1.0", "SPDXID": "SPDXRef-DOCUMENT",
        "name": "Teslatlas Hub dependency SBOM", "documentNamespace": "urn:teslatlas:sbom",
        "creationInfo": {"created": "1970-01-01T00:00:00Z", "creators": ["Tool: teslatlas-release-evidence"]},
        "packages": spdx_packages, "relationships": relationships,
    }
    notice_lines = [
        "# Generated dependency notices", "",
        "Generated from offline `cargo metadata --locked --all-features`.",
        "This file contains the exact dependency inventory and captured license texts.", "",
        "## Project notices", "", project_notices.read_text(encoding="utf-8"), "",
        "## Dependency inventory", "",
    ]
    for item in inventory:
        notice_lines.extend([f"### {item['name']} {item['version']}", "",
                             f"- Source: `{item['source']}`",
                             f"- License: `{item['license']}`",
                             f"- License text SHA-256: `{item['license_text_sha256']}`", ""])
    notice_lines.extend(["## License texts", ""])
    for text_hash, (name, text) in sorted(license_texts.items()):
        notice_lines.extend([f"### {name} ({text_hash})", "", "```text", text.rstrip(), "```", ""])
    return spdx, {"schema": SCHEMA, "packages": inventory}, "\n".join(notice_lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--tag", required=True)
    parser.add_argument("--tag-signer-fingerprint", required=True,
                        help="approved OpenPGP maintainer fingerprint for the signed tag")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--signing-key", type=Path, required=True,
                        help="unencrypted PEM private key used only for provenance signing")
    parser.add_argument("--public-key-sha256", required=True,
                        help="independently recorded SHA-256 of the PEM public-key trust anchor")
    parser.add_argument("--artifact", action="append", type=Path, required=True,
                        help="final artifact path; repeat for each pkg/zip/deb")
    args = parser.parse_args()
    if not TAG_RE.fullmatch(args.tag):
        raise GateError("tag contains unsafe filename characters")
    if not FINGERPRINT_RE.fullmatch(args.tag_signer_fingerprint):
        raise GateError("tag-signer-fingerprint must be a full OpenPGP fingerprint")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", args.public_key_sha256):
        raise GateError("public-key-sha256 must be 64 hexadecimal characters")
    repo = args.repo.resolve()
    output = args.output_dir.resolve()
    if not (repo / "Cargo.toml").is_file():
        raise GateError("repo has no Cargo.toml")
    if os.path.lexists(output):
        raise GateError(f"output directory already exists: {output}")
    if not output.parent.is_dir():
        raise GateError("output parent does not exist")
    private_signing_key(args.signing_key.resolve())
    for command in ("git", "cargo", "gzip", "openssl", "shasum"):
        if shutil.which(command) is None:
            raise GateError(f"required tool is unavailable: {command}")
    artifacts = []
    for raw in args.artifact:
        path = raw.resolve()
        regular_file(path, "artifact")
        try:
            path.relative_to(repo)
        except ValueError as exc:
            raise GateError(f"artifact must be inside repo: {path}") from exc
        artifacts.append(path)
    if len({path for path in artifacts}) != len(artifacts):
        raise GateError("duplicate artifact path")
    if any(requires_external_proxy_notice(path) for path in artifacts):
        raise GateError(
            "macOS artifact includes Tesla's external vehicle-command proxy; "
            "complete Go dependency notices/source capture are not implemented"
        )
    commit, created, tag_signer = clean_and_tag(
        repo, args.tag, artifacts, args.tag_signer_fingerprint
    )

    stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        artifact_witnesses = [capture_artifact(repo, path) for path in artifacts]
        source_name = f"teslatlas-hub-{args.tag}-source.tar.gz"
        source_path = stage / source_name
        archive(repo, commit, args.tag, source_path)
        metadata = cargo_metadata(repo)
        write_json(stage / "cargo-metadata.json", metadata)
        spdx, inventory, notices = sbom_and_notices(metadata, repo)
        spdx["documentNamespace"] = f"urn:teslatlas:sbom:{args.tag}:{commit}"
        spdx["creationInfo"]["created"] = created
        write_json(stage / "sbom.spdx.json", spdx)
        write_json(stage / "dependency-inventory.json", inventory)
        (stage / "THIRD_PARTY_NOTICES.generated.md").write_text(notices, encoding="utf-8")

        artifact_records = [
            {"path": witness.relative_path, "size": witness.size, "sha256": witness.digest}
            for witness in sorted(artifact_witnesses, key=lambda item: item.relative_path)
        ]
        generated_names = [source_name, "cargo-metadata.json", "sbom.spdx.json",
                           "dependency-inventory.json", "THIRD_PARTY_NOTICES.generated.md"]
        manifest = {"schema": SCHEMA, "tag": args.tag, "commit": commit,
                    "artifacts": artifact_records,
                    "generated": [{"path": f"{relative(repo, output)}/{name}", "sha256": sha256(stage / name)}
                                   for name in generated_names]}
        write_json(stage / "artifact-manifest.json", manifest)

        public_key = stage / "provenance-public-key.pem"
        run(["openssl", "pkey", "-in", str(args.signing_key.resolve()), "-pubout", "-out", str(public_key)], repo)
        public_key_digest = sha256(public_key)
        if public_key_digest.lower() != args.public_key_sha256.lower():
            raise GateError("public-key-sha256 does not match the supplied signing key")
        provenance = {"schema": SCHEMA, "tag": args.tag, "commit": commit,
                      "tag_signature": {"verified": True, "signer_fingerprint": tag_signer},
                      "created": created,
                      "source_archive": {"path": f"{relative(repo, output)}/{source_name}", "sha256": sha256(source_path)},
                      "artifact_manifest": {"path": f"{relative(repo, output)}/artifact-manifest.json",
                                             "sha256": sha256(stage / "artifact-manifest.json")},
                      "sbom_sha256": sha256(stage / "sbom.spdx.json"),
                      "dependency_inventory_sha256": sha256(stage / "dependency-inventory.json"),
                      "notices_sha256": sha256(stage / "THIRD_PARTY_NOTICES.generated.md"),
                      "signing": {"algorithm": "openssl-sha256", "public_key_sha256": public_key_digest}}
        provenance_path = stage / "provenance.json"
        write_json(provenance_path, provenance)
        signature = stage / "provenance.sig"
        run(["openssl", "dgst", "-sha256", "-sign", str(args.signing_key.resolve()),
             "-out", str(signature), str(provenance_path)], repo)
        run(["openssl", "dgst", "-sha256", "-verify", str(public_key), "-signature", str(signature),
             str(provenance_path)], repo)

        checksum_records = [(witness.relative_path, witness.digest) for witness in artifact_witnesses]
        checksum_records.extend(
            (relative(repo, output / path.name), sha256(path))
            for path in [
                stage / name
                for name in generated_names
                + [
                    "artifact-manifest.json",
                    "provenance.json",
                    "provenance.sig",
                    "provenance-public-key.pem",
                ]
            ]
        )
        checksum_lines = [
            f"{digest}  {path}" for path, digest in sorted(checksum_records)
        ]
        (stage / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")
        assert_candidate_unchanged(repo, args.tag, commit, artifacts, stage)
        for witness in artifact_witnesses:
            verify_artifact_unchanged(repo, witness)
        publish_evidence_directory(stage, output)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"release-evidence: BLOCKED: {exc}", file=sys.stderr)
        raise SystemExit(1)
