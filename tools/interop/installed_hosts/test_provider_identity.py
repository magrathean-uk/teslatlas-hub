# SPDX-License-Identifier: AGPL-3.0-only
"""Provider-only admission using real files and fixed private filesystem seams."""
import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from . import prepare, transport
from .contract import ContractError
from .test_platform_observation import registered


class ProviderIdentityTests(unittest.TestCase):
    def test_actual_prepare_main_accepts_ordinary_lima_alias_before_execute(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw).resolve();alias=root/'bin/limactl';target=root/'Cellar/lima/2.2.0/bin/limactl'
            alias.parent.mkdir();target.parent.mkdir(parents=True);target.write_bytes(b'synthetic lima executable');target.chmod(0o755)
            alias.symlink_to('../Cellar/lima/2.2.0/bin/limactl')
            r=registered();r.registration['provider_tool']={'path':'/opt/homebrew/bin/limactl','sha256':hashlib.sha256(target.read_bytes()).hexdigest()}
            for name in ('executable','config','identity_file'):
                p=root/name;p.write_bytes(name.encode());r.registration['ssh'][name]={'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
            for name in ('config.json','inventory.json'):(root/name).write_text('{}')
            calls=[]
            class Boundary:
                def __init__(self,registered):calls.append('constructed')
                def execute(self):calls.append('execute')
            def mapped(value):return alias if str(value)=='/opt/homebrew/bin/limactl' else Path(value)
            with mock.patch.object(transport,'Path',side_effect=mapped),mock.patch.object(prepare,'read_registered_config',return_value=r),mock.patch.object(prepare,'_verify_known_host',side_effect=lambda reg:calls.append('known-host')),mock.patch.object(prepare,'HostPreparer',Boundary):
                self.assertEqual(prepare.main(['--config',str(root/'config.json'),'--inventory',str(root/'inventory.json'),'--execute']),0)
            self.assertEqual(calls,['known-host','constructed','execute'])

    def test_generic_hash_regular_still_refuses_symlinks(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);target=root/'file';target.write_bytes(b'fixture');alias=root/'link';alias.symlink_to('file')
            with self.assertRaisesRegex(ContractError,'non-symlink'):
                transport._hash_regular({'path':str(alias),'sha256':hashlib.sha256(target.read_bytes()).hexdigest()},'fixture')

    def fixture(self,root,provider='lima-debian'):
        alias=root/'bin/limactl' if provider=='lima-debian' else root/'tart'
        target=root/'Cellar/lima/2.2.0/bin/limactl' if provider=='lima-debian' else alias
        target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(b'synthetic executable');target.chmod(0o755)
        if provider=='lima-debian':
            alias.parent.mkdir(parents=True,exist_ok=True);alias.symlink_to('../Cellar/lima/2.2.0/bin/limactl')
        fixed=transport._PROVIDER_TOOL_PATHS[provider]
        reg={'provider':provider,'provider_tool':{'path':fixed,'sha256':hashlib.sha256(target.read_bytes()).hexdigest()}}
        def mapped(value):return alias if str(value)==fixed else Path(value)
        return alias,target,reg,mapped

    def test_lima_identity_keeps_alias_and_revalidates_exact_single_target(self):
        from dataclasses import FrozenInstanceError
        with tempfile.TemporaryDirectory() as raw:
            alias,target,r,mapped=self.fixture(Path(raw).resolve())
            with mock.patch.object(transport,'Path',side_effect=mapped):
                observed=transport._verify_provider_tool(r)
                self.assertEqual(observed.invocation_path,'/opt/homebrew/bin/limactl')
                self.assertEqual(observed.resolved_path,str(target));self.assertEqual(observed.link_text,'../Cellar/lima/2.2.0/bin/limactl')
                self.assertEqual(observed.revalidate(),observed)
                self.assertEqual(observed.as_dict()['sha256'],r['provider_tool']['sha256'])
                with self.assertRaises(FrozenInstanceError):observed.invocation_path=str(target)

    def test_fixed_regular_tart_accepted_and_tart_symlink_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            alias,target,r,mapped=self.fixture(Path(raw).resolve(),'tart-macos')
            with mock.patch.object(transport,'Path',side_effect=mapped):
                observed=transport._verify_provider_tool(r);self.assertIsNone(observed.link_text)
                target.rename(target.with_name('real-tart'));alias.symlink_to('real-tart')
                with self.assertRaisesRegex(ContractError,'non-symlink'):transport._verify_provider_tool(r)

    def test_wrong_target_missing_target_and_multihop_are_rejected(self):
        for case in ('other-formula','absolute','traversal','different-name','dangling','chain','parent-link','non-executable','regular-alias'):
            with self.subTest(case=case),tempfile.TemporaryDirectory() as raw:
                alias,target,r,mapped=self.fixture(Path(raw).resolve())
                if case=='other-formula':alias.unlink();alias.symlink_to('../Cellar/other/2.2.0/bin/limactl')
                elif case=='absolute':alias.unlink();alias.symlink_to(str(target))
                elif case=='traversal':alias.unlink();alias.symlink_to('../Cellar/lima/2.2.0/bin/../bin/limactl')
                elif case=='different-name':alias.unlink();alias.symlink_to('../Cellar/lima/2.2.0/bin/other')
                elif case=='dangling':target.unlink()
                elif case=='chain':target.rename(target.with_name('real'));target.symlink_to('real')
                elif case=='parent-link':target.parent.rename(target.parent.with_name('real-bin'));target.parent.symlink_to('real-bin')
                elif case=='non-executable':target.chmod(0o600)
                elif case=='regular-alias':alias.unlink();alias.write_bytes(target.read_bytes());alias.chmod(0o755)
                with mock.patch.object(transport,'Path',side_effect=mapped):
                    with self.assertRaises(ContractError):transport._verify_provider_tool(r)

    def test_wrong_provider_path_digest_and_changed_target_bytes_are_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            alias,target,r,mapped=self.fixture(Path(raw).resolve())
            with mock.patch.object(transport,'Path',side_effect=mapped):
                observed=transport._verify_provider_tool(r)
                target.write_bytes(b'changed executable')
                with self.assertRaisesRegex(ContractError,'digest'):observed.revalidate()
                r['provider_tool']['path']=str(target)
                with self.assertRaisesRegex(ContractError,'fixed'):transport._verify_provider_tool(r)

    def test_alias_replacement_with_same_digest_after_observation_is_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            alias,target,r,mapped=self.fixture(Path(raw).resolve())
            replacement=target.parents[2]/'2.2.1/bin/limactl';replacement.parent.mkdir(parents=True);replacement.write_bytes(target.read_bytes());replacement.chmod(0o755)
            with mock.patch.object(transport,'Path',side_effect=mapped):
                observed=transport._verify_provider_tool(r)
                alias.unlink();alias.symlink_to('../Cellar/lima/2.2.1/bin/limactl')
                with self.assertRaisesRegex(ContractError,'changed after observation'):observed.revalidate()

    def test_alias_change_during_hashing_is_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            alias,target,r,mapped=self.fixture(Path(raw).resolve());real_open=os.open
            def swap(path,flags,*args,**kwargs):
                fd=real_open(path,flags,*args,**kwargs)
                if str(path)==str(target):alias.unlink();alias.symlink_to('../Cellar/lima/2.2.9/bin/limactl')
                return fd
            with mock.patch.object(transport,'Path',side_effect=mapped),mock.patch.object(transport.os,'open',side_effect=swap):
                with self.assertRaisesRegex(ContractError,'changed during observation'):transport._verify_provider_tool(r)

    def test_cli_revalidates_alias_after_other_preflight_before_execute(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw).resolve();alias,target,provider,mapped=self.fixture(root)
            r=registered();r.registration.update(provider)
            for name in ('executable','config','identity_file'):
                path=root/name;path.write_bytes(name.encode());r.registration['ssh'][name]={'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
            for name in ('config.json','inventory.json'):(root/name).write_text('{}')
            calls=[]
            def replace_alias(reg):alias.unlink();alias.symlink_to('../Cellar/lima/9.9.9/bin/limactl')
            class Boundary:
                def __init__(self,registered):pass
                def execute(self):calls.append('guest-side-effect')
            with mock.patch.object(transport,'Path',side_effect=mapped),mock.patch.object(prepare,'read_registered_config',return_value=r),mock.patch.object(prepare,'_verify_known_host',side_effect=replace_alias),mock.patch.object(prepare,'HostPreparer',Boundary):
                with self.assertRaises(ContractError):prepare.main(['--config',str(root/'config.json'),'--inventory',str(root/'inventory.json'),'--execute'])
            self.assertEqual(calls,[])

    def test_expired_caller_deadline_prevents_provider_read(self):
        from .bounded import Deadline
        deadline=Deadline(1);deadline.end=0
        with mock.patch.object(transport,'Path') as path:
            with self.assertRaises(TimeoutError):transport._verify_provider_tool({},deadline)
        path.assert_not_called()

    def test_real_cli_process_validates_registration_and_provider_before_guest_boundary(self):
        import subprocess,sys,time
        for case in ('valid','wrong-digest','generic-ssh-symlink'):
            with self.subTest(case=case),tempfile.TemporaryDirectory() as raw:
                started=time.time();monotonic=time.monotonic()
                child_argv=[sys.executable,'-B','-c',
                    'import sys; from tools.interop.installed_hosts.test_provider_identity import _cli_fixture; raise SystemExit(_cli_fixture(sys.argv[1],sys.argv[2]))',raw,case]
                result=subprocess.run(child_argv,capture_output=True,timeout=5)
                retained=os.environ.get('PROVIDER_TEST_EVIDENCE_DIR')
                if retained:
                    evidence=Path(retained);evidence.mkdir(parents=True,exist_ok=True)
                    record={'argv':child_argv,'cwd':os.getcwd(),'started_unix':started,'elapsed_seconds':time.monotonic()-monotonic,'exit':result.returncode,'stdout':result.stdout.decode(),'stderr':result.stderr.decode(),'environment':{key:os.environ[key] for key in ('PATH','HOME','TMPDIR','PYTHONDONTWRITEBYTECODE','PYTHONWARNINGS','PROVIDER_TEST_EVIDENCE_DIR') if key in os.environ},'inputs':{p.name:p.read_text() for p in Path(raw).glob('*.json')}}
                    (evidence/(case+'.json')).write_text(json.dumps(record,indent=2)+'\n')
                if case=='valid':
                    self.assertEqual(result.returncode,0,result.stderr.decode())
                    value=json.loads((Path(raw)/'guest-boundary.json').read_text())
                    self.assertEqual(value['invocation_path'],'/opt/homebrew/bin/limactl')
                    self.assertEqual(value['link_text'],'../Cellar/lima/2.2.0/bin/limactl')
                else:
                    self.assertEqual(result.returncode,1,result.stderr.decode())
                    self.assertIn(b'ContractError',result.stderr)
                    self.assertFalse((Path(raw)/'guest-boundary.json').exists())
                    self.assertFalse((Path(raw)/'private').exists())


def _cli_fixture(raw,case):
    """Child-process CLI fixture: real schemas/registration/keys, no guest calls."""
    import base64
    from .test_session import inputs
    from . import lease
    root=Path(raw).resolve();cfg,inventory=inputs(root)
    alias=root/'bin/limactl';target=root/'Cellar/lima/2.2.0/bin/limactl'
    alias.parent.mkdir();target.parent.mkdir(parents=True);target.write_bytes(b'synthetic executable');target.chmod(0o755)
    alias.symlink_to('../Cellar/lima/2.2.0/bin/limactl')
    registration_path=Path(cfg['host_registration']['path']);reg=json.loads(registration_path.read_text())
    reg['install_state']='preinstall';reg['guest']['permitted_service_uid']=None
    reg['provider_tool']['sha256']=hashlib.sha256(target.read_bytes()).hexdigest() if case!='wrong-digest' else '0'*64
    mappings={'/opt/homebrew/bin/limactl':alias}
    ssh=root/'ssh';ssh.write_bytes(b'synthetic SSH executable');mappings['/usr/bin/ssh']=ssh
    reg['ssh']['executable']['sha256']=hashlib.sha256(ssh.read_bytes()).hexdigest()
    for name in ('config','identity_file'):
        path=root/('ssh-'+name);path.write_bytes(name.encode())
        reg['ssh'][name]={'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
    if case=='generic-ssh-symlink':
        path=Path(reg['ssh']['config']['path']);path.rename(path.with_name('ssh-config-real'));path.symlink_to('ssh-config-real')
    key=b'synthetic-only-public-key';known=root/'known-hosts'
    known.write_text(reg['ssh']['host_alias']+' ssh-ed25519 '+base64.b64encode(key).decode()+'\n')
    reg['ssh']['known_hosts']={'path':str(known),'sha256':hashlib.sha256(known.read_bytes()).hexdigest()};reg['ssh']['host_key_sha256']=hashlib.sha256(key).hexdigest()
    registration_path.write_text(json.dumps(reg));cfg['host_registration']['sha256']=hashlib.sha256(registration_path.read_bytes()).hexdigest()
    inventory[reg['host_id']]=cfg['host_registration']['sha256']
    config_path=root/'cli-config.json';config_path.write_text(json.dumps(cfg));inventory_path=root/'cli-inventory.json';inventory_path.write_text(json.dumps(inventory))
    def mapped(value):return mappings.get(str(value),Path(value))
    def guest_boundary(self):
        (root/'guest-boundary.json').write_text(json.dumps(self.provider_identity.as_dict()))
        return {}
    with mock.patch.object(transport,'Path',side_effect=mapped),mock.patch.object(lease,'LOCK_ROOT',root/'locks'),mock.patch.object(prepare.HostPreparer,'_execute_locked',guest_boundary):
        return prepare.main(['--config',str(config_path),'--inventory',str(inventory_path),'--execute'])
