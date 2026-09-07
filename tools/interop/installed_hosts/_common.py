# SPDX-License-Identifier: AGPL-3.0-only
"""Shared fixed observation primitives used inside a registered guest."""
import hashlib
import importlib
import json
import socket
import ssl
import stat
import re
import time
import urllib.parse
from pathlib import Path

from .bounded import Deadline, run_capped

MAX_OUTPUT = 262_144


def run_checked(argv, timeout, allowed_status=(0,), deadline=None):
    command_deadline = Deadline(timeout) if deadline is None else Deadline(min(timeout, deadline.remaining()))
    return run_capped(argv, command_deadline, maximum=MAX_OUTPUT, allowed_status=allowed_status)


def sha256_bytes(value):
    return hashlib.sha256(value).hexdigest()


def sha256_file(path):
    target = Path(path)
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("observed path is not a regular non-symlink file")
    digest = hashlib.sha256()
    with target.open("rb") as handle:
        while True:
            block = handle.read(1024 * 1024)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def read_json(path, private=False):
    target = Path(path)
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_OUTPUT:
        raise ValueError("JSON evidence must be a bounded regular file")
    if private and info.st_mode & 0o077:
        raise ValueError("private JSON evidence has broad permissions")
    return json.loads(target.read_text(encoding="utf-8"))


def load_toml(path, module_name):
    if module_name not in ("tomllib", "tomli"):
        raise ValueError("unregistered TOML module")
    module = importlib.import_module(module_name)
    with Path(path).open("rb") as handle:
        return module.load(handle)


def loads_toml(raw, module_name):
    if module_name not in ("tomllib", "tomli"):
        raise ValueError("unregistered TOML module")
    return importlib.import_module(module_name).loads(raw.decode("utf-8"))


def validate_synthetic_config(config, expected_path, expected_data_dir, expected_origins):
    if config.get("data_dir") != expected_data_dir or config.get("bind") != "127.0.0.1:18480":
        raise ValueError("config does not bind the fresh matrix store and loopback listener")
    tls = config.get("tls")
    collector = config.get("collector")
    if not isinstance(tls, dict) or tls.get("public_url") != "https://127.0.0.1:18480":
        raise ValueError("config TLS endpoint is not fixed")
    if not isinstance(collector, dict) or collector.get("interval_seconds") != 0 or collector.get("owner_api_base_url") != "https://127.0.0.1:1/":
        raise ValueError("config synthetic collector constraints are absent")
    legacy = collector.get("legacy_auth")
    if not isinstance(legacy, dict) or legacy.get("enabled") is not False:
        raise ValueError("legacy network auth must be disabled")
    if config.get("terrain") != {"enabled": False} or config.get("geocoder") != {"enabled": False}:
        raise ValueError("external enrichments must be disabled")
    http = config.get("http")
    if not isinstance(http, dict) or http.get("allowed_origins") != expected_origins:
        raise ValueError("config allowed origins do not match the cell")
    expected_tls_paths = {
        "certificate_path": expected_path + "/server.pem",
        "private_key_path": expected_path + "/server-key.pem",
    }
    for key, expected in expected_tls_paths.items():
        if tls.get(key) != expected:
            raise ValueError("config TLS paths do not match the fixed fresh-cell identity")
    return tls


PROFILE_MEMBERS = frozenset((
    "SHA256SUMS", "auth.schema.json", "cases.json", "discovery.schema.json",
    "errors.schema.json", "examples/claim.json", "examples/current.json",
    "examples/discovery.json", "examples/drives.json", "examples/health.json",
    "examples/invitation.json", "examples/ready.json", "examples/vehicles.json",
    "field-semantics.json", "openapi.json", "profile.json",
    "resources.schema.json", "sync-regression.json",
))


def validate_profile_bundle(sum_path):
    """Bind every reviewed profile member, then return schema-derived constants."""
    sums = Path(sum_path)
    raw = sums.read_text(encoding="utf-8")
    members = {}
    for line in raw.splitlines():
        fields = line.split("  ", 1)
        if len(fields) != 2 or len(fields[0]) != 64 or fields[1] in members:
            raise ValueError("profile SHA256SUMS is invalid")
        members[fields[1]] = fields[0]
    expected = PROFILE_MEMBERS - {"SHA256SUMS"}
    if set(members) != expected:
        raise ValueError("profile member set differs from the reviewed profile")
    for name, digest in members.items():
        if sha256_file(sums.parent / name) != digest:
            raise ValueError("profile member digest mismatch")
    profile = read_json(sums.parent / "profile.json")
    discovery = read_json(sums.parent / "discovery.schema.json")
    resources = read_json(sums.parent / "resources.schema.json")
    if profile.get("profile_id") != "hub-http-v1@1.0.0":
        raise ValueError("profile identity is not hub-http-v1@1.0.0")
    if not isinstance(discovery.get("required"), list) or not isinstance(resources.get("$defs"), dict):
        raise ValueError("profile schemas are incomplete")
    return {"profile": profile, "discovery": discovery, "resources": resources}


def _matches_schema(value, schema):
    """Evaluate the closed JSON-Schema subset used by the three profile probes."""
    if not isinstance(schema, dict):
        return False
    supported = {"$id", "$schema", "type", "const", "enum", "oneOf", "anyOf", "properties", "required", "additionalProperties", "items", "maxLength", "pattern", "format", "minimum", "maximum"}
    if any(key not in supported for key in schema):
        return False
    if "oneOf" in schema:
        return sum(_matches_schema(value, candidate) for candidate in schema["oneOf"]) == 1
    if "anyOf" in schema and not any(_matches_schema(value, candidate) for candidate in schema["anyOf"]):
        return False
    expected_type = schema.get("type")
    type_ok = {
        "object": lambda item: isinstance(item, dict),
        "array": lambda item: isinstance(item, list),
        "string": lambda item: isinstance(item, str),
        "integer": lambda item: type(item) is int,
        "number": lambda item: (type(item) is int or isinstance(item, float)),
        "boolean": lambda item: type(item) is bool,
        "null": lambda item: item is None,
    }
    if expected_type is not None and (expected_type not in type_ok or not type_ok[expected_type](value)):
        return False
    def same_json_literal(left, right):
        return type(left) is type(right) and left == right
    if "const" in schema and not same_json_literal(value, schema["const"]):
        return False
    if "enum" in schema and not any(same_json_literal(value, item) for item in schema["enum"]):
        return False
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        if any(name not in value for name in schema.get("required", [])):
            return False
        if schema.get("additionalProperties") is False and any(name not in properties for name in value):
            return False
        if any(not _matches_schema(item, properties[name]) for name, item in value.items() if name in properties):
            return False
    if isinstance(value, str):
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            return False
        if "pattern" in schema and re.fullmatch(schema["pattern"], value) is None:
            return False
        if schema.get("format") == "uuid":
            try:
                import uuid
                if str(uuid.UUID(value)) != value:
                    return False
            except (ValueError, AttributeError):
                return False
        if schema.get("format") == "uri" and not urllib.parse.urlparse(value).scheme:
                return False
    if isinstance(value, list) and "items" in schema and any(not _matches_schema(item, schema["items"]) for item in value):
        return False
    if type(value) in (int, float):
        if "minimum" in schema and value < schema["minimum"]:
            return False
        if "maximum" in schema and value > schema["maximum"]:
            return False
    return True


def validate_scenario_bytes(raw):
    """Validate the exact synthetic scenario identity before executing the seed."""
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("scenario is not strict UTF-8 JSON") from error
    if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value["schema_version"] != 1:
        raise ValueError("scenario schema version is not supported")
    if value.get("name") != "two-vehicles-five-drives" or value.get("provenance") != "synthetic-only" or "later_current" not in value:
        raise ValueError("scenario identity or synthetic provenance differs")
    return value


def _wait_socket(sock, deadline, write=False):
    import select
    ready = select.select([], [sock], [], deadline.remaining())[1] if write else select.select([sock], [], [], deadline.remaining())[0]
    if not ready:
        raise TimeoutError("TLS operation exceeded its whole-operation deadline")


def _https_get(context, path, deadline):
    raw = socket.create_connection(("127.0.0.1", 18480), timeout=deadline.remaining())
    raw.setblocking(False)
    sock = context.wrap_socket(raw, server_hostname="127.0.0.1", do_handshake_on_connect=False)
    try:
        while True:
            try:
                sock.do_handshake()
                break
            except ssl.SSLWantReadError:
                _wait_socket(sock, deadline)
            except ssl.SSLWantWriteError:
                _wait_socket(sock, deadline, write=True)
        peer = sock.getpeercert(binary_form=True)
        request = ("GET {} HTTP/1.1\r\nHost: 127.0.0.1:18480\r\nAccept: application/json\r\nConnection: close\r\n\r\n".format(path)).encode("ascii")
        sent = 0
        while sent < len(request):
            try:
                sent += sock.send(request[sent:])
            except ssl.SSLWantReadError:
                _wait_socket(sock, deadline)
            except (ssl.SSLWantWriteError, BlockingIOError):
                _wait_socket(sock, deadline, write=True)
        response = bytearray()
        header_end = -1
        content_length = None
        while True:
            if len(response) > MAX_OUTPUT + 16384:
                raise ValueError("normal TLS response exceeded bound")
            if header_end >= 0 and len(response) >= header_end + content_length:
                break
            try:
                chunk = sock.recv(65536)
            except ssl.SSLWantReadError:
                _wait_socket(sock, deadline)
                continue
            except ssl.SSLWantWriteError:
                _wait_socket(sock, deadline, write=True)
                continue
            if not chunk:
                raise ValueError("normal TLS response ended before declared body")
            response.extend(chunk)
            if header_end < 0:
                split = response.find(b"\r\n\r\n")
                if split >= 0:
                    header_end = split + 4
                    lines = bytes(response[:split]).split(b"\r\n")
                    if not lines or not lines[0].startswith(b"HTTP/1.1 "):
                        raise ValueError("normal TLS response status line is invalid")
                    fields = {}
                    for line in lines[1:]:
                        if b":" not in line:
                            raise ValueError("normal TLS response header is invalid")
                        name, value = line.split(b":", 1)
                        key = name.strip().lower()
                        if key in fields:
                            raise ValueError("normal TLS response has duplicate header")
                        fields[key] = value.strip()
                    if b"transfer-encoding" in fields or b"content-length" not in fields:
                        raise ValueError("normal TLS response requires one content-length")
                    try:
                        content_length = int(fields[b"content-length"])
                    except ValueError as error:
                        raise ValueError("normal TLS response content-length is invalid") from error
                    if content_length < 0 or content_length > MAX_OUTPUT:
                        raise ValueError("normal TLS response body exceeds bound")
        status = int(bytes(response[:header_end]).split(b" ", 2)[1])
        return status, bytes(response[header_end:header_end + content_length]), peer
    finally:
        sock.close()


def normal_tls_probe(certificate_path, expected_version, deadline=None, profile_path=None):
    deadline = deadline or Deadline(10)
    schemas = validate_profile_bundle(profile_path) if profile_path is not None else None
    context = ssl.create_default_context(cafile=certificate_path)
    context.check_hostname = True
    bodies = {}
    certificate_der = None
    for path in ("/.well-known/teslatlas-hub", "/healthz", "/readyz"):
        while True:
            try:
                status, body, peer = _https_get(context, path, deadline)
                if not peer:
                    raise ValueError("TLS peer certificate was not observed")
                if certificate_der is None:
                    certificate_der = peer
                elif peer != certificate_der:
                    raise ValueError("TLS peer leaf changed during the fixed probe set")
                if 300 <= status < 400:
                    raise ValueError("normal TLS probe failed or redirected")
                if status == 200:
                    if len(body) > MAX_OUTPUT:
                        raise ValueError("normal TLS probe failed or redirected")
                    bodies[path] = body
                    break
                if path != "/readyz" or not 500 <= status < 600:
                    raise ValueError("normal TLS probe failed or redirected")
            except (ConnectionRefusedError, socket.timeout, ConnectionResetError):
                deadline.remaining()
            time.sleep(min(0.05, deadline.remaining()))
    discovery = json.loads(bodies["/.well-known/teslatlas-hub"].decode("utf-8"))
    health = json.loads(bodies["/healthz"].decode("utf-8"))
    ready = json.loads(bodies["/readyz"].decode("utf-8"))
    if schemas is not None:
        resources = schemas["resources"].get("$defs", {})
        if not _matches_schema(discovery, schemas["discovery"]) or not _matches_schema(health, resources.get("health", {})) or not _matches_schema(ready, resources.get("ready", {})):
            raise ValueError("normal response set fails the bound profile schemas")
    if discovery.get("protocol") != "teslatlas-sync" or discovery.get("protocol_major") != 1 or discovery.get("api_versions") != ["1.0"] or discovery.get("pack_format") != "sqlite-zstd" or discovery.get("version") != expected_version:
        raise ValueError("discovery does not match current Hub HTTP profile")
    if health != {"status": "ok", "version": expected_version} or ready != {"status": "ready"}:
        raise ValueError("health/readiness bodies do not match current Hub profile")
    if certificate_der is None:
        raise ValueError("TLS peer certificate was not observed")
    combined = b"".join(path.encode("ascii") + b"\0" + bodies[path] for path in sorted(bodies))
    return {
        "certificate_der_sha256": sha256_bytes(certificate_der),
        "hub_id": discovery.get("hub_id"),
        "response_sha256": sha256_bytes(bodies["/.well-known/teslatlas-hub"]),
        "validated_response_set_sha256": sha256_bytes(combined),
    }


def process_tree_digest(*identities):
    raw = json.dumps(identities, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return sha256_bytes(raw)


def verify_payload_members(manifest_path):
    # The manifest itself was validated on the runner before transfer. The
    # guest still requires the closed member shape before touching any path.
    value = read_json(manifest_path, private=True)
    members = value.get("payload_members") if isinstance(value, dict) else None
    if not isinstance(members, list) or not 1 <= len(members) <= 512:
        raise ValueError("guest package manifest has invalid payload members")
    observed = []
    seen = set()
    for item in members:
        if not isinstance(item, dict) or set(item) != {"path", "sha256"} or item["path"] in seen:
            raise ValueError("guest payload member is invalid or duplicated")
        seen.add(item["path"])
        actual = sha256_file(item["path"])
        if actual != item["sha256"]:
            raise ValueError("installed payload member digest mismatch")
        observed.append({"path": item["path"], "sha256": actual})
    digest = sha256_bytes(json.dumps(sorted(observed, key=lambda item: item["path"]), sort_keys=True, separators=(",", ":")).encode("utf-8"))
    if digest != value.get("payload_manifest_sha256"):
        raise ValueError("installed payload manifest aggregate mismatch")
    return digest
