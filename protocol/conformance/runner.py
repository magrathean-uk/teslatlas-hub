from __future__ import annotations

import argparse
import json
import math
import os
import signal
import subprocess
import sys
from pathlib import Path
from threading import TIMEOUT_MAX
from typing import Any

try:
    from . import hub_sync
except ImportError:
    import hub_sync


ROOT = Path(__file__).resolve().parents[1]
HUB_SYNC_PROFILE = hub_sync.PROFILE_ID
HUB_HTTP_12_PROFILE = "hub-http-v1@1.2.0"


class ConformanceError(Exception):
    pass


def run_actual_hub(command: list[str], timeout_seconds: float) -> int:
    """Bound the complete owned acceptance process, including its descendants."""
    process = subprocess.Popen(command, start_new_session=os.name == "posix")
    try:
        return process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        raise ConformanceError("actual-Hub acceptance exceeded its total process deadline") from error
    finally:
        # A reaped leader can still leave members of its owned process group.
        if os.name == "posix" or process.poll() is None:
            if os.name == "posix":
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            elif process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                pass
            finally:
                if os.name == "posix":
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                elif process.poll() is None:
                    process.kill()
                process.wait()


def run() -> int:
    parser = argparse.ArgumentParser(description="Run Teslatlas conformance cases")
    parser.add_argument("--profile", action="append", help="protocol profile; repeatable")
    parser.add_argument("--adapter", help="refused: conformance runs use no adapter executable")
    parser.add_argument("--config", help="private actual-Hub connection descriptor (hub-http-v1@1.2.0)")
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=10.0,
        help="maximum seconds for the complete actual-Hub process",
    )
    parser.add_argument("--json", action="store_true", help="emit one machine-readable summary")
    args = parser.parse_args()
    if not math.isfinite(args.timeout_seconds) or args.timeout_seconds <= 0:
        raise ConformanceError("timeout-seconds must be finite and positive")
    if args.timeout_seconds > TIMEOUT_MAX:
        raise ConformanceError("timeout-seconds exceeds the platform wait limit")

    if args.profile and HUB_HTTP_12_PROFILE in args.profile:
        # Offline profile checks always run; native cases only when a private actual-Hub
        # descriptor is supplied. The whole run is one owned process group with one deadline,
        # and the child keeps 20 percent of it back so it can report cases it had no time for.
        if args.profile != [HUB_HTTP_12_PROFILE]:
            raise ConformanceError("hub-http-v1@1.2.0 must run separately from other profiles")
        if args.adapter is not None:
            raise ConformanceError("hub-http-v1@1.2.0 uses no adapter executable; supply a private --config descriptor for native cases")
        command = [sys.executable, str(ROOT / "conformance/hub_http_12.py"), "--timeout-seconds", repr(args.timeout_seconds * 0.8)]
        if args.config:
            command += ["--config", args.config]
        if args.json:
            command += ["--json"]
        return run_actual_hub(command, args.timeout_seconds)

    if args.adapter is not None:
        raise ConformanceError("conformance runs use no adapter executable")
    selected = args.profile or [HUB_SYNC_PROFILE]
    unknown = [item for item in selected if item != HUB_SYNC_PROFILE]
    if unknown:
        raise ConformanceError(f"unknown profile: {', '.join(unknown)}")

    runs = 0
    passed = 0
    failures: list[dict[str, Any]] = []
    for result in hub_sync.run_fixture_cases():
        runs += 1
        if result["passed"]:
            passed += 1
            if not args.json:
                print(f"PASS {HUB_SYNC_PROFILE} {result['case_id']}")
        else:
            failures.append(
                {
                    "profile": HUB_SYNC_PROFILE,
                    "case_id": result["case_id"],
                    "fixture_ids": result["fixture_ids"],
                    "errors": result["errors"],
                    "expected_errors": result["expected_errors"],
                }
            )
            if not args.json:
                print(f"FAIL {HUB_SYNC_PROFILE} {result['case_id']}: {result['errors']}")

    summary = {
        "profiles": selected,
        "runs": runs,
        "passed": passed,
        "failed": runs - passed,
        "failures": failures,
    }
    if args.json:
        print(json.dumps(summary, sort_keys=True, separators=(",", ":"), allow_nan=False))
    else:
        print(f"profiles={len(selected)} runs={runs} passed={passed} failed={summary['failed']}")
    return 0 if summary["failed"] == 0 else 1


def main() -> int:
    try:
        return run()
    except (ConformanceError, OSError, subprocess.SubprocessError) as error:
        print(json.dumps({"error": str(error)}, separators=(",", ":")))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
