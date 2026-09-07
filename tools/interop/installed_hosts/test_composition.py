# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded real-method compositions; OS/SSH seams use synthetic fixtures only."""
import copy
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from . import macos, guest
from .bounded import Deadline, run_capped
from .transport import SSHTransport
from .test_platform_observation import observation, registered, process
from .test_session import FakeTransport, inputs
from .session import InstalledSession, CleanupError, SessionError


class CompositionTests(unittest.TestCase):
    def test_real_writer_status_255_and_signal_are_not_remote_completion(self):
        for code in ('raise SystemExit(255)', 'import os,signal; os.kill(os.getpid(), signal.SIGTERM)'):
            with self.subTest(code=code):
                t = SSHTransport()
                t.process = subprocess.Popen([sys.executable, '-B', '-c', code], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    t.process.wait(timeout=2)
                    self.assertFalse(t.finish_writer(0.1))
                    t._reap_process(0.1)
                    self.assertFalse(t.finish_writer(0.1))
                finally:
                    for stream in (t.process.stdin,t.process.stdout,t.process.stderr): stream.close()

    def test_recovery_timeout_blocks_next_recovery_and_final_verifier(self):
        t=SSHTransport(); t.registered=registered(); t.paths={'entrypoint':'/fixture/guest.py','session':'/fixture/session.json'}
        t.process=subprocess.Popen([sys.executable,'-c','pass'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        t.process.wait(timeout=2)
        try:
            self.assertTrue(t.finish_writer(0.1))
            # Actual run_capped launches only this bounded local process. No SSH.
            t._argv=lambda *args:[sys.executable,'-c','import time;time.sleep(2)']
            with self.assertRaises(TimeoutError):t.recover_stop(t.registered,0.15)
            self.assertFalse(t.finish_writer(0.1))
            with self.assertRaisesRegex(RuntimeError,'writer'):
                t.recover_stop(t.registered,0.2)
            with self.assertRaisesRegex(RuntimeError,'writer'):
                t.verify_stopped(t.registered,0.2)
        finally:
            for stream in (t.process.stdin,t.process.stdout,t.process.stderr):stream.close()

    def test_guest_verify_preserves_mac_private_acquisition(self):
        with tempfile.TemporaryDirectory() as raw:
            r=registered('macOS'); p=macos.MacOSController(r,{'ownership_path':raw+'/ownership.json'})
            c=guest.GuestController(r,p); proof=observation('macOS')
            c.seed_facts={'store_id':proof['config']['store_id'],'store_schema_version':proof['config']['store_schema_version']}
            helper=process(42,501,40,'/bin/sleep','1001:1')
            c.owned_service=dict(proof['service'],acquisition_state='complete',transient_helpers=[helper])
            p.observe=lambda:copy.deepcopy(proof);p.paired_device_ids=lambda:set()
            c._observe({'sequence':1,'challenge':'a'*64})
            owned=json.loads(c.ownership_path.read_text())
            self.assertEqual(owned['service']['acquisition_state'],'complete')
            self.assertEqual(p.reconcile_owned(owned)['transient_helpers'],[helper])

    def test_short_mac_stop_budget_bounds_observer_and_join(self):
        r=registered('macOS');p=macos.MacOSController(r,{})
        s=observation('macOS')['service'];p.app_process=s['app_process']
        p._launch_status=lambda required=True:(40,'loaded')
        p._hub_child=lambda *args:s['hub']
        p._run=lambda *args,**kwargs:subprocess.CompletedProcess([],0,b'',b'')
        p.set_deadline(Deadline(0.15));started=time.monotonic()
        with mock.patch.object(macos,'_process',return_value=s['supervisor']),mock.patch.object(macos,'_generation_present',return_value=True):
            with self.assertRaises((RuntimeError,TimeoutError)):p.stop()
        self.assertLess(time.monotonic()-started,0.45)
        self.assertIsNone(p._last_stop)

    def test_start_dispatch_marks_pending_before_lost_reply(self):
        class Lost(FakeTransport):
            def exchange(self,request,timeout):
                result=super().exchange(request,timeout)
                if request['op']=='start':raise OSError('lost after acquisition B')
                return result
        with tempfile.TemporaryDirectory() as raw:
            t=Lost();s=InstalledSession.open(*inputs(Path(raw)),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'});s.request({'op':'stop'})
                with self.assertRaises(OSError):s.request({'op':'start'})
                self.assertTrue(s._service_outstanding)
                self.assertEqual(s._pending_acquisition['sequence'],3)
            finally:
                try:s.close()
                except CleanupError:pass
                s.journal.close();s._host_lease.release()

    def test_actual_process_identity_sysctl_subprocess_obeys_short_deadline(self):
        from ._common import run_checked
        calls=[]
        def slow_boot(argv, timeout, allowed_status=(0,), deadline=None):
            calls.append((argv, deadline))
            return run_checked([sys.executable,'-c','import time;time.sleep(3)'], timeout, deadline=deadline)
        deadline=Deadline(0.16);started=time.monotonic()
        # Actual libproc identity and argv reads for this test process; only the
        # fixed sysctl subprocess is replaced with a slow bounded local child.
        with mock.patch.object(macos,'run_checked',side_effect=slow_boot):
            with self.assertRaises(TimeoutError):macos._process(os.getpid(),deadline=deadline)
        self.assertIs(calls[0][1],deadline)
        self.assertLess(time.monotonic()-started,0.4)

    def test_real_mac_guest_verify_eof_and_fresh_helper_stop(self):
        for fresh in (False,True):
            with self.subTest(fresh=fresh), tempfile.TemporaryDirectory() as raw:
                root=Path(raw);r=registered('macOS');r.config['allowed_origins']=[]
                r.registration['guest']['python_toml_module']='tomllib'
                private={'ownership_path':str(root/'ownership.json'),'controller_root':raw,'guest_inputs':{key:str(root/key) for key in ('package_manifest','seed','scenario','profile')}}
                p=macos.MacOSController(r,private);c=guest.GuestController(r,p)
                proof=observation('macOS');s=proof['service'];p.app_process=s['app_process'];p.connection={'hub_id':proof['config']['store_id']}
                c.seed_facts={'store_id':proof['config']['store_id'],'store_schema_version':59}
                c.cleanup_required=True;c.state='running';c.challenge='a'*64
                p.config_path=str(root/'config');p.plist_path=str(root/'plist')
                Path(p.config_path).write_text('synthetic')
                import plistlib
                plist_args=[macos.WRAPPER,'--config',p.config_path,'--stdout-log',p.home+'/Library/Logs/Teslatlas Hub/hub.out.log','--stderr-log',p.home+'/Library/Logs/Teslatlas Hub/hub.err.log']
                Path(p.plist_path).write_bytes(plistlib.dumps({'Label':macos.LABEL,'ProgramArguments':plist_args}))
                helper=process(42,501,40,'/bin/sleep','1001:1')
                loaded=[True];helper_read=[False]
                context=dict(proof['config'],path=p.config_path)
                p.current_context=lambda ownership=None:context
                p.paired_device_ids=lambda:set()
                p._receipt=lambda:({'package_sha256':'d'*64,'package_manifest_sha256':'e'*64,'installed_version':'2026.36.2','payload_manifest_sha256':'e'*64},'f'*64)
                p._listener=lambda pid:proof['listener']
                def run(argv,timeout=15,allowed_status=(0,)):
                    status=0;out=b''
                    if argv[0].endswith('launchctl'):
                        if argv[1]=='bootout':loaded[0]=False
                        elif loaded[0]:out=b'pid = 40\n'
                        else:status=3
                    elif argv[0].endswith('pgrep'):
                        out=b'41\n42\n' if not helper_read[0] else b'41\n'
                    elif argv[0].endswith('sw_vers'):out=b'13.7.4\n'
                    elif argv[-1]=='-m':out=b'arm64\n'
                    elif argv[0].endswith('lsof'):status=1
                    return subprocess.CompletedProcess(argv,status,out,b'')
                p._run=run
                def proc(pid,expected_argv=None,deadline=None):
                    if pid==42:
                        if helper_read[0]:raise macos.ProcessAbsent('synthetic helper completed')
                        helper_read[0]=True;return helper
                    return s['supervisor'] if pid==40 else s['hub'] if pid==41 else proof['observer']['process']
                c._record_acquisition(dict(s,acquisition_state='complete',transient_helpers=[]))
                # All product/OS reads are fixture-backed; observe, _hub_child,
                # acquisition persistence, assert_owned, stop, and stopped proof
                # validators execute their actual production methods.
                with mock.patch.object(macos,'_process',side_effect=proc),mock.patch.object(macos,'_generation_present',side_effect=lambda identity:loaded[0] and identity['pid']!=42),mock.patch.object(macos,'loads_toml',return_value={'data_dir':p.data_root+'/hub'}),mock.patch.object(macos,'validate_synthetic_config',return_value={'public_url':'https://127.0.0.1:18480'}),mock.patch.object(macos,'verify_payload_members',return_value='e'*64),mock.patch.object(macos,'normal_tls_probe',return_value={'hub_id':proof['config']['store_id'],'certificate_der_sha256':'1'*64,'response_sha256':'2'*64,'validated_response_set_sha256':'3'*64}),mock.patch.object(macos,'sha256_file',return_value='a'*64):
                    c._observe({'sequence':1,'challenge':'a'*64})
                    canonical=json.loads(c.ownership_path.read_text())['service']
                    self.assertEqual(canonical['transient_helpers'],[helper])
                    self.assertEqual(canonical['acquisition_state'],'complete')
                    if fresh:
                        out=[]
                        with mock.patch.object(guest,'_load_private',return_value=(r,private)),mock.patch.object(guest,'_platform',return_value=p),mock.patch.object(guest,'_write',side_effect=lambda value,deadline=None:out.append(value)):
                            guest.main(['--recover-stop','fixture','--budget-ms','5000'])
                        final=out[0]
                    else:
                        final,errors=c.close();self.assertEqual(errors,[])
                    owned=json.loads(c.ownership_path.read_text())
                    final=p.verify_stopped(owned=owned)
                    self.assertTrue(final['service']['normal_exit'])
                    stopped=final['service']['owned_generation']
                    self.assertEqual(stopped['service']['transient_helpers'],[helper])
                    self.assertEqual(stopped['stop_evidence']['transient_helpers'],[helper])
                    from .session import _validate_mac_stop_evidence
                    _validate_mac_stop_evidence(stopped['service'],stopped['stop_evidence'],r.config['session_id'])

    def test_real_wrapper_only_fresh_recovery_has_no_invented_hub_or_normal_stop(self):
        with tempfile.TemporaryDirectory() as raw:
            r=registered('macOS');private={'ownership_path':raw+'/ownership.json'};p=macos.MacOSController(r,private)
            s=observation('macOS')['service'];partial={'acquisition_state':'wrapper-acquired','supervisor':s['supervisor'],'app_process':s['app_process']}
            owned={'session_id':r.config['session_id'],'lease':r.registration['lease'],'service':partial,'config_path':p.config_path,'config':{'store_id':'store'},'store_id':'store','state':'failed','stop_evidence':None,'sequence':2,'challenge':'a'*64}
            Path(private['ownership_path']).write_text(json.dumps(owned));loaded=[True]
            p.current_context=lambda ownership=None:{'store_id':'store'}
            def run(argv,timeout=15,allowed_status=(0,)):
                if argv[:2]==['/bin/launchctl','bootout']:loaded[0]=False
                if argv[:2]==['/bin/launchctl','print']:return subprocess.CompletedProcess(argv,0 if loaded[0] else 3,b'pid = 40\n' if loaded[0] else b'',b'')
                return subprocess.CompletedProcess(argv,1,b'',b'')
            p._run=run;out=[]
            with mock.patch.object(macos,'_process',return_value=s['supervisor']),mock.patch.object(macos,'_generation_present',side_effect=lambda i:loaded[0]),mock.patch.object(guest,'_load_private',return_value=(r,private)),mock.patch.object(guest,'_platform',return_value=p),mock.patch.object(guest,'_write',side_effect=lambda value,deadline=None:out.append(value)):
                guest.main(['--recover-stop','fixture','--budget-ms','1000'])
            evidence=out[0]['service'];self.assertFalse(evidence['normal_exit'])
            acquired=evidence['owned_generation'];self.assertIsNone(acquired['service'].get('hub'))
            self.assertTrue(macos.validate_wrapper_cleanup(acquired['service'],acquired['stop_evidence'],r.config['session_id']))


class SyntheticPlatform:
    """OS fixture; real GuestController drives and persists its generations."""
    acquisition_callback=None
    context_callback=None
    def __init__(self,r,private):
        self.r=r;self.private=private;self.config_path='/fixture/config';self.generation=0
        self.running=False;self.fail_stop=False;self.stops=[];self.deadline=None
    def set_deadline(self,d):self.deadline=d
    def preflight(self):return {'stopped':True}
    def prepare(self):
        t=FakeTransport();t.registered=self.r
        values=t.exchange({'sequence':1,'challenge':'a'*64,'op':'verify'},1)['result']
        return {'invitation':values['invitation'],'expired_invitation':values['expired_invitation'],'seed_facts':{'store_id':'hub-uuid-1','store_schema_version':59,'scenario_sha256':'3'*64}}
    def paired_device_ids(self):return set()
    def current_context(self):return observation()['config']
    def start(self):
        self.acquisition_callback({'acquisition_state':'pending-start'})
        self.running=True;self.generation+=1
        self.acquisition_callback(self.capture_service())
    def capture_service(self):return dict(self.observe()['service'],acquisition_state='complete')
    def observe(self):
        result=observation(start=str(self.generation)+':1');result['host']['host_id']=self.r.config['host_id']
        return result
    def assert_owned(self,owned):
        if owned['service']['hub']!=self.capture_service()['hub']:raise RuntimeError('synthetic generation replacement')
        if owned.get('stop_evidence') is not None:self.verify_stopped(owned)
    def bind_recovery(self,owned):self.assert_owned(owned)
    def reconcile_owned(self,owned):self.assert_owned(owned);return owned['service']
    def stop(self):
        self.stops.append(self.generation)
        if self.fail_stop:raise RuntimeError('synthetic first B cleanup failure')
        self.running=False;s=self.capture_service()
        return {'operation':'stop','session_id':self.r.config['session_id'],'pid':s['hub']['pid'],'hub_start_identity':s['hub']['start_identity'],'control_group':s['control_group'],'invocation_id':s['invocation_id'],'result_raw':'success','exec_main_code_raw':'1','exec_main_code_semantic':'exited','exec_main_status_raw':'0','normal_exit':True}
    def verify_stopped(self,owned=None,initial=False):
        if self.running:raise RuntimeError('synthetic generation remains running')
        if not owned or not owned.get('stop_evidence'):raise RuntimeError('synthetic stop evidence missing')
        return {'schema_version':1,'status':'stopped','session_id':self.r.config['session_id'],'host_id':self.r.config['host_id'],'service':{'state':'stopped','generation':None,'forced_escalation':False,'normal_exit':True,'owned_generation':{'service':owned['service'],'stop_evidence':owned['stop_evidence']},'cleanup_errors':[]},'listener':{'host':'127.0.0.1','port':18480,'owner_pid':None}}


class FixtureSSH(SSHTransport):
    """Actual transport lifecycle; fixed SSH filesystem boundary uses local files."""
    def __init__(self,root,exit_status=0):
        super().__init__();self.fixture=root;self.exit_status=exit_status;self.guest_closed=False
        self.fail_start_reply=False;self.invalid_start_reply=False;self.fail_stop_reply=False
        self.recover_failures=0;self.verifiers=0;self.disposals=0;self.recovers=0
    def open(self,r):
        self.registered=r;remote=self.fixture/'remote';remote.mkdir()
        self.paths={'root':str(remote),'archive':str(remote/'controller.tar'),'session':str(remote/'session.json'),'entrypoint':str(remote/'guest.py')}
        for name in ('controller.tar','session.json','guest.py'):(remote/name).write_text('synthetic')
        (remote/'guest.journal.jsonl').write_text('{"operation":"preflight"}\n')
        private={'ownership_path':str(remote/'ownership.json'),'controller_root':str(remote)}
        self.platform=SyntheticPlatform(r,private)
        def journal(value):
            with (remote/'guest.journal.jsonl').open('a') as f:f.write(json.dumps(value)+'\n')
        self.guest=guest.GuestController(r,self.platform,journal=journal)
        self.process=subprocess.Popen([sys.executable,'-c','import sys;sys.stdin.read();raise SystemExit('+str(self.exit_status)+')'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self._root_created=True;self._setup_complete=True
        self.public_certificate_path=str(self.fixture/'public.pem');Path(self.public_certificate_path).write_text('fixture')
        self.public_certificate_der_sha256='1'*64
        return self.guest.open()
    def exchange(self,request,timeout):
        result=self.guest.handle(request,Deadline(timeout))
        if request['op']=='start':
            if self.fail_start_reply:
                self.platform.fail_stop=True
                raise OSError('lost start reply after B exists')
            if self.invalid_start_reply:
                self.platform.fail_stop=True
                result['result']['proof']['sequence']+=1
        if request['op']=='stop' and self.fail_stop_reply:raise OSError('lost processed stop reply')
        return result
    def verify_local_tunnel(self,proof,timeout):pass
    def finish_writer(self,timeout):
        if not self.guest_closed:
            self.guest.close();self.guest_closed=True
        return super().finish_writer(timeout)
    def _verify_remote_bundle_members(self,deadline=None):pass
    def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False):
        import shlex, shutil
        if '--recover-stop' in command or '--verify-stopped' in command:
            recovery='--recover-stop' in command
            if recovery:
                self.recovers+=1
                if self.recover_failures:
                    self.recover_failures-=1
                    raise RuntimeError('conclusive synthetic recovery service failure')
                self.platform.fail_stop=False
            else:self.verifiers+=1
            out=[]
            # Run actual private entrypoint and durable ownership logic. Native
            # service/process observations remain the SyntheticPlatform fixture.
            with mock.patch.object(guest,'_load_private',return_value=(self.registered,self.platform.private)),mock.patch.object(guest,'_platform',return_value=self.platform),mock.patch.object(guest,'_write',side_effect=lambda value,deadline=None:out.append(value)):
                guest.main(['--recover-stop' if recovery else '--verify-stopped','fixture','--budget-ms',str(max(1,int(timeout*1000)))])
            return json.dumps(out[0]).encode()
        if command.startswith('/bin/cat '):return Path(shlex.split(command)[1]).read_bytes()
        if 'sha256sum' in command:return (self.registered.config['controller_bundle']['sha256']+'  fixture\n').encode()
        if '/bin/rm -f --' in command:
            self.disposals+=1;shutil.rmtree(self.paths['root']);return b''
        if command.startswith('/usr/bin/find '):return b''
        raise AssertionError('unhandled fixed fixture command: '+command)


class LifecycleCompositionTests(unittest.TestCase):
    def test_stop_A_start_B_lost_or_rejected_reply_failed_guest_cleanup_and_retry(self):
        for invalid in (False,True):
            with self.subTest(invalid=invalid),tempfile.TemporaryDirectory() as raw:
                root=Path(raw);t=FixtureSSH(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
                try:
                    s.request({'op':'verify'});s.request({'op':'stop'})
                    t.invalid_start_reply=invalid;t.fail_start_reply=not invalid;t.recover_failures=1
                    with self.assertRaises((OSError,SessionError)):s.request({'op':'start'})
                    b=json.loads(Path(t.platform.private['ownership_path']).read_text())['service']
                    self.assertEqual(b['acquisition_intent'],s._pending_acquisition)
                    self.assertEqual(b['hub']['start_identity'],'2:1')
                    with mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                        with self.assertRaises(CleanupError):s.close()
                        self.assertTrue(t.platform.running);self.assertTrue(s._service_outstanding)
                        self.assertFalse(t._root_disposed)
                        with self.assertRaises(CleanupError) as result:s.close()
                    self.assertEqual(result.exception.evidence.state,'failed')
                    self.assertTrue(s._cleanup_complete);self.assertFalse(t.platform.running)
                    self.assertEqual(result.exception.evidence.final_stopped['service']['owned_generation']['service']['hub']['start_identity'],'2:1')
                    self.assertEqual(t.recovers,2);self.assertEqual(t.disposals,1)
                finally:
                    s.journal.close();s._host_lease.release()
                    t._reap_process(0.2)

    def test_lost_processed_stop_reply_retains_actual_sequence(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=FixtureSSH(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'});t.fail_stop_reply=True
                with self.assertRaises(OSError):s.request({'op':'stop'})
                exact=json.loads(Path(t.platform.private['ownership_path']).read_text())['stop_evidence']
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                    with self.assertRaises(CleanupError) as result:s.close()
                self.assertTrue(s._cleanup_complete)
                retained=result.exception.evidence.final_stopped['service']['owned_generation']['stop_evidence']
                self.assertEqual(retained,exact);self.assertEqual(retained['operation_sequence'],2)
                self.assertEqual(t.platform.stops,[1])
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)

    def test_session_transport_listener_retry_after_real_root_deletion(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=FixtureSSH(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'})
                probes=[subprocess.CompletedProcess([],0,b'p999\n',b''),subprocess.CompletedProcess([],1,b'',b'')]
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',side_effect=lambda *args,**kwargs:probes.pop(0)):
                    with self.assertRaises(CleanupError) as first:s.close()
                    self.assertFalse(Path(t.paths['root']).exists());self.assertTrue((s.journal.path.parent/'final-stopped.json').is_file())
                    stages=[x for e in first.exception.evidence.cleanup_errors for x in e.get('stages',[])]
                    self.assertIn('local-listener',[x['resource'] for x in stages])
                    self.assertTrue(any('survived' in x['message'] for x in stages))
                    verifiers=t.verifiers
                    with self.assertRaises(CleanupError):s.close()
                self.assertEqual(t.verifiers,verifiers);self.assertEqual(t.disposals,1);self.assertTrue(s._cleanup_complete)
                self.assertEqual(s.state,'failed')
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)

    def test_conclusive_guest_exit_one_completes_resources_and_permanently_fails_row(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=FixtureSSH(root,exit_status=1);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'})
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                    with self.assertRaises(CleanupError) as first:s.close()
                    counts=(t.verifiers,t.disposals)
                    with self.assertRaises(CleanupError):s.close()
                self.assertTrue(s._cleanup_complete);self.assertTrue(t._closed)
                self.assertEqual(counts,(t.verifiers,t.disposals));self.assertEqual(first.exception.evidence.state,'failed')
                self.assertIn({'resource':'guest-exit','error':'GuestExitStatus','status':1},first.exception.evidence.cleanup_errors)
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)

    def test_actual_recovery_timeout_in_session_blocks_retry_writers_and_verifier(self):
        class SlowRecovery(FixtureSSH):
            def _argv(self,*args):return [sys.executable,'-c','import time;time.sleep(2)']
            def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False):
                if '--recover-stop' in command:
                    self.recovers+=1
                    return SSHTransport._run_remote(self,command,timeout,deadline=deadline,writer=writer)
                return super()._run_remote(command,timeout,input_bytes,deadline,writer)
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=SlowRecovery(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'});s._had_failure=True;t.platform.fail_stop=True
                s._cleanup_stage_budgets.update(service=0.5,verify=0.3,transport=0.5)
                def local_only(argv,*args,**kwargs):
                    if argv[0]==sys.executable:return run_capped(argv,*args,**kwargs)
                    return subprocess.CompletedProcess(argv,1,b'',b'')
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',side_effect=local_only):
                    with self.assertRaises(CleanupError):s.close()
                    with self.assertRaises(CleanupError):s.close()
                self.assertEqual(t.recovers,1);self.assertEqual(t.verifiers,0);self.assertEqual(t.disposals,0)
                self.assertTrue(t._mutation_unproved);self.assertTrue(Path(t.paths['root']).is_dir())
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)

    def test_durable_stopped_checkpoint_failure_preserves_remote_inputs(self):
        from .bounded import durable_bytes
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=FixtureSSH(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'})
                def fail_checkpoint(path,raw):
                    if Path(path).name=='final-stopped.json':raise OSError('synthetic directory fsync failure')
                    return durable_bytes(path,raw)
                with mock.patch('tools.interop.installed_hosts.session.durable_bytes',side_effect=fail_checkpoint),mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                    with self.assertRaises(CleanupError):s.close()
                self.assertIsNone(s._validated_stopped);self.assertTrue(Path(t.paths['root']).is_dir());self.assertEqual(t.disposals,0)
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                    with self.assertRaises(CleanupError):s.close()
                self.assertTrue(s._cleanup_complete);self.assertEqual(t.disposals,1)
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)

    def test_atomic_evidence_handoff_fsyncs_file_then_replace_then_parent(self):
        import stat
        from .bounded import durable_bytes
        with tempfile.TemporaryDirectory() as raw:
            target=Path(raw)/'proof.json';events=[];real_sync=os.fsync;real_replace=os.replace
            def sync(fd):
                events.append('parent' if stat.S_ISDIR(os.fstat(fd).st_mode) else 'file');return real_sync(fd)
            def replace(a,b):events.append('replace');return real_replace(a,b)
            with mock.patch('tools.interop.installed_hosts.bounded.os.fsync',side_effect=sync),mock.patch('tools.interop.installed_hosts.bounded.os.replace',side_effect=replace):
                durable_bytes(target,b'canonical evidence\n')
            self.assertEqual(events,['file','replace','parent']);self.assertEqual(target.read_bytes(),b'canonical evidence\n')
            self.assertEqual(target.stat().st_mode & 0o777,0o600)

    def test_lost_verify_reply_stops_exact_generation_without_another_request(self):
        class LostVerify(FixtureSSH):
            lose=False
            def exchange(self,request,timeout):
                reply=super().exchange(request,timeout)
                if self.lose and request['op']=='verify':raise OSError('lost actual processed verify reply')
                return reply
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);t=LostVerify(root);s=InstalledSession.open(*inputs(root),transport=t,verify_local_inputs=False)
            try:
                s.request({'op':'verify'});t.lose=True
                with self.assertRaises(OSError):s.request({'op':'verify'})
                with self.assertRaises(SessionError):s.request({'op':'pair'})
                with mock.patch('tools.interop.installed_hosts.transport.run_capped',return_value=subprocess.CompletedProcess([],1,b'',b'')):
                    with self.assertRaises(CleanupError) as result:s.close()
                self.assertTrue(s._cleanup_complete)
                self.assertEqual(result.exception.evidence.final_stopped['service']['owned_generation']['stop_evidence']['operation_sequence'],2)
            finally:s.journal.close();s._host_lease.release();t._reap_process(0.2)
