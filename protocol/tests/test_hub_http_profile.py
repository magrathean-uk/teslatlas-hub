"""Independent current-Hub contract tests; no Hub implementation imports."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / 'profiles/hub-http-v1/1.0.0'

class HubHttpTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.assert_bundle = PROFILE / 'profile.json'
        if not cls.assert_bundle.exists():
            raise AssertionError('current-Hub profile bundle must exist before acceptance')
        spec = importlib.util.spec_from_file_location('hub_http', ROOT / 'conformance/hub_http.py')
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def validate(self, kind, value, status=200, headers=None):
        return self.module.validate_raw(PROFILE, kind, status, headers or {'content-type':'application/json'}, json.dumps(value).encode())

    def example(self, kind):
        return json.loads((PROFILE / 'examples' / (kind + '.json')).read_text())

    def test_bundle_is_deterministic_and_intact(self):
        result = subprocess.run([sys.executable, str(ROOT / 'tools/build_hub_http_profile.py'), '--check'], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(self.module.load_profile(PROFILE)['profile_id'], 'hub-http-v1@1.0.0')

    def test_every_positive_example_validates_without_invented_version_headers(self):
        for kind in ('discovery','vehicles','current','health','ready','claim','invitation'):
            with self.subTest(kind=kind):
                self.assertEqual(self.validate(kind,self.example(kind)), [])
        self.assertEqual(self.validate('drives',self.example('drives'),headers={'content-type':'application/json','etag':'"synthetic-page"','cache-control':'no-store'}),[])

    def test_discovery_rejects_rich_profile_and_urn_identity(self):
        for patch in ({'protocol':{'current_version':'1.2.0'}},{'hub_id':'urn:uuid:11111111-1111-4111-8111-111111111111'},{'capabilities':[{'id':'query.vehicles'}]},{'capabilities':['commands']}):
            value=self.example('discovery');value.update(patch)
            self.assertTrue(self.validate('discovery',value))

    def test_null_current_is_preserved_but_missing_or_wrong_types_fail(self):
        value=self.example('current')
        self.assertIsNone(value['observed_at_ms'])
        for field in ('observed_at_ms','battery_level','locked','car'):
            changed=copy.deepcopy(value);changed[field]='unknown'
            self.assertTrue(self.validate('current',changed))
        del value['observed_at_ms']
        self.assertTrue(self.validate('current',value))

    def test_signed64_values_are_lossless_and_overflow_is_rejected(self):
        value=self.example('current')
        for integer in (-(2**63),2**53+1,2**63-1):
            value['observed_at_ms']=integer
            self.assertEqual(self.validate('current',value),[])
        for integer in (-(2**63)-1,2**63,True,1.25):
            value['observed_at_ms']=integer
            self.assertTrue(self.validate('current',value))

    def test_route_specific_etags_and_empty_error_bodies(self):
        self.assertTrue(self.validate('drives',self.example('drives')))
        self.assertEqual(self.module.validate_raw(PROFILE,'drives',304,{'etag':'"page"','cache-control':'no-store'},b''),[])
        self.assertTrue(self.module.validate_raw(PROFILE,'drives',304,{'etag':'"page"','cache-control':'no-store'},b'{}'))
        self.assertTrue(self.module.validate_raw(PROFILE,'drives',304,{},b''))
        for kind in ('vehicles','current','claim','rotate'):
            self.assertEqual(self.module.validate_raw(PROFILE,kind,401,{},b''),[])
            self.assertTrue(self.module.validate_raw(PROFILE,kind,401,{},b'{"access_token":"must-not-leak"}'))

    def test_errors_and_unknown_status_fail_closed_without_payload_diagnostics(self):
        for code in ('invalid_limit','invalid_time_range','invalid_query','invalid_cursor'):
            self.assertEqual(self.validate('drives',{'error':{'code':code,'message':'synthetic error'}},400),[])
        secret='must-not-leak'
        errors=self.module.validate_raw(PROFILE,'claim',200,{'content-type':'application/json'},('{"access_token":"'+secret+'"}').encode())
        self.assertTrue(errors);self.assertNotIn(secret,str(errors))
        self.assertTrue(self.module.validate_raw(PROFILE,'vehicles',418,{},b''))

    def test_strict_json_rejects_nan_duplicate_keys_and_trailing_bytes(self):
        for raw in (b'{"battery_level":NaN}',b'{"vehicles":[],"vehicles":[]}',b'{"vehicles":[]} x'):
            self.assertTrue(self.module.validate_raw(PROFILE,'vehicles',200,{'content-type':'application/json'},raw))

    def test_each_route_rejects_wrong_wire_semantics(self):
        invalids = [('health', {'status':'ready'}),('ready', {'status':'ok'}),
            ('vehicles', {'vehicles':[{'vehicle_id':'bad-uuid','display_name':None}]}),
            ('current', {'speed':3.5}),('claim', {'device_id':'bad-uuid'}),
            ('invitation', {'pairingId':'bad-uuid'})]
        for kind, patch in invalids:
            value=self.example(kind);value.update(patch)
            self.assertTrue(self.validate(kind,value), kind)
        value=self.example('claim')
        self.assertEqual(self.validate('rotate',value),[])
        value['access_token']='not-a-bearer'
        self.assertTrue(self.validate('rotate',value))
        value=self.example('drives');value['next_cursor']=42
        self.assertTrue(self.validate('drives',value,headers={'content-type':'application/json','etag':'"page"','cache-control':'no-store'}))

    def test_schema2_2_appendix_does_not_advertise_rich_features(self):
        # Discovery remains valid without signing key, but cannot silently add
        # unsupported rich semantic fields or claim commands capability.
        value=self.example('discovery');value['protocol_revision']=1
        self.assertTrue(self.validate('discovery',value))

    def test_private_network_config_rejects_missing_prerequisites(self):
        for config in ({}, {'endpoint':'https://127.0.0.1:1','profile_id':'hub-http-v1@1.0.0'}):
            with self.assertRaises(self.module.AcceptanceError):
                self.module.run_network(config)

    def test_profile_hash_tampering_and_missing_manifest_members_fail(self):
        import tempfile
        import shutil
        with tempfile.TemporaryDirectory() as directory:
            copied=Path(directory)/'profile';shutil.copytree(PROFILE,copied)
            with self.assertRaises(self.module.AcceptanceError):
                self.module.load_profile(copied,'0'*64)
            (copied/'resources.schema.json').write_text('{}')
            with self.assertRaises(self.module.AcceptanceError):
                self.module.load_profile(copied)

    def test_pairing_uri_must_agree_with_distinct_invitation_fields(self):
        value=self.example('invitation')
        value['pairingId']='22222222-2222-4222-8222-222222222222'
        self.assertTrue(self.validate('invitation',value))

    def test_errors_use_endpoint_specific_schemas_and_codes(self):
        error={'error':{'code':'service_unavailable','message':'synthetic'}}
        self.assertEqual(self.validate('discovery',error,503),[])
        self.assertEqual(self.validate('drives',error,503),[])
        self.assertTrue(self.validate('drives',error,400))
        ready={'status':'not_ready','reason':'collector_absent'}
        self.assertEqual(self.validate('ready',ready,503),[])
        self.assertTrue(self.validate('ready',ready,200))
        self.assertTrue(self.validate('ready',{'status':'not_ready','reason':'invented'},503))

    def test_overflowing_json_float_is_rejected_before_validation(self):
        value=self.example('current');value['outside_temp']=1.5
        raw=json.dumps(value).replace('1.5','1e999').encode()
        self.assertTrue(self.module.validate_raw(PROFILE,'current',200,{'content-type':'application/json'},raw))

    def test_drive_503_accepts_both_actual_forms_and_rejects_wrong_bodies(self):
        self.assertEqual(self.module.validate_raw(PROFILE,'drives',503,{},b''),[])
        value={'error':{'code':'service_unavailable','message':'synthetic'}}
        self.assertEqual(self.validate('drives',value,503),[])
        for headers,raw in [({},b'outage'),({'content-type':'text/plain'},b'outage'),
                ({'content-type':'application/json'},b'{}'),
                ({'content-type':'application/json'},b'{"error":{"code":"invalid_cursor","message":"synthetic"}}')]:
            self.assertTrue(self.module.validate_raw(PROFILE,'drives',503,headers,raw))

    def test_drive_200_and_304_require_no_store_even_with_valid_etag(self):
        for status,raw in [(200,json.dumps(self.example('drives')).encode()),(304,b'')]:
            for cache in (None,'public, max-age=3600','private','no-cache'):
                headers={'content-type':'application/json','etag':'"page"'}
                if cache is not None:headers['cache-control']=cache
                self.assertTrue(self.module.validate_raw(PROFILE,'drives',status,headers,raw))
            self.assertEqual(self.module.validate_raw(PROFILE,'drives',status,
                {'content-type':'application/json','etag':'"page"','cache-control':'no-store'},raw),[])

    def test_explicit_acceptance_without_private_prerequisites_fails(self):
        result=subprocess.run([sys.executable,str(ROOT/'conformance/runner.py'),'--profile','hub-http-v1@1.0.0','--adapter',str(ROOT/'conformance/adapters/actual-hub'),'--json'],capture_output=True,env={'PATH':'/usr/bin:/bin'})
        self.assertNotEqual(result.returncode,0)
        self.assertNotIn(b'passed":1',result.stdout)



class NativeEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec=importlib.util.spec_from_file_location('hub_http_native_test',ROOT/'conformance/hub_http.py')
        cls.module=importlib.util.module_from_spec(spec);spec.loader.exec_module(cls.module)

    def setUp(self):
        import tempfile, hashlib, os
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)/'fixture';self.root.mkdir(mode=0o700)
        stage=Path(self.temp.name)/'staged';stage.mkdir(mode=0o700)
        self.binary=stage/'hub';self.seed=stage/'seed'
        source=stage/'test.c';source.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
        compiled=subprocess.run(['cc',str(source),'-o',str(self.binary)],capture_output=True)
        self.assertEqual(compiled.returncode,0,'native test helper compile failed')
        self.seed.write_bytes(self.binary.read_bytes())
        self.binary.chmod(0o500);self.seed.chmod(0o500)
        self.config_path=self.root/'config.toml'
        self.config_path.write_text('data_dir = "'+str(self.root/'hub')+'"\nbind = "127.0.0.1:18443"\n[tls]\npublic_url = "https://127.0.0.1:18443"\n')
        self.config_path.chmod(0o600)
        self.child=subprocess.Popen([str(self.binary),'--config',str(self.config_path),'serve'])
        self.addCleanup(self.stop)
        self.assertTrue(hasattr(self.module,'native_evidence'),'native evidence verifier required')
        evidence=self.module.native_evidence
        hub=evidence.process_identity(self.child.pid);launcher=evidence.process_identity(os.getpid())
        self.config={'binary_path':str(self.binary),'seed_binary_path':str(self.seed),
            'binary_sha256':hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            'seed_binary_sha256':hashlib.sha256(self.seed.read_bytes()).hexdigest(),
            'hub_pid':self.child.pid,'launcher_pid':os.getpid(),'hub_started_at':hub['started_at'],
            'launcher_started_at':launcher['started_at'],'ready_path':str(self.root/'ready.json'),
            'config_path':str(self.config_path),'endpoint':'https://127.0.0.1:18443',
            'status':'ready','provenance':'synthetic-real-process'}
        self.write_ready()

    def stop(self):
        if self.child.poll() is None:self.child.terminate()
        self.child.wait(timeout=5)

    def write_ready(self):
        path=Path(self.config['ready_path']);path.write_text(json.dumps(self.config));path.chmod(0o600)

    def verify(self):
        return self.module.verify_native_fixture(self.config)

    def test_live_owned_native_process_and_retained_hashes_verify(self):
        self.verify()

    def test_missing_malformed_mismatched_binary_evidence_rejected(self):
        import copy
        original=copy.deepcopy(self.config)
        for key in ('binary_path','seed_binary_path','binary_sha256','seed_binary_sha256','hub_pid','launcher_pid','ready_path'):
            self.config=copy.deepcopy(original);del self.config[key]
            with self.subTest(missing=key), self.assertRaises(self.module.AcceptanceError):self.verify()
        for key,value in [('binary_sha256',None),('seed_binary_sha256',True),('binary_sha256','not-a-digest'),('binary_path','relative'),('hub_pid',True),('launcher_pid',0),('binary_sha256','0'*64),('seed_binary_sha256','1'*64)]:
            self.config=copy.deepcopy(original);self.config[key]=value;self.write_ready()
            with self.subTest(key=key,value=value), self.assertRaises(self.module.AcceptanceError):self.verify()
        self.config=original;self.write_ready()
        self.seed.chmod(0o700);self.seed.write_bytes(b'changed input');self.seed.chmod(0o500)
        with self.assertRaisesRegex(self.module.AcceptanceError,'digest'):self.verify()

    def test_stale_or_unbound_readiness_cannot_pass(self):
        original=dict(self.config)
        for key,value in [('hub_started_at','stale'),('launcher_started_at','stale'),('status','stopped'),('binary_path',str(self.seed))]:
            self.config=dict(original);self.config[key]=value;self.write_ready()
            with self.subTest(key=key), self.assertRaises(self.module.AcceptanceError):self.verify()
        self.config=dict(original);self.write_ready()
        self.config['hub_started_at']='descriptor disagrees with retained witness'
        with self.assertRaises(self.module.AcceptanceError):self.verify()
        self.config=dict(original);self.write_ready()
        stopped=self.root/'stopped.json';stopped.write_text('{}');stopped.chmod(0o600)
        with self.assertRaisesRegex(self.module.AcceptanceError,'stale'):self.verify()
        stopped.unlink()
        self.stop()
        with self.assertRaises(self.module.AcceptanceError):self.verify()

    def test_failed_native_preflight_does_not_read_invitation(self):
        from unittest.mock import patch
        self.config.update(profile_id='hub-http-v1@1.0.0',profile_path=str(PROFILE),profile_sha256='0'*64,
            invitation_path=str(self.root/'never-read-secret.json'),certificate_path='unused',scenario_path='unused',scenario_sha256='0'*64,
            update_request_path='unused',update_receipt_path='unused')
        self.config['binary_sha256']=None
        with patch.object(self.module,'read_private',side_effect=AssertionError('invitation read before provenance')):
            with self.assertRaises(self.module.AcceptanceError):self.module.run_network(self.config)

if __name__ == '__main__':
    unittest.main()
