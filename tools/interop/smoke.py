#!/usr/bin/env python3
"""Run current-profile raw-response acceptance against an owned native fixture."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from fixture import read_private_json, write_private_json


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--descriptor',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    # Validate owner-only descriptor before handing its path to the independent
    # Apache protocol adapter. Only synthetic redacted receipts leave that tool.
    read_private_json(args.descriptor)
    protocol=Path(__file__).resolve().parents[3]/'teslatlas-protocol'
    result=subprocess.run([str(protocol/'conformance/run'),'--profile','hub-http-v1@1.0.0',
        '--adapter',str(protocol/'conformance/adapters/actual-hub'),'--config',str(args.descriptor),'--json'],
        stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
    if result.returncode:
        print('current-Hub acceptance failed; inspect private fixture evidence',file=sys.stderr)
        return 1
    receipt=json.loads(result.stdout)
    if receipt.get('status')!='passed' or receipt.get('profile_id')!='hub-http-v1@1.0.0':
        raise ValueError('profile acceptance receipt missing')
    write_private_json(args.output,receipt)
    print(json.dumps({'status':'passed','case_count':receipt['runs'],'profile_sha256':receipt['profile_sha256'],'receipt':str(args.output)}))
    return 0

if __name__=='__main__':
    try:
        raise SystemExit(main())
    except (OSError,ValueError,subprocess.SubprocessError):
        print('current-Hub acceptance failed; inspect private fixture evidence',file=sys.stderr)
        raise SystemExit(1)
