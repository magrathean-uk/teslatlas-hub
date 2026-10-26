#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 - "$root" <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

checkout = Path(sys.argv[1]).resolve()
source = (checkout / 'scripts/test-docker-packaging.sh').read_text()
begin = 'docker_context_target="${CARGO_TARGET_DIR:-$context_root/target}/docker-context-check"' 
end = '# Repository-selection variables'
assert source.count(begin) == source.count(end) == 1, 'Production routing block is not unique'
block = begin + source.split(begin, 1)[1].split(end, 1)[0]
fake = '''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
record={'argv':sys.argv[1:],'target':os.environ['CARGO_TARGET_DIR'],'source_commit':os.environ['TESLATLAS_HUB_SOURCE_COMMIT']}
Path(os.environ['FAKE_CARGO_LOG']).write_text(json.dumps(record))
target=Path(record['target']).resolve();owned=Path(os.environ['FAKE_CASE_ROOT']).resolve()
if os.path.commonpath([str(owned),str(target)])!=str(owned):raise SystemExit(97)
target.mkdir(parents=True,exist_ok=True);(target/'fake-cargo-output').write_text('fictional')
'''
results=[]
with tempfile.TemporaryDirectory(prefix='source-packaging-routing-') as temporary:
    root=Path(temporary)
    commands=root/'bin';commands.mkdir()
    cargo=commands/'cargo';cargo.write_text(fake);cargo.chmod(0o700)
    for name in ['managed','fallback','checkout','dotdot','symlink']:
        case=root/name;case.mkdir()
        context=case/'context';context.mkdir();(context/'Cargo.toml').write_text('[workspace]\n')
        log=case/'cargo.json'
        environment={**os.environ,'PATH':str(commands)+os.pathsep+os.environ['PATH'],
            'root':str(checkout),'context_root':str(context),'FAKE_CARGO_LOG':str(log),'FAKE_CASE_ROOT':str(case)}
        if name=='managed':environment['CARGO_TARGET_DIR']=str(case/'managed-target')
        elif name=='fallback':environment.pop('CARGO_TARGET_DIR',None)
        elif name=='checkout':environment['CARGO_TARGET_DIR']=str(checkout/'target')
        elif name=='dotdot':environment['CARGO_TARGET_DIR']=str(checkout/'..'/checkout.name/'target')
        else:
            alias=case/'alias';alias.symlink_to(checkout,target_is_directory=True)
            environment['CARGO_TARGET_DIR']=str(alias/'target')
        result=subprocess.run(['/bin/sh','-eu','-c',block],cwd=case,env=environment,capture_output=True)
        refused=name not in ['managed','fallback']
        if refused:
            assert result.returncode!=0,(name,'unsafe target was accepted')
            assert not log.exists(),(name,'Cargo was invoked before refusal')
            assert b'Docker context check target must be outside the checkout.' in result.stderr,(name,result.stderr)
            assert not list(case.rglob('fake-cargo-output')),(name,'refusal wrote target output')
        else:
            assert result.returncode==0,(name,result.stderr)
            record=json.loads(log.read_text())
            expected=(case/'managed-target' if name=='managed' else context/'target')/'docker-context-check'
            assert Path(record['target'])==expected.resolve(),(name,record)
            assert record['argv']==['check','--locked','--offline','--manifest-path',str(context/'Cargo.toml'),'--bin','teslatlas-hub','--quiet'],record
            assert record['source_commit']=='0123456789abcdef0123456789abcdef01234567',record
            assert (expected/'fake-cargo-output').is_file()
        results.append({'case':name,'refused_before_cargo':refused,'exit_code':result.returncode})
print(json.dumps({'status':'PASS','cases':results,'scope':'Extracted production block; fake Cargo only; no checkout output is ever written'}))
PY
