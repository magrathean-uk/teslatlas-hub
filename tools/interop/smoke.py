#!/usr/bin/env python3
"""Run current-profile raw-response acceptance against an owned native fixture."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from fixture import HTTP_PROFILE_ID, read_private_json, write_private_json

REFUSAL = 'current-Hub acceptance failed; inspect private fixture evidence'


def accepted(summary):
    """True only for a complete native hub-http-v1@1.2.0 run in which every check passed."""
    return (
        isinstance(summary, dict)
        and summary.get('profile_id') == HTTP_PROFILE_ID
        and summary.get('complete') is True
        and summary.get('failed') == 0
        and summary.get('not_run') == 0
        and type(summary.get('runs')) is int
        and summary['runs'] > 0
        and summary.get('passed') == summary['runs']
    )


def refuse():
    print(REFUSAL, file=sys.stderr)
    return 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--descriptor', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        # Validate the owner-only descriptor before handing its path to the
        # protocol runner. Only synthetic redacted summaries leave that tool.
        read_private_json(args.descriptor)
        protocol = Path(__file__).resolve().parents[2] / 'protocol'
        result = subprocess.run(
            [
                str(protocol / 'conformance/run'),
                '--profile', HTTP_PROFILE_ID,
                '--config', str(args.descriptor),
                '--timeout-seconds', '600',
                '--json',
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=660,
        )
        if result.returncode != 0:
            return refuse()
        summary = json.loads(result.stdout)
        if not accepted(summary):
            return refuse()
        write_private_json(args.output, summary)
    except (OSError, ValueError, subprocess.SubprocessError):
        return refuse()
    print(json.dumps({
        'status': 'passed',
        'runs': summary['runs'],
        'profile_sha256': summary.get('profile_sha256'),
        'receipt': str(args.output),
    }))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
