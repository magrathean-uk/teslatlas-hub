# SPDX-License-Identifier: AGPL-3.0-only
import copy
import hashlib
import http.server
import json
import os
import socket
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from tools.interop.installed_hosts._common import _matches_schema, normal_tls_probe, process_tree_digest
from tools.interop.installed_hosts.bounded import Deadline, IncrementalLineReader, run_capped, write_all
from tools.interop.installed_hosts.contract import ContractError, RegisteredConfig, payload_manifest_digest, read_registered_config, validate_installed_observation, validate_session_config
from tools.interop.installed_hosts.guest import GuestController
from tools.interop.installed_hosts import guest as guest_module
from tools.interop.installed_hosts.lease import HostLease
from tools.interop.installed_hosts.linux import CONFIG, EXECUTABLE, LinuxController, _cross_boot_tick
from tools.interop.installed_hosts.macos import MacOSController, WRAPPER, _observe_absent_before
from tools.interop.installed_hosts.session import CLEANUP_BUDGET, INNER_BUDGETS, OUTER_BUDGETS, CleanupError, InstalledSession, SessionError, _validate_mac_stop_evidence
from tools.interop.installed_hosts.prepare import HostPreparer
from tools.interop.installed_hosts.test_contract import registration as raw_registration
from tools.interop.installed_hosts.test_platform_observation import observation, registered
from tools.interop.installed_hosts.test_session import FakeTransport, inputs
from tools.interop.installed_hosts.transport import SSHTransport, build_session_ssh_argv, remote_session_paths


class _Server(http.server.ThreadingHTTPServer):
    allow_reuse_address = True


class _Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    redirect = False
    ready_failures = 0

    def do_GET(self):
        if self.redirect and self.path == "/.well-known/teslatlas-hub":
            self.send_response(302)
            self.send_header("Location", "/elsewhere")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.path == "/readyz" and self.ready_failures:
            type(self).ready_failures -= 1
            self.send_response(503)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            return
        bodies = {
            "/.well-known/teslatlas-hub": {"protocol": "teslatlas-sync", "protocol_major": 1, "api_versions": ["1.0"], "capabilities": ["query.vehicles", "query.current", "query.drives", "sync.packs"], "pack_format": "sqlite-zstd", "version": "2026.36.2", "hub_id": "11111111-1111-4111-8111-111111111111", "sourceUrl": "https://example.invalid/source"},
            "/healthz": {"status": "ok", "version": "2026.36.2"},
            "/readyz": {"status": "ready"},
        }
        raw = json.dumps(bodies[self.path], separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, *_args):
        pass


def _certificate(directory, hostname="127.0.0.1"):
    config = directory / "openssl.cnf"
    alt = "IP:127.0.0.1" if hostname == "127.0.0.1" else "DNS:" + hostname
    config.write_text("[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN={}\n[ext]\nsubjectAltName={}\n".format(hostname, alt))
    cert, key = directory / "cert.pem", directory / "key.pem"
    subprocess.run(["/usr/bin/openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", str(config), "-keyout", str(key), "-out", str(cert)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return cert, key


class CorrectionTests(unittest.TestCase):
    def test_database_identity_and_installed_inputs_are_pinned_before_linux_cli(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            cell = root / "cell"
            database = cell / "hub" / "hub.sqlite"
            database.parent.mkdir(parents=True)
            scenario = cell / "scenario.json"
            scenario.write_text(json.dumps({"schema_version":1,"name":"two-vehicles-five-drives","provenance":"synthetic-only","later_current":{}}))
            seed = root / "seed"
            seed.write_bytes(b"current-seed")
            manifest = root / "manifest.json"
            manifest.write_text("{}")
            first_id = "11111111-1111-4111-8111-111111111111"
            second_id = "22222222-2222-4222-8222-222222222222"

            def make_store(store_id):
                if database.exists():
                    database.unlink()
                connection = sqlite3.connect(str(database))
                connection.execute("PRAGMA user_version=59")
                connection.execute("CREATE TABLE hub_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                connection.execute("INSERT INTO hub_metadata(key,value) VALUES('installation_id',?)", (store_id,))
                connection.commit()
                connection.close()

            make_store(first_id)
            r = registered()
            r.registration["guest"].update(python_toml_module="tomllib", permitted_service_user="teslatlas")
            r.registration["guest"]["python"]["path"] = sys.executable
            r.config["allowed_origins"] = []
            r.config["seed"]["sha256"] = hashlib.sha256(seed.read_bytes()).hexdigest()
            r.config["scenario"]["sha256"] = hashlib.sha256(scenario.read_bytes()).hexdigest()
            private = {"guest_inputs":{"package_manifest":str(manifest),"scenario":str(scenario),"seed":str(seed)},"controller_root":str(root),"installation_receipt":str(root/"receipt")}
            controller = LinuxController(r, private)
            controller.data_root = str(cell)
            config_raw = ('data_dir = "{0}/hub"\nbind = "127.0.0.1:18480"\n[tls]\npublic_url = "https://127.0.0.1:18480"\ncertificate_path = "{0}/server.pem"\nprivate_key_path = "{0}/server-key.pem"\n[collector]\ninterval_seconds = 0\nowner_api_base_url = "https://127.0.0.1:1/"\n[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n[geocoder]\nenabled = false\n[http]\nallowed_origins = []\n'.format(controller.data_root)).encode()
            parsed = {"data_dir":controller.data_root+"/hub","bind":"127.0.0.1:18480","tls":{"public_url":"https://127.0.0.1:18480","certificate_path":controller.data_root+"/server.pem","private_key_path":controller.data_root+"/server-key.pem"},"collector":{"interval_seconds":0,"owner_api_base_url":"https://127.0.0.1:1/","legacy_auth":{"enabled":False}},"terrain":{"enabled":False},"geocoder":{"enabled":False},"http":{"allowed_origins":[]}}
            product_commands = []

            def service_read(path):
                if path == CONFIG:
                    return config_raw
                if path.endswith("connection.json"):
                    return json.dumps({"hub_id":first_id}).encode()
                if path.endswith("scenario.json"):
                    return scenario.read_bytes()
                raise AssertionError(path)

            def run(argv, timeout=15, allowed_status=(0,)):
                if "-c" in argv and "sqlite3" in argv[argv.index("-c") + 1]:
                    start = argv.index(sys.executable)
                    return subprocess.run(argv[start:], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
                product_commands.append(tuple(argv))
                return subprocess.CompletedProcess(argv, 0, b'[{"device_id":"x"}]', b"")

            controller._service_read = service_read
            controller._run = run
            controller._receipt = lambda: ({"payload_manifest_sha256":"e"*64}, "f"*64)
            with mock.patch("tools.interop.installed_hosts.linux.loads_toml", return_value=parsed), mock.patch("tools.interop.installed_hosts.linux.verify_payload_members", return_value="e"*64):
                context = controller.pin_current_context()
                self.assertEqual(context["store_id"], first_id)
                self.assertEqual(context["scenario_sha256"], r.config["scenario"]["sha256"])
                self.assertEqual(context["seed_sha256"], r.config["seed"]["sha256"])
                make_store(second_id)
                with self.assertRaisesRegex(RuntimeError, "database installation identity changed"):
                    controller.paired_device_ids()
            self.assertEqual(product_commands, [])

    def test_database_identity_is_pinned_before_macos_cli(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            data_root = root / "cell"
            database = data_root / "hub" / "hub.sqlite"
            database.parent.mkdir(parents=True)
            store_id = "11111111-1111-4111-8111-111111111111"
            replacement_id = "22222222-2222-4222-8222-222222222222"
            scenario = data_root / "scenario.json"
            scenario.write_text(json.dumps({"schema_version":1,"name":"two-vehicles-five-drives","provenance":"synthetic-only","later_current":{}}))
            seed = root / "seed"; seed.write_bytes(b"seed")
            manifest = root / "manifest"; manifest.write_text("{}")

            def make_store(identity):
                if database.exists(): database.unlink()
                db = sqlite3.connect(str(database)); db.execute("PRAGMA user_version=59"); db.execute("CREATE TABLE hub_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL)"); db.execute("INSERT INTO hub_metadata VALUES('installation_id',?)",(identity,)); db.commit(); db.close()

            make_store(store_id)
            r = registered("macOS")
            r.registration["guest"].update(python_toml_module="tomllib")
            r.registration["guest"]["python"]["path"] = sys.executable
            r.config["allowed_origins"] = []
            r.config["seed"]["sha256"] = hashlib.sha256(seed.read_bytes()).hexdigest()
            r.config["scenario"]["sha256"] = hashlib.sha256(scenario.read_bytes()).hexdigest()
            controller = MacOSController(r,{"guest_inputs":{"package_manifest":str(manifest),"scenario":str(scenario),"seed":str(seed)},"controller_root":str(root),"installation_receipt":str(root/"receipt")})
            controller.data_root = str(data_root); controller.config_path = str(root/"config.toml")
            Path(controller.config_path).write_text("synthetic")
            (data_root/"connection.json").write_text(json.dumps({"hub_id":store_id}))
            parsed={"data_dir":controller.data_root+"/hub","bind":"127.0.0.1:18480","tls":{"public_url":"https://127.0.0.1:18480","certificate_path":controller.data_root+"/server.pem","private_key_path":controller.data_root+"/server-key.pem"},"collector":{"interval_seconds":0,"owner_api_base_url":"https://127.0.0.1:1/","legacy_auth":{"enabled":False}},"terrain":{"enabled":False},"geocoder":{"enabled":False},"http":{"allowed_origins":[]}}
            product=[]
            def run(argv,timeout=15,allowed_status=(0,)):
                if "-c" in argv and "sqlite3" in argv[argv.index("-c")+1]: return subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False)
                product.append(tuple(argv)); return subprocess.CompletedProcess(argv,0,b'[]',b'')
            controller._run=run; controller._receipt=lambda:({"payload_manifest_sha256":"e"*64},"f"*64)
            with mock.patch("tools.interop.installed_hosts.macos.loads_toml",return_value=parsed),mock.patch("tools.interop.installed_hosts.macos.verify_payload_members",return_value="e"*64):
                self.assertEqual(controller.pin_current_context()["store_id"],store_id)
                make_store(replacement_id)
                with self.assertRaisesRegex(RuntimeError,"database installation identity changed"):
                    controller.paired_device_ids()
            self.assertEqual(product,[])

    def test_linux_start_failure_reconciles_the_exact_started_generation(self):
        r = registered()
        r.registration["guest"].update(permitted_service_user="teslatlas", permitted_service_uid=997)
        controller = LinuxController(r,{"guest_inputs":{},"controller_root":"/tmp"})
        process = copy.deepcopy(observation()["service"]["hub"])
        process["start_identity"] = "101"
        stopped = {"ActiveState":"inactive","SubState":"dead","MainPID":"0","ControlGroup":"","InvocationID":""}
        running = {"ActiveState":"active","SubState":"running","MainPID":str(process["pid"]),"ControlGroup":"/system.slice/teslatlas-hub.service","InvocationID":"0123456789abcdef0123456789abcdef"}
        state = {"started":False}
        acquired = []
        controller.acquisition_callback = lambda value: acquired.append(copy.deepcopy(value))
        controller._systemd = lambda: running if state["started"] else stopped
        controller._process = lambda pid, expected_argv=None, service_owned=False: process
        def run(argv,timeout=15,allowed_status=(0,)):
            if argv == ["/usr/bin/sudo","-n","/bin/systemctl","start","teslatlas-hub.service"]:
                state["started"] = True
                raise TimeoutError("synthetic SSH completion lost after unit launch")
            return subprocess.CompletedProcess(argv,0,b"",b"")
        controller._run = run
        with mock.patch("tools.interop.installed_hosts.linux._cross_boot_tick",return_value={"clock_basis":"CLOCK_BOOTTIME_SC_CLK_TCK","ticks_per_second":os.sysconf("SC_CLK_TCK"),"not_before_start_tick":100,"boundary_boottime_ns":1_000_000_000}):
            with self.assertRaisesRegex(TimeoutError,"completion lost"):
                controller.start()
        self.assertEqual(acquired[-1]["hub"],process)
        self.assertEqual(acquired[-1]["invocation_id"],running["InvocationID"])

    def test_linux_pending_reconciliation_rejects_a_pre_intent_generation(self):
        r=registered(); controller=LinuxController(r,{"guest_inputs":{},"controller_root":"/tmp"})
        prior="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; current="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        boundary={"clock_basis":"CLOCK_BOOTTIME_SC_CLK_TCK","ticks_per_second":os.sysconf("SC_CLK_TCK"),"not_before_start_tick":100,"boundary_boottime_ns":1_000_000_000}
        ownership={"config":{},"service":{"acquisition_state":"pending-start","target":"teslatlas-hub.service","start_boundary":boundary,"pre_start_state":{"active":"inactive","sub":"dead","pid":0,"invocation_id":prior}}}
        controller.current_context=lambda owned=None:{}
        controller._systemd=lambda:{"ActiveState":"active","SubState":"running","MainPID":"41","InvocationID":current}
        stale=copy.deepcopy(observation()["service"]); stale["hub"]["start_identity"]="1"
        controller.capture_service=lambda status=None,publish=True:stale
        with self.assertRaisesRegex(RuntimeError,"pre-intent generation"):
            controller.reconcile_owned(ownership)

    def test_linux_pending_reconciliation_accepts_legitimate_same_tick_start(self):
        r=registered(); controller=LinuxController(r,{"guest_inputs":{},"controller_root":"/tmp"})
        hz=os.sysconf("SC_CLK_TCK")
        pending={"acquisition_state":"pending-start","target":"teslatlas-hub.service","start_boundary":{"clock_basis":"CLOCK_BOOTTIME_SC_CLK_TCK","ticks_per_second":hz,"not_before_start_tick":100,"boundary_boottime_ns":1_000_000_000},"pre_start_state":{"active":"inactive","sub":"dead","pid":0,"invocation_id":"a"*32}}
        status={"ActiveState":"active","SubState":"running","MainPID":"41","InvocationID":"b"*32}
        captured=copy.deepcopy(observation()["service"]); captured["hub"]["start_identity"]="100"
        controller.capture_service=lambda status=None,publish=True:copy.deepcopy(captured)
        self.assertEqual(controller._capture_pending(pending,status)["hub"]["start_identity"],"100")

    def test_linux_start_boundary_crosses_one_boottime_tick_deterministically(self):
        values=iter((1_000_000_000,1_000_000_001,1_009_999_999,1_010_000_000))
        boundary=_cross_boot_tick(clock_ns=lambda:next(values),pause=lambda _seconds:None,ticks_per_second=100)
        self.assertEqual(boundary,{"clock_basis":"CLOCK_BOOTTIME_SC_CLK_TCK","ticks_per_second":100,"not_before_start_tick":101,"boundary_boottime_ns":1_010_000_000})

    def test_macos_wrapper_is_persisted_before_hub_capture_failure(self):
        r=registered("macOS")
        controller=MacOSController(r,{"controller_root":"/tmp"})
        service=observation("macOS")["service"]
        wrapper=service["supervisor"]
        controller._launch_status=lambda required=True:(wrapper["pid"],"loaded")
        controller._hub_child=lambda *_args: (_ for _ in ()).throw(RuntimeError("synthetic child capture failure"))
        acquired=[]; controller.acquisition_callback=lambda value:acquired.append(copy.deepcopy(value))
        with mock.patch("tools.interop.installed_hosts.macos._process",return_value=wrapper):
            with self.assertRaisesRegex(RuntimeError,"child capture failure"):
                controller.capture_service()
        self.assertEqual(acquired,[{"acquisition_state":"wrapper-acquired","supervisor":wrapper,"app_process":None}])

    def test_macos_fresh_app_only_record_with_context_is_recoverable(self):
        r=registered("macOS")
        controller=MacOSController(r,{"guest_inputs":{"package_manifest":"/manifest","seed":"/seed"},"controller_root":"/tmp","installation_receipt":"/receipt"})
        app=observation("macOS")["service"]["app_process"]
        context={"path":controller.config_path,"sha256":"1"*64,"data_dir":controller.data_root+"/hub","store_id":"11111111-1111-4111-8111-111111111111","store_schema_version":59,"scenario_sha256":"3"*64,"seed_sha256":"1"*64}
        owned={"session_id":r.config["session_id"],"lease":r.registration["lease"],"config_path":controller.config_path,"store_id":context["store_id"],"store_schema_version":59,"state":"starting","config":context,"service":{"acquisition_state":"app-acquired","app_process":app}}
        controller.current_context=lambda ownership=None:context
        controller._launch_status=lambda required=True:None
        with mock.patch("tools.interop.installed_hosts.macos._generation_present",return_value=True):
            controller.assert_owned(owned)
        self.assertEqual(controller.app_process,app)

    def test_explicit_stop_evidence_survives_guest_close(self):
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"
            r=registered(); service=observation()["service"]
            class Platform:
                def __init__(self): self.private={"ownership_path":str(path)}; self.stop_calls=0; self.deadline=None
                def set_deadline(self,deadline): self.deadline=deadline
                def assert_owned(self,_owned): return None
                def stop(self):
                    self.stop_calls+=1
                    return {"operation":"stop","session_id":r.config["session_id"],"pid":service["hub"]["pid"],"control_group":service["control_group"],"result_raw":"success","exec_main_code_raw":"1","exec_main_status_raw":"0","normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped","owned":copy.deepcopy(owned)}
            platform=Platform(); controller=GuestController(r,platform,challenge_factory=lambda:"b"*64)
            controller.cleanup_required=True; controller.state="running"; controller.verified=True; controller.challenge="a"*64
            controller.seed_facts={"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64}
            controller.config_context={"path":CONFIG,"sha256":"1"*64,"data_dir":"/data","store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64,"seed_sha256":"1"*64}
            controller.owned_service=copy.deepcopy(service); controller._write_ownership()
            controller.handle({"schema_version":1,"session_id":r.config["session_id"],"sequence":1,"challenge":"a"*64,"op":"stop","budget_ms":50000})
            first=copy.deepcopy(json.loads(path.read_text())["stop_evidence"])
            stopped,errors=controller.close()
            durable=json.loads(path.read_text())
            self.assertEqual(errors,[]); self.assertEqual(platform.stop_calls,1)
            self.assertEqual(durable["stop_evidence"],first)
            self.assertEqual(first["operation_sequence"],1)
            self.assertEqual(first["operation_challenge"],"a"*64)
            self.assertEqual(first["acquired_service_sha256"],hashlib.sha256(json.dumps(service,sort_keys=True,separators=(",",":")).encode()).hexdigest())
            self.assertEqual(stopped["status"],"stopped")

    def test_runner_rejects_same_pid_stale_stopped_generation(self):
        class Stale(FakeTransport):
            def verify_stopped(self,registered,timeout):
                value=super().verify_stopped(registered,timeout)
                acquired=copy.deepcopy(observation(start="999:1")["service"])
                acquired["hub"]["pid"]=40; acquired["supervisor"]["pid"]=40
                stop=value["service"]["owned_generation"]["stop_evidence"]
                stop.update(operation_sequence=2,operation_challenge=self.requests[-1]["challenge"],acquired_service_sha256=hashlib.sha256(json.dumps(acquired,sort_keys=True,separators=(",",":")).encode()).hexdigest())
                value["service"]["owned_generation"]["service"]=acquired
                return value
        with tempfile.TemporaryDirectory() as raw:
            session=InstalledSession.open(*inputs(Path(raw)),transport=Stale(),verify_local_inputs=False)
            session.request({"op":"verify"}); session.request({"op":"stop"})
            with self.assertRaisesRegex(SessionError,"last acquired generation"):
                session.verify_stopped()
            session.transport.verify_stopped=FakeTransport.verify_stopped.__get__(session.transport,Stale)
            self.assertEqual(session.close().state,"closed")

    def test_macos_stop_waits_for_owned_wrapper_inside_helper_cap(self):
        r=registered("macOS"); controller=MacOSController(r,{"controller_root":"/tmp"})
        service=observation("macOS")["service"]; wrapper,hub,app=service["supervisor"],service["hub"],service["app_process"]
        controller.app_process=app; controller._launch_status=lambda required=False:(wrapper["pid"],"loaded")
        controller._hub_child=lambda *_args:hub; controller._run=lambda argv,timeout=15,allowed_status=(0,):subprocess.CompletedProcess(argv,0,b"",b"")
        verified=[]
        controller.verify_stopped=lambda owned=None,initial=False:(verified.append(copy.deepcopy(owned)) or {"status":"stopped"})
        wrapper_reads=[True,False]
        def present(identity):
            if identity==hub or identity==app:return False
            if identity==wrapper:return wrapper_reads.pop(0)
            return False
        with mock.patch("tools.interop.installed_hosts.macos._process",return_value=wrapper),mock.patch("tools.interop.installed_hosts.macos._generation_present",side_effect=present):
            evidence=controller.stop()
        observations=evidence["cleanup_absence_observations"]
        self.assertEqual([row["role"] for row in observations],["app","wrapper"])
        self.assertFalse(observations[-1]["present"])
        self.assertLessEqual(observations[-1]["observed_monotonic_ns"]-evidence["stop_requested_monotonic_ns"],evidence["wrapper_helper_cleanup_cap_ns"])
        self.assertEqual(verified[-1]["service"]["hub"],hub)
        self.assertEqual(verified[-1]["stop_evidence"],evidence)

    def test_macos_no_hub_stop_cannot_substitute_for_acquired_hub(self):
        r=registered("macOS"); controller=MacOSController(r,{"controller_root":"/tmp"})
        service=observation("macOS")["service"]
        stop={"operation":"stop","session_id":r.config["session_id"],"hub":None,"app":service["app_process"],"transient_helpers":[],"normal_exit":True,"normal_exit_code":"unavailable","normal_exit_predicate":"no-hub-generation-acquired-app-absence","stop_requested_monotonic_ns":1,"app_absent_monotonic_ns":2}
        owned={"service":service,"stop_evidence":stop}
        controller._launch_status=lambda required=False:None
        controller._run=lambda argv,timeout=15,allowed_status=(0,):subprocess.CompletedProcess(argv,1,b"",b"")
        with mock.patch("tools.interop.installed_hosts.macos._generation_present",return_value=False):
            proof=controller.verify_stopped(owned=owned)
        self.assertFalse(proof["service"]["normal_exit"])

    def test_broker_idle_wait_does_not_consume_next_frame_budget(self):
        with tempfile.TemporaryDirectory() as raw, mock.patch.dict("tools.interop.installed_hosts.session.INNER_BUDGETS", {"verify":0.05}):
            session=InstalledSession.open(*inputs(Path(raw)),transport=FakeTransport(),verify_local_inputs=False)
            client=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); client.settimeout(1); client.connect(session.descriptor["broker_socket"])
            stream=client.makefile("rb"); greeting=json.loads(stream.readline())
            time.sleep(0.08)
            request={"schema_version":1,"session_id":session.registered.config["session_id"],"sequence":1,"challenge":greeting["challenge"],"op":"verify"}
            client.sendall(json.dumps(request).encode()+b"\n")
            reply=json.loads(stream.readline())
            self.assertEqual(reply["type"],"reply")
            session.close(); client.close()

    def test_private_recovery_budget_is_set_before_ownership_check(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw); ownership=root/"ownership.json"; private_path=root/"private.json"
            r=registered(); owned={"session_id":r.config["session_id"],"lease":r.registration["lease"],"state":"running","service":{"hub":{"pid":41}},"config":{}}
            ownership.write_text(json.dumps(owned)); private={"ownership_path":str(ownership)}
            class SlowPlatform:
                def __init__(self): self.deadline=None; self.stop_calls=0
                def set_deadline(self,deadline): self.deadline=deadline
                def bind_recovery(self,_owned): pass
                def assert_owned(self,_owned): time.sleep(0.03); self.deadline.remaining()
                def stop(self): self.stop_calls+=1; return {}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped"}
            platform=SlowPlatform()
            with mock.patch.object(guest_module,"_load_private",return_value=(r,private)),mock.patch.object(guest_module,"_platform",return_value=platform):
                with self.assertRaises(TimeoutError): guest_module.main(["--recover-stop",str(private_path),"--budget-ms","10"])
            self.assertIsNotNone(platform.deadline); self.assertEqual(platform.stop_calls,0)

    def test_transport_fixed_recovery_commands_carry_relative_budget(self):
        r=registered(); seen=[]
        class RecoveryTransport(SSHTransport):
            def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False): seen.append((command,timeout)); return b'{}'
        transport=RecoveryTransport(); transport.registered=r; transport.paths=remote_session_paths(r.registration,r.config["session_id"])
        transport.recover_stop(r,0.2); transport.verify_stopped(r,0.15)
        self.assertIn("--budget-ms",seen[0][0]); self.assertIn("--budget-ms",seen[1][0])
        self.assertLessEqual(int(seen[0][0].split()[-1]),200); self.assertLessEqual(int(seen[1][0].split()[-1]),150)

    def test_actual_transport_close_reaps_after_deadline_and_closes_stderr(self):
        with tempfile.TemporaryDirectory() as raw:
            stderr=open(Path(raw)/"stderr","wb")
            process=subprocess.Popen([sys.executable,"-c","import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(30)"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr)
            transport=SSHTransport(); transport.process=process; transport.stderr_file=stderr
            started=time.monotonic()
            with self.assertRaises(Exception): transport.close(timeout=0.2)
            self.assertLess(time.monotonic()-started,1.5)
            self.assertIsNotNone(process.poll())
            self.assertTrue(stderr.closed)

    def test_retention_failure_preserves_remote_recovery_root(self):
        r=registered(); commands=[]
        class RetentionFailure(SSHTransport):
            def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False):
                commands.append(command)
                if "guest.journal.jsonl" in command and command.startswith("/bin/cat"):
                    raise RuntimeError("synthetic retention failure")
                if "shasum" in command or "sha256sum" in command:return (r.config["controller_bundle"]["sha256"]+"  archive\n").encode()
                return b""
            def _verify_remote_bundle_members(self,deadline=None): pass
        transport=RetentionFailure(); transport.registered=r; transport.paths=remote_session_paths(r.registration,r.config["session_id"]); transport._root_created=True; transport._setup_complete=True
        transport.bundle_members={}
        with self.assertRaises(Exception): transport.close(timeout=1)
        self.assertFalse(any("/bin/rm -f --" in command for command in commands))
        self.assertFalse(transport._closed)

    def test_listener_failure_retry_does_not_repeat_retention_or_disposal(self):
        with tempfile.TemporaryDirectory() as raw:
            r=registered(); r.config["local_private_root"]=raw
            (Path(raw)/r.config["session_id"]).mkdir()
            commands=[]
            class RetryListener(SSHTransport):
                def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False):
                    commands.append(command)
                    if "sha256sum" in command:return (r.config["controller_bundle"]["sha256"]+"  archive\n").encode()
                    if command.startswith("/bin/cat") and command.endswith("guest.journal.jsonl"):
                        return b'{"operation":"preflight"}\n{"kind":"closed"}\n'
                    if command.startswith("/bin/cat") and command.endswith("ownership.json"):return b'{}\n'
                    return b""
                def _verify_remote_bundle_members(self,deadline=None): pass
            transport=RetryListener(); transport.registered=r; transport.paths=remote_session_paths(r.registration,r.config["session_id"]); transport._root_created=True; transport._setup_complete=True
            probes=[subprocess.CompletedProcess([],0,b"p1\n",b""),subprocess.CompletedProcess([],1,b"",b"")]
            with mock.patch("tools.interop.installed_hosts.transport.run_capped",side_effect=lambda *_args,**_kwargs:probes.pop(0)):
                with self.assertRaisesRegex(RuntimeError,"transport cleanup failed"):transport.close(timeout=1)
                retained_count=len([c for c in commands if c.startswith("/bin/cat") and c.endswith("guest.journal.jsonl")])
                disposal_count=len([c for c in commands if "/bin/rm -f --" in c])
                transport.close(timeout=1)
            self.assertTrue(transport._closed)
            self.assertEqual(len([c for c in commands if c.startswith("/bin/cat") and c.endswith("guest.journal.jsonl")]),retained_count)
            self.assertEqual(len([c for c in commands if "/bin/rm -f --" in c]),disposal_count)

    def test_partial_open_uses_quarantined_session_cleanup(self):
        class PartialOpen(FakeTransport):
            def __init__(self): super().__init__(); self.close_args=[]
            def open(self,registered): self.registered=registered; raise RuntimeError("synthetic partial open")
            def close(self,timeout=None,preserve_recovery=False): self.close_args.append(preserve_recovery); raise RuntimeError("synthetic retained partial root")
        with tempfile.TemporaryDirectory() as raw:
            transport=PartialOpen()
            with self.assertRaisesRegex(RuntimeError,"partial open"):
                InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            self.assertTrue(transport.close_args)
            self.assertTrue(all(transport.close_args))

    def test_quarantine_record_blocks_a_new_process_lifetime(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw)/"locks"; r=registered()
            lease=HostLease(r,lock_root=root).acquire(1)
            lease.quarantine([{"resource":"service","error":"TimeoutError"}])
            lease.release()
            with self.assertRaisesRegex(ContractError,"quarantined"):
                HostLease(r,lock_root=root).acquire(1)

    def test_failed_service_cleanup_retries_without_promoting_row(self):
        class RetryService(FakeTransport):
            def __init__(self): super().__init__(fail="stop"); self.recovery_calls=0; self.verify_calls=0
            def recover_stop(self,registered,timeout):
                self.recovery_calls+=1
                if self.recovery_calls==1: raise TimeoutError("first recovery")
                self.fail=None; self.running=False; return {"status":"stopped"}
            def verify_stopped(self,registered,timeout):
                self.verify_calls+=1
                if self.recovery_calls<2: raise RuntimeError("not stopped")
                return super().verify_stopped(registered,timeout)
            def finish_writer(self,timeout): return True
            def close(self,timeout=None,preserve_recovery=False): self.closed=True
        with tempfile.TemporaryDirectory() as raw:
            transport=RetryService(); session=InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            session.request({"op":"verify"}); session._cleanup_stage_budgets={key:0.1 for key in session._cleanup_stage_budgets}
            with self.assertRaises(CleanupError): session.close()
            with self.assertRaises(CleanupError): session.close()
            self.assertEqual(transport.recovery_calls,2)
            self.assertTrue(session._cleanup_complete)

    def test_recovery_writer_is_not_started_without_original_writer_barrier(self):
        class UncertainWriter(FakeTransport):
            def __init__(self): super().__init__(fail="stop"); self.recovery_calls=0
            def finish_writer(self,timeout): return False
            def recover_stop(self,registered,timeout): self.recovery_calls+=1; return {}
            def close(self,timeout=None,preserve_recovery=False): self.closed=True
        with tempfile.TemporaryDirectory() as raw:
            transport=UncertainWriter(); session=InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            session.request({"op":"verify"}); session._cleanup_stage_budgets={key:0.05 for key in session._cleanup_stage_budgets}
            with self.assertRaises(CleanupError): session.close()
            self.assertEqual(transport.recovery_calls,0)
            self.assertFalse(session._host_lease.file.closed)
            session.journal.close(); session._host_lease.release()

    def test_macos_expansion_is_durable_before_creation_failure(self):
        raw=raw_registration(); raw["provider"]="tart-macos"
        preparer=HostPreparer(RegisteredConfig({"package":{"sha256":"d"*64}},raw,"1"*64))
        events=[]
        preparer._obligation=lambda phase,remote_package,expanded=None,errors=None,**extra:events.append((phase,expanded,copy.deepcopy(extra)))
        def remote(argv,timeout,allowed_status=(0,),input_bytes=None):
            events.append(("command",tuple(argv),{}))
            if "--expand-full" in argv: raise RuntimeError("synthetic partial expansion")
            return subprocess.CompletedProcess(argv,0,b"",b"")
        preparer._remote=remote
        with self.assertRaisesRegex(RuntimeError,"partial expansion"):
            preparer._inspect_macos_components("/tmp/teslatlas-package-"+"d"*64+".pkg",{"package_scripts":{"launchagent_template":{"sha256":"7"*64}}})
        expansion_index=next(i for i,item in enumerate(events) if item[0]=="expansion-pending")
        command_index=next(i for i,item in enumerate(events) if item[0]=="command" and "--expand-full" in item[1])
        self.assertLess(expansion_index,command_index)
        self.assertEqual(events[expansion_index][1],"/tmp/teslatlas-package-"+"d"*64+".pkg.expanded")

    def test_install_unwind_cleans_each_temp_independently(self):
        raw=raw_registration(); r=RegisteredConfig({},raw,"1"*64)
        class Independent(HostPreparer):
            def __init__(self,registered): super().__init__(registered); self.commands=[]
            def _remote(self,argv,timeout,allowed_status=(0,),input_bytes=None):
                self.commands.append(tuple(argv))
                if argv[:2]==["/usr/bin/find","/tmp/package.expanded"]: raise RuntimeError("synthetic expansion delete failure")
                return subprocess.CompletedProcess(argv,0,b"",b"")
            def _observe_stopped(self): return {"service_loaded":False,"listener_owner":None}
        preparer=Independent(r); preparer.deadline=Deadline(1)
        errors=preparer._unwind_failed_install("/tmp/package.pkg","/tmp/package.expanded",True,remote_receipt="/tmp/receipt.json",installer_complete=True)
        self.assertTrue(any(item["resource"]=="expanded" for item in errors))
        self.assertIn(("/bin/rm","-f","/tmp/package.pkg"),preparer.commands)
        self.assertIn(("/bin/rm","-f","/tmp/receipt.json"),preparer.commands)

    def test_unproved_remote_installer_completion_blocks_stop_and_cleanup(self):
        raw=raw_registration(); r=RegisteredConfig({},raw,"1"*64)
        class Unknown(HostPreparer):
            def __init__(self,registered): super().__init__(registered); self.commands=[]
            def _remote(self,argv,timeout,allowed_status=(0,),input_bytes=None): self.commands.append(tuple(argv)); return subprocess.CompletedProcess(argv,0,b"",b"")
        preparer=Unknown(r); preparer.deadline=Deadline(1)
        errors=preparer._unwind_failed_install("/tmp/package.pkg","/tmp/package.expanded",True,remote_receipt="/tmp/receipt.json",installer_complete=False)
        self.assertEqual(errors,[{"resource":"remote-installer","error":"CompletionUnproved"}])
        self.assertEqual(preparer.commands,[])

    def test_pinned_context_is_persisted_before_macos_app_side_effect(self):
        controller=MacOSController(registered("macOS"),{"controller_root":"/tmp"})
        context={"path":controller.config_path,"sha256":"1"*64,"data_dir":controller.data_root+"/hub","store_id":"11111111-1111-4111-8111-111111111111","store_schema_version":59,"scenario_sha256":"3"*64,"seed_sha256":"1"*64}
        events=[]
        controller.current_context=lambda:events.append("context-read") or copy.deepcopy(context)
        controller.context_callback=lambda value:events.append(("persisted",copy.deepcopy(value)))
        controller.pin_current_context()
        events.append("app-side-effect")
        self.assertEqual(events,["context-read",("persisted",context),"app-side-effect"])

    def test_operation_failure_leaves_cleanup_to_guest_close_owner(self):
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"; r=registered(); service=observation()["service"]
            class Platform:
                def __init__(self): self.private={"ownership_path":str(path)}; self.stop_calls=0
                def set_deadline(self,_deadline): pass
                def observe(self): raise RuntimeError("synthetic observation failure")
                def paired_device_ids(self): return set()
                def assert_owned(self,_owned): pass
                def stop(self):
                    self.stop_calls+=1
                    return {"operation":"stop","session_id":r.config["session_id"],"pid":service["hub"]["pid"],"control_group":service["control_group"],"result_raw":"success","exec_main_code_raw":"1","exec_main_status_raw":"0","normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped"}
            platform=Platform(); controller=GuestController(r,platform)
            controller.cleanup_required=True; controller.state="running"; controller.challenge="a"*64
            controller.seed_facts={"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64}
            controller.config_context={"path":CONFIG,"sha256":"1"*64,"data_dir":"/data","store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64,"seed_sha256":"1"*64}
            controller.owned_service=copy.deepcopy(service); controller._write_ownership()
            with self.assertRaisesRegex(RuntimeError,"observation failure"):
                controller.handle({"schema_version":1,"session_id":r.config["session_id"],"sequence":1,"challenge":"a"*64,"op":"verify","budget_ms":25000})
            self.assertEqual(platform.stop_calls,0)
            _stopped,errors=controller.close()
            self.assertEqual(errors,[]); self.assertEqual(platform.stop_calls,1)

    def test_cleanup_persists_narrowly_reconciled_pending_generation_before_stop(self):
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"; r=registered(); generation=copy.deepcopy(observation()["service"])
            seen=[]
            class Platform:
                def __init__(self): self.private={"ownership_path":str(path)}
                def set_deadline(self,_deadline): pass
                def reconcile_owned(self,owned):
                    seen.append(("reconcile",copy.deepcopy(owned["service"])))
                    return copy.deepcopy(generation)
                def assert_owned(self,owned): seen.append(("assert",copy.deepcopy(owned["service"])))
                def stop(self):
                    seen.append(("stop",json.loads(path.read_text())["service"]))
                    return {"operation":"stop","session_id":r.config["session_id"],"pid":generation["hub"]["pid"],"control_group":generation["control_group"],"result_raw":"success","exec_main_code_raw":"1","exec_main_status_raw":"0","normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped"}
            platform=Platform(); controller=GuestController(r,platform)
            controller.cleanup_required=True; controller.state="failed"; controller.challenge="a"*64
            controller.config_context={"path":CONFIG,"sha256":"1"*64,"data_dir":"/data","store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64,"seed_sha256":"1"*64}
            controller.owned_service={"acquisition_state":"pending-start","target":"teslatlas-hub.service"}; controller._write_ownership()
            _stopped,errors=controller.close()
            self.assertEqual(errors,[])
            self.assertEqual([name for name,_value in seen],["reconcile","assert","stop"])
            self.assertEqual({key:seen[-1][1][key] for key in generation},generation)

    def test_runner_strictly_validates_macos_stop_timing_and_helper_cap(self):
        acquired=copy.deepcopy(observation("macOS")["service"])
        helper=copy.deepcopy(acquired["hub"]); helper["pid"]+=10; helper["start_identity"]="999:1"
        acquired["transient_helpers"]=[helper]
        requested=10_000_000_000
        stop={
            "operation":"stop","session_id":registered("macOS").config["session_id"],
            "wrapper":acquired["supervisor"],"hub":acquired["hub"],"app":acquired["app_process"],
            "transient_helpers":[helper],"normal_exit":True,"normal_exit_code":"unavailable",
            "normal_exit_predicate":"hub-child-pre-escalation-generation-absence",
            "stop_requested_monotonic_ns":requested,"hub_absent_monotonic_ns":requested+1_000_000_000,
            "elapsed_to_hub_absence_ns":1_000_000_000,"escalation_boundary_ns":2_000_000_000,
            "accepted_deadline_ns":1_800_000_000,"wrapper_helper_cleanup_cap_ns":30_000_000_000,
            "cleanup_absence_observations":[
                {"role":"app","identity":acquired["app_process"],"present":False,"observed_monotonic_ns":requested+1_100_000_000},
                {"role":"wrapper","identity":acquired["supervisor"],"present":False,"observed_monotonic_ns":requested+2_100_000_000},
                {"role":"transient-helper","identity":helper,"present":False,"observed_monotonic_ns":requested+2_200_000_000},
            ],
            "operation_sequence":4,"operation_challenge":"a"*64,
            "acquired_service_sha256":hashlib.sha256(json.dumps(acquired,sort_keys=True,separators=(",",":")).encode()).hexdigest(),
        }
        _validate_mac_stop_evidence(acquired,stop,registered("macOS").config["session_id"])
        malformed=copy.deepcopy(stop); malformed["hub_absent_monotonic_ns"]=True
        with self.assertRaisesRegex(SessionError,"timestamp"):
            _validate_mac_stop_evidence(acquired,malformed,registered("macOS").config["session_id"])
        malformed=copy.deepcopy(stop); malformed["cleanup_absence_observations"][-1]["observed_monotonic_ns"]=requested+30_000_000_001
        with self.assertRaisesRegex(SessionError,"cleanup cap"):
            _validate_mac_stop_evidence(acquired,malformed,registered("macOS").config["session_id"])

    def test_inner_whole_operation_budgets_leave_protocol_margin(self):
        self.assertEqual(OUTER_BUDGETS,{"greeting":10,"verify":30,"stop":60,"start":60,"pair":160,"revoke":160})
        self.assertEqual({k:INNER_BUDGETS[k] for k in OUTER_BUDGETS},{"greeting":8,"verify":25,"stop":50,"start":50,"pair":145,"revoke":145})
        self.assertEqual(CLEANUP_BUDGET,45)
    def test_incremental_frame_and_command_output_are_bounded(self):
        left, right = socket.socketpair()
        try:
            right.sendall(b"{")
            with self.assertRaises(TimeoutError):
                IncrementalLineReader(left.fileno(), 64).read(Deadline(0.03))
        finally:
            left.close(); right.close()
        left,right=socket.socketpair()
        try:
            right.sendall(b'{"schema_version":1')
            with self.assertRaises(TimeoutError):
                InstalledSession.__new__(InstalledSession)._read_frame(IncrementalLineReader(left.fileno(),1024),Deadline(0.03))
        finally:
            left.close(); right.close()
        with self.assertRaisesRegex(RuntimeError, "output exceeded"):
            run_capped([sys.executable, "-c", "import sys; sys.stdout.write('x'*100000)"], Deadline(2), maximum=1024)

    def test_nonreading_pipe_and_guest_remaining_budget_are_real_deadlines(self):
        read_fd, write_fd = os.pipe()
        try:
            with self.assertRaises(TimeoutError):
                write_all(write_fd, b"x" * (2 * 1024 * 1024), Deadline(0.03))
        finally:
            os.close(read_fd); os.close(write_fd)
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"
            class Platform:
                def __init__(self): self.private={"ownership_path":str(path)}; self.deadline=None; self.stop_calls=0
                def set_deadline(self,deadline): self.deadline=deadline
                def observe(self): time.sleep(0.03); self.deadline.remaining(); return observation()
                def paired_device_ids(self): return set()
                def stop(self): self.stop_calls+=1
            platform=Platform(); controller=GuestController(registered(),platform)
            controller.state="running"; controller.challenge="a"*64; controller.seed_facts={"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64}
            with self.assertRaises(TimeoutError):
                controller.handle({"schema_version":1,"session_id":controller.registered.config["session_id"],"sequence":1,"challenge":controller.challenge,"op":"verify","budget_ms":10})
            self.assertEqual(platform.stop_calls,0)

    def test_schema_literals_and_listener_integers_are_json_type_strict(self):
        self.assertFalse(_matches_schema(True,{"const":1}))
        self.assertFalse(_matches_schema(1,{"enum":[True]}))
        self.assertFalse(_matches_schema({}, {"type":"object","unknownKeyword":True}))
        proof=observation(); proof["listener"]["port"]=True
        with self.assertRaises(ContractError): validate_installed_observation(proof,registered())

    def test_exact_authoritative_ha_cells_and_types(self):
        with tempfile.TemporaryDirectory() as raw:
            cfg,_=inputs(Path(raw)); cfg.update(adapter_id="home_assistant",client_id="home_assistant")
            for cell,os_name,arch,mode in (
                ("home_assistant__macos_arm64","macOS","arm64","installed-app-launchagent"),
                ("home_assistant__debian13_amd64","Debian 13","amd64","installed-deb-systemd"),
                ("home_assistant__debian13_arm64","Debian 13","arm64","installed-deb-systemd"),
            ):
                candidate=copy.deepcopy(cfg); candidate["cell_id"]=cell; candidate["expected"].update(os=os_name,architecture=arch,service_mode=mode)
                self.assertEqual(validate_session_config(candidate)["cell_id"],cell)
            bad=copy.deepcopy(cfg); bad["cell_id"]="home-assistant__debian13_amd64"
            with self.assertRaises(ContractError): validate_session_config(bad)
            bad=copy.deepcopy(cfg); bad.update(cell_id="home_assistant__debian13_amd64",client_id=True)
            with self.assertRaises(ContractError): validate_session_config(bad)

    def test_systemd_numeric_cld_exit_is_retained_and_required(self):
        controller=LinuxController(registered(),{"guest_inputs":{},"controller_root":"/tmp"})
        before={"MainPID":"41","ControlGroup":"/system.slice/teslatlas-hub.service","InvocationID":"0123456789abcdef0123456789abcdef"}
        after={"Result":"success","ExecMainCode":"1","ExecMainStatus":"0"}
        rows=iter((before,after)); controller._systemd=lambda:next(rows)
        controller._run=lambda argv,timeout=15,allowed_status=(0,):subprocess.CompletedProcess(argv,0,b"",b"")
        controller._process=lambda pid,expected_argv=None,service_owned=False:{"pid":pid,"start_identity":"101"}
        controller.verify_stopped=lambda owned=None,initial=False:{"status":"stopped"}
        evidence=controller.stop()
        self.assertEqual((evidence["exec_main_code_raw"],evidence["exec_main_code_semantic"],evidence["exec_main_status_raw"]),("1","exited","0"))
        for result,code,status in (("signal","2","15"),("signal","3","9"),("exit-code","1","1")):
            rejected=LinuxController(registered(),{"guest_inputs":{},"controller_root":"/tmp"}); count=[0]
            def systemd():
                count[0]+=1
                return before if count[0]==1 else {"Result":result,"ExecMainCode":code,"ExecMainStatus":status}
            rejected._systemd=systemd; rejected._run=controller._run; rejected._process=controller._process; rejected.verify_stopped=controller.verify_stopped; rejected.set_deadline(Deadline(0.01))
            with self.assertRaises(TimeoutError): rejected.stop()

    def test_macos_hub_absence_timestamp_after_boundary_is_rejected(self):
        values=iter((1_799_999_999,1_800_000_001,1_800_000_001))
        self.assertIsNone(_observe_absent_before({"pid":41},1_800_000_000,clock=lambda:next(values),present=lambda _identity:False,pause=lambda _seconds:None))
        values=iter((1_700_000_000,1_700_000_001))
        self.assertEqual(_observe_absent_before({"pid":41},1_800_000_000,clock=lambda:next(values),present=lambda _identity:False,pause=lambda _seconds:None),1_700_000_001)

    def test_resource_lease_conflicts_across_different_output_roots(self):
        with tempfile.TemporaryDirectory() as raw:
            first = registered()
            first.config["local_private_root"] = str(Path(raw) / "one")
            second = RegisteredConfig(dict(first.config, local_private_root=str(Path(raw) / "two"), session_id="22222222-2222-4222-8222-222222222222"), first.registration, first.registration_sha256)
            one = HostLease(first, lock_root=Path(raw) / "locks").acquire(10)
            try:
                with self.assertRaisesRegex(ContractError, "active controller"):
                    HostLease(second, lock_root=Path(raw) / "locks").acquire(10)
            finally:
                one.release()

    def test_expired_lease_is_rejected_before_transport_open(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw); cfg, inventory = inputs(directory)
            reg_path = Path(cfg["host_registration"]["path"]); reg = json.loads(reg_path.read_text())
            reg["lease"]["expires_at_unix"] = int(time.time()) + 1
            payload = json.dumps(reg, sort_keys=True, separators=(",", ":")).encode(); reg_path.write_bytes(payload)
            cfg["host_registration"]["sha256"] = hashlib.sha256(payload).hexdigest(); inventory[cfg["host_id"]] = cfg["host_registration"]["sha256"]
            transport = FakeTransport()
            with self.assertRaisesRegex(ContractError, "does not cover"):
                InstalledSession.open(cfg, inventory, transport=transport, verify_local_inputs=False)
            self.assertIsNone(transport.registered)

    def test_session_ssh_has_one_explicit_forward_and_no_inherited_config(self):
        reg = raw_registration()
        argv = build_session_ssh_argv(reg)
        self.assertEqual(argv[argv.index("-F") + 1], "/dev/null")
        self.assertEqual(argv.count("127.0.0.1:18480:127.0.0.1:18480"), 1)
        self.assertNotIn("ClearAllForwardings=yes", argv)
        self.assertIn("HostKeyAlias=" + reg["ssh"]["host_alias"], argv)
        self.assertEqual(argv[-1], reg["ssh"]["user"] + "@" + reg["ssh"]["hostname"])

    def test_normal_tls_captures_close_leaf_and_rejects_chain_hostname_redirect(self):
        def run_server(cert, key, redirect=False, ready_failures=0):
            _Handler.redirect = redirect
            _Handler.ready_failures = ready_failures
            server = _Server(("127.0.0.1", 18480), _Handler)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(str(cert), str(key))
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            return server, thread
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            profile = Path(__file__).resolve().parents[4] / "teslatlas-protocol/profiles/hub-http-v1/1.0.0/SHA256SUMS"
            good_cert, good_key = _certificate(root / "good" if False else root, "127.0.0.1")
            server, thread = run_server(good_cert, good_key, ready_failures=1)
            try:
                result = normal_tls_probe(str(good_cert), "2026.36.2", Deadline(3), profile)
                self.assertEqual(result["hub_id"], "11111111-1111-4111-8111-111111111111")
                self.assertEqual(_Handler.ready_failures, 0)
            finally:
                server.shutdown(); server.server_close(); thread.join()
            wrong = root / "wrong"; wrong.mkdir()
            wrong_cert, wrong_key = _certificate(wrong, "localhost")
            server, thread = run_server(wrong_cert, wrong_key)
            try:
                with self.assertRaises(ssl.SSLCertVerificationError):
                    normal_tls_probe(str(wrong_cert), "2026.36.2", Deadline(3), profile)
                with self.assertRaises(ssl.SSLCertVerificationError):
                    normal_tls_probe(str(good_cert), "2026.36.2", Deadline(3), profile)
            finally:
                server.shutdown(); server.server_close(); thread.join()
            server, thread = run_server(good_cert, good_key, redirect=True)
            try:
                with self.assertRaisesRegex(ValueError, "failed or redirected"):
                    normal_tls_probe(str(good_cert), "2026.36.2", Deadline(3), profile)
            finally:
                server.shutdown(); server.server_close(); thread.join()

    def test_tls_slow_drip_cannot_reset_whole_response_deadline(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw); cert,key=_certificate(root)
            class Slow(http.server.BaseHTTPRequestHandler):
                protocol_version="HTTP/1.1"
                def do_GET(self):
                    body=b'{"protocol":"teslatlas-sync"}'
                    self.send_response(200); self.send_header("Content-Length",str(len(body))); self.send_header("Connection","close"); self.end_headers()
                    for byte in body:
                        try: self.wfile.write(bytes([byte])); self.wfile.flush()
                        except BrokenPipeError: break
                        time.sleep(0.02)
                def log_message(self,*_args): pass
            server=_Server(("127.0.0.1",18480),Slow)
            context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); context.load_cert_chain(str(cert),str(key)); server.socket=context.wrap_socket(server.socket,server_side=True)
            thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
            try:
                started=time.monotonic()
                with self.assertRaises(TimeoutError): normal_tls_probe(str(cert),"2026.36.2",Deadline(0.08))
                self.assertLess(time.monotonic()-started,0.4)
            finally:
                server.shutdown(); server.server_close(); thread.join()

    def test_linux_real_observer_construction_passes_admission_and_rejects_store_replacement(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            r = registered()
            r.registration["guest"]["python_toml_module"] = "tomllib"
            r.registration["guest"]["permitted_service_user"] = "teslatlas"
            manifest = root / "manifest.json"; manifest.write_text(json.dumps({"store_schema_version": 59}))
            ca = root / "public-ca.pem"; ca.write_text("synthetic")
            machine = root / "machine-id"; machine.write_text("machine")
            os_release = root / "os-release"; os_release.write_text('ID=debian\nVERSION_ID="13"\n')
            fragment = root / "unit"; fragment.write_text("unit")
            r.registration["guest"]["machine_identity_sha256"] = hashlib.sha256(machine.read_bytes()).hexdigest()
            r.config["package_manifest"]["sha256"] = "e" * 64
            private = {"guest_inputs": {"package_manifest": str(manifest), "scenario": str(root / "scenario"), "seed": str(root / "seed"), "profile": str(root / "profile")}, "controller_root": str(root), "installation_receipt": str(root / "receipt")}
            for name, digest in (("scenario", "3" * 64), ("seed", "1" * 64), ("profile", "2" * 64)):
                p = root / name; p.write_bytes(bytes.fromhex(digest))
            controller = LinuxController(r, private)
            expected = observation()
            proc = expected["service"]["hub"]
            status = {"LoadState":"loaded","ActiveState":"active","SubState":"running","MainPID":"40","ControlGroup":"/system.slice/teslatlas-hub.service","InvocationID":"0123456789abcdef0123456789abcdef","FragmentPath":"/usr/lib/systemd/system/teslatlas-hub.service","DropInPaths":"","Result":"success","ExecMainCode":"0","ExecMainStatus":"0"}
            controller._systemd = lambda: status
            controller._process = lambda pid, expected_argv=None, service_owned=False: proc if pid == 40 else expected["observer"]["process"]
            controller._listener = lambda pid: expected["listener"]
            config = 'data_dir = "{}"\nbind = "127.0.0.1:18480"\n[tls]\npublic_url = "https://127.0.0.1:18480"\ncertificate_path = "{}/server.pem"\nprivate_key_path = "{}/server-key.pem"\n[collector]\ninterval_seconds = 0\nowner_api_base_url = "https://127.0.0.1:1/"\n[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n[geocoder]\nenabled = false\n[http]\nallowed_origins = ["http://localhost:18481"]\n'.format(controller.data_root+"/hub", controller.data_root, controller.data_root)
            controller.cfg["allowed_origins"] = ["http://localhost:18481"]
            store_id="11111111-1111-4111-8111-111111111111"
            scenario_raw=json.dumps({"schema_version":1,"name":"two-vehicles-five-drives","provenance":"synthetic-only","later_current":{}}).encode()
            controller.cfg["scenario"]["sha256"]=hashlib.sha256(scenario_raw).hexdigest()
            controller._service_read = lambda path: (config.encode() if path == CONFIG else scenario_raw if path.endswith("scenario.json") else (b"0::/system.slice/teslatlas-hub.service\n" if path.startswith("/proc/") else json.dumps({"hub_id":store_id}).encode()))
            receipt = {"package_sha256":"d"*64,"package_manifest_sha256":"e"*64,"installed_version":"2026.36.2-1","payload_manifest_sha256":"e"*64}
            controller._receipt = lambda: (receipt, "f" * 64)
            controller._run = lambda argv, timeout=15, allowed_status=(0,): subprocess.CompletedProcess(argv, 0, ((json.dumps([store_id,59])+"\n").encode() if "-c" in argv and "sqlite3" in argv[argv.index("-c")+1] else b"amd64\n" if "--print-architecture" in argv else b"observed-kernel\n"), b"")
            real_path = Path
            def mapped(value):
                return machine if str(value) == "/etc/machine-id" else os_release if str(value) == "/etc/os-release" else real_path(value)
            parsed_config = {"data_dir":controller.data_root+"/hub","bind":"127.0.0.1:18480","tls":{"public_url":"https://127.0.0.1:18480","certificate_path":controller.data_root+"/server.pem","private_key_path":controller.data_root+"/server-key.pem"},"collector":{"interval_seconds":0,"owner_api_base_url":"https://127.0.0.1:1/","legacy_auth":{"enabled":False}},"terrain":{"enabled":False},"geocoder":{"enabled":False},"http":{"allowed_origins":["http://localhost:18481"]}}
            with mock.patch("tools.interop.installed_hosts.linux.Path", side_effect=mapped), mock.patch("tools.interop.installed_hosts.linux.loads_toml", return_value=parsed_config), mock.patch("tools.interop.installed_hosts.linux.verify_payload_members", return_value="e"*64), mock.patch("tools.interop.installed_hosts.linux.normal_tls_probe", return_value={"certificate_der_sha256":"1"*64,"hub_id":store_id,"response_sha256":"2"*64,"validated_response_set_sha256":"3"*64}), mock.patch("tools.interop.installed_hosts.linux.sha256_file", side_effect=lambda p: {"/usr/lib/systemd/system/teslatlas-hub.service":"a"*64,str(machine):r.registration["guest"]["machine_identity_sha256"],str(root/"scenario"):controller.cfg["scenario"]["sha256"],str(root/"seed"):"1"*64,str(root/"profile"):"2"*64}.get(str(p), "f"*64)):
                proof = controller.observe(); proof.update(schema_version=1,status="verified",session_id=r.config["session_id"],sequence=1,challenge="d"*64)
                self.assertEqual(validate_installed_observation(proof, r)["config"]["store_id"], store_id)
                changed = copy.deepcopy(proof); changed["config"]["store_id"] = "other-store"
                with self.assertRaises(ContractError): validate_installed_observation(changed, r)

    def test_macos_real_observer_construction_passes_admission(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw); r=registered("macOS"); r.registration["guest"].update(python_toml_module="tomllib", permitted_service_user="matrix")
            expected=observation("macOS"); ioreg=b"synthetic-ioreg"; r.registration["guest"]["machine_identity_sha256"]=hashlib.sha256(ioreg).hexdigest()
            manifest=root/"manifest"; manifest.write_text(json.dumps({"store_schema_version":59,"package_scripts":{"launchagent_template":{"sha256":"7"*64}}}))
            for name in ("scenario","seed","profile"):(root/name).write_text(name)
            private={"guest_inputs":{"package_manifest":str(manifest),"scenario":str(root/"scenario"),"seed":str(root/"seed"),"profile":str(root/"profile")},"controller_root":str(root),"installation_receipt":str(root/"receipt")}
            controller=MacOSController(r,private); controller.connection={"hub_id":"hub-uuid-1"}; controller.app_process=expected["service"]["app_process"]
            plist_argv=[WRAPPER,"--config",controller.config_path,"--stdout-log",controller.home+"/Library/Logs/Teslatlas Hub/hub.out.log","--stderr-log",controller.home+"/Library/Logs/Teslatlas Hub/hub.err.log"]
            plist=root/"launch.plist"; plist.write_bytes(__import__("plistlib").dumps({"Label":"com.teslatlas.hub","ProgramArguments":plist_argv}))
            config_file=root/"config"; config_file.write_text("synthetic")
            controller._launch_status=lambda required=True:(40,"launch")
            controller._hub_child=lambda pid,argv:expected["service"]["hub"]
            controller._listener=lambda pid:expected["listener"]
            controller._receipt=lambda:({"package_sha256":"d"*64,"package_manifest_sha256":"e"*64,"installed_version":"2026.36.2","payload_manifest_sha256":"e"*64},"f"*64)
            controller.current_context=lambda ownership=None:{"path":controller.config_path,"sha256":hashlib.sha256(b"synthetic").hexdigest(),"data_dir":controller.data_root+"/hub","store_id":"hub-uuid-1","store_schema_version":59}
            def run(argv,timeout=15,allowed_status=(0,)):
                out=ioreg if argv[0].endswith("ioreg") else b"13.7.4\n" if argv[0].endswith("sw_vers") else b"arm64\n" if argv[-1]=="-m" else b"observed-kernel\n"
                return subprocess.CompletedProcess(argv,0,out,b"")
            controller._run=run
            parsed={"data_dir":controller.data_root+"/hub","bind":"127.0.0.1:18480","tls":{"public_url":"https://127.0.0.1:18480","certificate_path":controller.data_root+"/server.pem","private_key_path":controller.data_root+"/server-key.pem"},"collector":{"interval_seconds":0,"owner_api_base_url":"https://127.0.0.1:1/","legacy_auth":{"enabled":False}},"terrain":{"enabled":False},"geocoder":{"enabled":False},"http":{"allowed_origins":r.config.get("allowed_origins",[])}}
            r.config["allowed_origins"]=[]
            real_path=Path
            def mapped(value):
                if str(value)==controller.plist_path:return plist
                if str(value)==controller.config_path:return config_file
                return real_path(value)
            def proc(pid,expected_argv=None,deadline=None):
                if pid==40:return expected["service"]["supervisor"]
                if pid==expected["service"]["hub"]["pid"]:return expected["service"]["hub"]
                return expected["observer"]["process"]
            digests={controller.plist_path:"a"*64,WRAPPER:"8"*64,str(root/"scenario"):"3"*64,str(root/"seed"):"1"*64,str(root/"profile"):"2"*64}
            with mock.patch("tools.interop.installed_hosts.macos.Path",side_effect=mapped),mock.patch("tools.interop.installed_hosts.macos._process",side_effect=proc),mock.patch("tools.interop.installed_hosts.macos.loads_toml",return_value=parsed),mock.patch("tools.interop.installed_hosts.macos.verify_payload_members",return_value="e"*64),mock.patch("tools.interop.installed_hosts.macos.normal_tls_probe",return_value={"certificate_der_sha256":"1"*64,"hub_id":"hub-uuid-1","response_sha256":"2"*64,"validated_response_set_sha256":"3"*64}),mock.patch("tools.interop.installed_hosts.macos.sha256_file",side_effect=lambda p:digests.get(str(p),"f"*64)):
                proof=controller.observe(); proof.update(schema_version=1,status="verified",session_id=r.config["session_id"],sequence=1,challenge="d"*64)
                self.assertEqual(validate_installed_observation(proof,r)["service"]["hub"]["pid"],41)

    def test_macos_stop_deadline_starts_before_command_and_binds_all_generations(self):
        with tempfile.TemporaryDirectory() as raw:
            r=registered("macOS"); r.registration["guest"].update(python_toml_module="tomllib", permitted_service_user="matrix")
            controller=MacOSController(r,{"controller_root":raw})
            base=observation("macOS")["service"]
            wrapper,hub,app=base["supervisor"],base["hub"],base["app_process"]
            controller.app_process=app
            controller._launch_status=lambda required=False:(wrapper["pid"],"loaded")
            controller._hub_child=lambda pid,argv:hub
            command_times=[]
            controller._run=lambda argv,timeout=15,allowed_status=(0,):(command_times.append(time.monotonic_ns()) or subprocess.CompletedProcess(argv,0,b"",b""))
            controller.verify_stopped=lambda owned=None,initial=False:{"status":"stopped"}
            seen=[]
            def absent(identity):
                seen.append(identity)
                return False
            with mock.patch("tools.interop.installed_hosts.macos._process",return_value=wrapper),mock.patch("tools.interop.installed_hosts.macos._generation_present",side_effect=absent):
                controller.stop()
            evidence=controller._last_stop
            self.assertLessEqual(evidence["stop_requested_monotonic_ns"],command_times[0])
            self.assertLessEqual(command_times[0],evidence["hub_absent_monotonic_ns"])
            self.assertEqual(seen[:3],[hub,app,wrapper])
            self.assertLess(evidence["elapsed_to_hub_absence_ns"],evidence["accepted_deadline_ns"])
            self.assertEqual(evidence["normal_exit_code"],"unavailable")

    def test_session_rejects_replaced_store_uuid_after_initial_verify(self):
        class Replaced(FakeTransport):
            def exchange(self, request, timeout):
                reply=super().exchange(request,timeout)
                if len([r for r in self.requests if r["op"]=="verify"])>1:
                    reply["result"]["proof"]["config"]["store_id"]="replacement"
                    reply["result"]["proof"]["discovery"]["hub_id"]="replacement"
                return reply
        with tempfile.TemporaryDirectory() as raw:
            session=InstalledSession.open(*inputs(Path(raw)),transport=Replaced(),verify_local_inputs=False)
            self.addCleanup(session._host_lease.release)
            self.addCleanup(session.journal.close)
            self.addCleanup(session._close_broker)
            session.request({"op":"verify"})
            with self.assertRaisesRegex(SessionError,"store identity changed"):
                session.request({"op":"verify"})
            with self.assertRaises(CleanupError):session.close()

    def test_full_linux_preparer_recipe_proves_absence_install_and_stopped_handoff(self):
        with tempfile.TemporaryDirectory() as raw:
            directory=Path(raw); cfg,inventory=inputs(directory)
            package=directory/"package.deb"; package.write_bytes(b"candidate-package")
            cfg["package"]={"path":str(package),"sha256":hashlib.sha256(package.read_bytes()).hexdigest()}
            members=[{"path":"/usr/bin/teslatlas-hub","sha256":"f"*64},{"path":"/usr/lib/systemd/system/teslatlas-hub.service","sha256":"a"*64}]
            manifest={"schema_version":1,"kind":"installed-host-package","package_sha256":cfg["package"]["sha256"],"product_version":"2026.36.2","package_manager_version":"2026.36.2-1","store_schema_version":59,"os":"Debian 13","architecture":"amd64","hub_executable":members[0],"platform_payload":{"hub_executable":members[0],"systemd_unit":members[1]},"payload_manifest_sha256":payload_manifest_digest(members),"payload_members":members,"package_scripts":{},"source_export_sha256":"b"*64,"source_manifest_sha256":"c"*64,"build_manifest_sha256":"d"*64}
            manifest_path=directory/"manifest.json"; manifest_path.write_text(json.dumps(manifest))
            cfg["package_manifest"]={"path":str(manifest_path),"sha256":hashlib.sha256(manifest_path.read_bytes()).hexdigest()}
            reg_path=Path(cfg["host_registration"]["path"]); reg=json.loads(reg_path.read_text()); reg["install_state"]="preinstall"; reg["guest"]["permitted_service_uid"]=None; reg["guest"]["machine_identity_sha256"]=hashlib.sha256(b"machine\n").hexdigest()
            raw_reg=json.dumps(reg,sort_keys=True,separators=(",", ":")).encode(); reg_path.write_bytes(raw_reg); cfg["host_registration"]["sha256"]=hashlib.sha256(raw_reg).hexdigest(); inventory[cfg["host_id"]]=cfg["host_registration"]["sha256"]
            bound=read_registered_config(cfg,inventory,required_install_state="preinstall")
            class SyntheticPreparer(HostPreparer):
                def __init__(self,registered): super().__init__(registered); self.commands=[]; self.installed=False
                def _transfer(self,raw,remote_path): self.commands.append(("transfer",remote_path,len(raw)))
                def _remote(self,argv,timeout,allowed_status=(0,),input_bytes=None):
                    self.commands.append(tuple(argv)); status=0; out=b""
                    if argv[:2]==["/bin/cat","/etc/machine-id"]: out=b"machine\n"
                    elif argv[:2]==["/bin/cat","/etc/os-release"]: out=b'ID=debian\nVERSION_ID="13"\n'
                    elif "--print-architecture" in argv: out=b"amd64\n"
                    elif argv[:3]==["/usr/bin/id","-u","teslatlas"]: status=0 if self.installed else 1; out=b"997\n" if self.installed else b""
                    elif argv[:3]==["/usr/bin/dpkg-query","-W","teslatlas-hub"]: status=1
                    elif argv[:2]==["/usr/bin/dpkg-query","-W"]: out=b"2026.36.2-1"
                    elif "/usr/bin/dpkg" in argv and "--install" in argv: self.installed=True
                    elif "/bin/systemctl" in argv and "stop" in argv and not self.installed: status=5
                    elif argv[0]=="/usr/bin/sha256sum":
                        target=argv[-1]; digest=cfg["package"]["sha256"] if target.startswith("/tmp/teslatlas-package-") else "f"*64 if target=="/usr/bin/teslatlas-hub" else "a"*64
                        out=(digest+"  "+target+"\n").encode()
                    elif argv[0]=="/usr/bin/teslatlas-hub" and "--version" in argv: out=b"teslatlas-hub 2026.36.2\n"
                    elif argv[:2]==["/bin/systemctl","show"]: out=b"LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\nControlGroup=\n"
                    return subprocess.CompletedProcess(argv,status,out,b"")
            preparer=SyntheticPreparer(bound); result=preparer.execute()
            self.assertTrue(result["preflight"]["account_absent"])
            self.assertEqual(result["registration_update"],{"install_state":"installed","permitted_service_uid":997})
            self.assertTrue(result["service_left_stopped"])
            self.assertTrue(any("--install" in command for command in preparer.commands if isinstance(command,tuple)))
            install_index=next(i for i,c in enumerate(preparer.commands) if isinstance(c,tuple) and "--install" in c)
            self.assertFalse(any(isinstance(c,tuple) and "/bin/systemctl" in c and "stop" in c for c in preparer.commands[:install_index]))
            class Failing(SyntheticPreparer):
                def _remote(self,argv,timeout,allowed_status=(0,),input_bytes=None):
                    if "/usr/bin/dpkg" in argv and "--install" in argv:
                        self.commands.append(tuple(argv)); self.installed=True
                        return subprocess.CompletedProcess(argv,1,b"",b"synthetic installer failed after starting service")
                    return super()._remote(argv,timeout,allowed_status,input_bytes)
            failing=Failing(bound)
            with self.assertRaisesRegex(RuntimeError,"installer failed"):
                failing.execute()
            obligation=Path(cfg["local_private_root"])/cfg["session_id"]/"preparation-obligation.json"
            retained=json.loads(obligation.read_text())
            self.assertEqual(retained["phase"],"failed-handoff")
            self.assertTrue(any("/bin/systemctl" in c and "stop" in c for c in failing.commands if isinstance(c,tuple)))
            self.assertTrue(any(c[:2]==("/bin/rm","-f") for c in failing.commands if isinstance(c,tuple)))

    def test_combined_macos_package_selects_one_service_template_and_app_component(self):
        raw=raw_registration()
        temporary=tempfile.TemporaryDirectory(); self.addCleanup(temporary.cleanup)
        cfg={"local_private_root":temporary.name,"session_id":"11111111-1111-4111-8111-111111111111","host_id":raw["host_id"],"package":{"sha256":"d"*64}}
        r=RegisteredConfig(cfg,raw,"1"*64)
        template_digest="7"*64
        manifest={"package_scripts":{"launchagent_template":{"sha256":template_digest}}}
        class Components(HostPreparer):
            def __init__(self,registered,duplicate=False): super().__init__(registered); self.duplicate=duplicate
            def _remote(self,argv,timeout,allowed_status=(0,),input_bytes=None):
                out=b""
                if "-name" in argv:
                    paths=["/expanded/App.pkg/PackageInfo","/expanded/Service.pkg/PackageInfo"]
                    if self.duplicate: paths.append("/expanded/OtherService.pkg/PackageInfo")
                    out=("\n".join(paths)+"\n").encode()
                elif argv[:1]==["/bin/cat"]:
                    identifier="com.teslatlas.hub.app" if "App.pkg" in argv[-1] else "com.teslatlas.hub.service"
                    out=('<pkg-info identifier="{}" version="2026.36.2"/>'.format(identifier)).encode()
                elif argv[:3]==["/usr/bin/shasum","-a","256"]:
                    out=(template_digest+"  "+argv[-1]+"\n").encode()
                return subprocess.CompletedProcess(argv,0,out,b"")
        result=Components(r)._inspect_macos_components("/candidate.pkg",manifest)
        self.assertEqual(result[1],"/expanded/Service.pkg/Scripts/com.teslatlas.hub.plist.in")
        self.assertEqual([item["identifier"] for item in result[2]],["com.teslatlas.hub.app","com.teslatlas.hub.service"])
        with self.assertRaisesRegex(RuntimeError,"ambiguous"):
            Components(r,duplicate=True)._inspect_macos_components("/candidate.pkg",manifest)

    def test_linux_seed_recipe_creates_owned_ancestors_then_installs_scenario(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw); r=registered(); r.registration["guest"].update(permitted_service_user="teslatlas",python_toml_module="tomllib")
            r.config.update(allowed_origins=[],guest_run_id="run-1/cell-1")
            seed=root/"seed"; seed.write_bytes(b"seed")
            scenario=root/"scenario"; scenario.write_text(json.dumps({"schema_version":1,"name":"two-vehicles-five-drives","provenance":"synthetic-only","later_current":{}}))
            manifest=root/"manifest"; manifest.write_text(json.dumps({"store_schema_version":59}))
            r.config["seed"]["sha256"]=hashlib.sha256(seed.read_bytes()).hexdigest(); r.config["scenario"]["sha256"]=hashlib.sha256(scenario.read_bytes()).hexdigest()
            private={"guest_inputs":{"seed":str(seed),"scenario":str(scenario),"package_manifest":str(manifest)},"controller_root":str(root),"ownership_path":str(root/"ownership")}
            class Recipe(LinuxController):
                def __init__(self,registered,private): super().__init__(registered,private); self.commands=[]; self.created=set()
                def _run(self,argv,timeout=15,allowed_status=(0,)):
                    self.commands.append(tuple(argv)); status=0; out=b""
                    if "/usr/bin/stat" in argv:
                        target=argv[-1]
                        if target in self.created: out=b"teslatlas:700:directory\n"
                        else: status=1
                    elif "/usr/bin/install" in argv and "-d" in argv: self.created.add(argv[-1])
                    elif argv[0:4]==["/usr/bin/sudo","-n","-u","teslatlas"] and argv[4]==str(seed):
                        out=json.dumps({"config_path":self.data_root+"/config.toml","certificate_path":self.data_root+"/server.pem","endpoint":"https://127.0.0.1:18480","hub_id":"hub-uuid-1"}).encode()
                    elif "-c" in argv and "sqlite3" in argv[argv.index("-c")+1]: out=b"59\n"
                    elif EXECUTABLE in argv and "pair" in argv:
                        expires=int(time.time()*1000)-1 if argv[-1]=="1" else int(time.time()*1000)+900000
                        out=json.dumps({"secret":"s","expiresAtMs":expires}).encode()
                    return subprocess.CompletedProcess(argv,status,out,b"")
                def _service_read(self,path):
                    if path.endswith("scenario.json"): return scenario.read_bytes()
                    if path.endswith("server.pem"): return b"certificate"
                    return b"data_dir='synthetic'\n"
                def current_context(self,ownership=None): return {"store_id":"hub-uuid-1"}
            controller=Recipe(r,private); result=controller.prepare()
            seed_index=next(i for i,c in enumerate(controller.commands) if str(seed) in c and "--output" in c)
            ancestor_indices=[i for i,c in enumerate(controller.commands) if "/usr/bin/install" in c and "-d" in c]
            scenario_index=next(i for i,c in enumerate(controller.commands) if c[-1].endswith("scenario.json"))
            self.assertTrue(ancestor_indices and max(ancestor_indices)<seed_index<scenario_index)
            self.assertEqual(result["seed_facts"],{"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":r.config["scenario"]["sha256"]})

    def test_broker_eof_triggers_cleanup_and_failure_is_durable(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport()
            session = InstalledSession.open(*inputs(Path(raw)), transport=transport, verify_local_inputs=False)
            client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); client.connect(session.descriptor["broker_socket"])
            client.recv(4096); client.close()
            deadline = time.monotonic() + 3
            while not transport.closed and time.monotonic() < deadline: time.sleep(0.01)
            self.assertTrue(transport.closed)
            with self.assertRaises(CleanupError): session.close()

    def test_close_waits_for_active_operation_before_cleanup_stop(self):
        class Slow(FakeTransport):
            def __init__(self):
                super().__init__(); self.entered=threading.Event(); self.release=threading.Event()
            def exchange(self, request, timeout):
                if request["op"] == "verify": self.entered.set(); self.release.wait(2)
                return super().exchange(request, timeout)
        with tempfile.TemporaryDirectory() as raw:
            transport=Slow(); session=InstalledSession.open(*inputs(Path(raw)), transport=transport, verify_local_inputs=False)
            operation=threading.Thread(target=lambda: session.request({"op":"verify"})); operation.start(); self.assertTrue(transport.entered.wait(1))
            result=[]
            closer=threading.Thread(target=lambda: result.append(session.close())); closer.start(); time.sleep(0.05)
            self.assertTrue(closer.is_alive()); transport.release.set(); operation.join(); closer.join()
            self.assertEqual([item["op"] for item in transport.requests][-1], "stop")

    def test_cleanup_quarantines_lease_and_retries_after_transport_failure(self):
        class RetryTransport(FakeTransport):
            def __init__(self): super().__init__(); self.close_calls=0
            def close(self,timeout=None,preserve_recovery=False):
                self.close_calls+=1
                if self.close_calls==1: raise RuntimeError("synthetic partial cleanup")
                self.closed=True
        with tempfile.TemporaryDirectory() as raw:
            transport=RetryTransport(); session=InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            session._cleanup_stage_budgets={key:0.05 for key in session._cleanup_stage_budgets}
            with self.assertRaises(CleanupError): session.close()
            self.assertFalse(session._host_lease.file.closed)
            with self.assertRaises(CleanupError): session.close()
            self.assertTrue(session._cleanup_complete)
            self.assertTrue(session._host_lease.file.closed)
            self.assertTrue(transport.closed)

    def test_cleanup_cancels_lock_exhaustion_before_releasing_host(self):
        class Interruptible(FakeTransport):
            def __init__(self): super().__init__(); self.release=threading.Event(); self.interrupted=False
            def interrupt(self,timeout=1): self.interrupted=True; self.release.set()
            def close(self,timeout=None,preserve_recovery=False): self.closed=True
        with tempfile.TemporaryDirectory() as raw:
            transport=Interruptible(); session=InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            session._cleanup_stage_budgets={key:0.05 for key in session._cleanup_stage_budgets}
            acquired=threading.Event()
            def hold():
                session._operation_lock.acquire(); acquired.set(); transport.release.wait(1); session._operation_lock.release()
            worker=threading.Thread(target=hold); worker.start(); self.assertTrue(acquired.wait(1))
            with self.assertRaises(CleanupError) as captured: session.close()
            worker.join()
            self.assertTrue(transport.interrupted)
            self.assertTrue(session._cleanup_complete)
            self.assertEqual(captured.exception.evidence.state,"failed")

    def test_before_use_context_rejection_precedes_linux_mutation(self):
        controller=LinuxController(registered(),{"guest_inputs":{},"controller_root":"/tmp"})
        commands=[]
        controller.current_context=lambda ownership=None: (_ for _ in ()).throw(RuntimeError("changed payload/config/store"))
        controller._systemd=lambda: commands.append("systemd")
        owned={"session_id":controller.cfg["session_id"],"lease":controller.reg["lease"],"config_path":CONFIG,"store_id":"old","state":"running","service":{"hub":{"pid":41},"control_group":"/system.slice/teslatlas-hub.service"}}
        with self.assertRaisesRegex(RuntimeError,"changed payload"):
            controller.assert_owned(owned)
        self.assertEqual(commands,[])

    def test_current_config_store_schema_and_payload_are_reread_from_source_seams(self):
        r=registered(); r.registration["guest"].update(python_toml_module="tomllib",permitted_service_user="teslatlas"); r.config["allowed_origins"]=[]
        private={"guest_inputs":{"package_manifest":"/synthetic/manifest","seed":"/synthetic/seed"},"controller_root":"/tmp","installation_receipt":"/receipt"}
        controller=LinuxController(r,private)
        state={"raw":None,"store":"11111111-1111-4111-8111-111111111111","schema":59,"payload":"e"*64}
        state["raw"]=('data_dir = "{0}/hub"\nbind = "127.0.0.1:18480"\n[tls]\npublic_url = "https://127.0.0.1:18480"\ncertificate_path = "{0}/server.pem"\nprivate_key_path = "{0}/server-key.pem"\n[collector]\ninterval_seconds = 0\nowner_api_base_url = "https://127.0.0.1:1/"\n[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n[geocoder]\nenabled = false\n[http]\nallowed_origins = []\n'.format(controller.data_root)).encode()
        controller._receipt=lambda:({"payload_manifest_sha256":"e"*64},"f"*64)
        scenario_raw=json.dumps({"schema_version":1,"name":"two-vehicles-five-drives","provenance":"synthetic-only","later_current":{}}).encode()
        r.config["scenario"]["sha256"]=hashlib.sha256(scenario_raw).hexdigest()
        controller._service_read=lambda path:state["raw"] if path==CONFIG else scenario_raw if path.endswith("scenario.json") else json.dumps({"hub_id":state["store"]}).encode()
        controller._run=lambda argv,timeout=15,allowed_status=(0,):subprocess.CompletedProcess(argv,0,(json.dumps([state["store"],state["schema"]])+"\n").encode(),b"")
        parsed={"data_dir":controller.data_root+"/hub","bind":"127.0.0.1:18480","tls":{"public_url":"https://127.0.0.1:18480","certificate_path":controller.data_root+"/server.pem","private_key_path":controller.data_root+"/server-key.pem"},"collector":{"interval_seconds":0,"owner_api_base_url":"https://127.0.0.1:1/","legacy_auth":{"enabled":False}},"terrain":{"enabled":False},"geocoder":{"enabled":False},"http":{"allowed_origins":[]}}
        with mock.patch("tools.interop.installed_hosts.linux.loads_toml",return_value=parsed), mock.patch("tools.interop.installed_hosts.linux.verify_payload_members",side_effect=lambda _p:state["payload"]), mock.patch("tools.interop.installed_hosts.linux.sha256_file",return_value=r.config["seed"]["sha256"]):
            original=controller.current_context(); ownership={"config":original,"store_schema_version":59}
            state["schema"]=60
            with self.assertRaisesRegex(RuntimeError,"changed"): controller.current_context(ownership)
            state["schema"]=59; state["store"]="22222222-2222-4222-8222-222222222222"
            with self.assertRaisesRegex(RuntimeError,"changed"): controller.current_context(ownership)
            state["store"]=original["store_id"]; state["payload"]="0"*64
            with self.assertRaisesRegex(RuntimeError,"payload differs"): controller.current_context(ownership)

    def test_guest_start_failure_retains_cleanup_obligation(self):
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"
            class Platform:
                acquisition_callback=None
                def __init__(self): self.private={"ownership_path":str(path)}; self.running=False; self.starts=0; self.stops=0
                def preflight(self): return {"stopped":True}
                def prepare(self): return {"invitation":{},"expired_invitation":{},"seed_facts":{"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64}}
                def paired_device_ids(self): return set()
                def start(self):
                    self.starts+=1; self.running=True
                    self.acquisition_callback({"acquisition_state":"complete","hub":{"pid":41,"start_identity":"1"}})
                    raise RuntimeError("synthetic start boundary")
                def stop(self): self.stops+=1; self.running=False; return {"operation":"stop","session_id":registered().config["session_id"],"normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped"}
            platform=Platform(); controller=GuestController(registered(),platform)
            with self.assertRaisesRegex(RuntimeError,"start boundary"): controller.open()
            _stopped,errors=controller.close()
            self.assertEqual(errors,[]); self.assertFalse(platform.running); self.assertGreaterEqual(platform.stops,1)

    def test_acquired_generation_is_durable_before_post_start_failure(self):
        with tempfile.TemporaryDirectory() as raw:
            path=Path(raw)/"ownership.json"; service={"hub":{"pid":41,"boot_id":"boot","start_identity":"1"}}
            class Platform:
                acquisition_callback=None
                def __init__(self): self.private={"ownership_path":str(path)}
                def preflight(self): return {"stopped":True}
                def prepare(self): return {"invitation":{},"expired_invitation":{},"seed_facts":{"store_id":"hub-uuid-1","store_schema_version":59,"scenario_sha256":"3"*64}}
                def paired_device_ids(self): return set()
                def start(self): self.acquisition_callback(service); raise RuntimeError("post-start verification")
                def stop(self): return {"normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): return {"status":"stopped"}
            platform=Platform(); controller=GuestController(registered(),platform)
            with self.assertRaisesRegex(RuntimeError,"post-start"):
                controller.open()
            durable=json.loads(path.read_text())
            self.assertEqual({key:durable["service"][key] for key in service},service)
            controller.close()

    def test_app_failure_after_generation_acquisition_retains_cleanup_identity(self):
        with tempfile.TemporaryDirectory() as raw:
            r=registered("macOS"); controller=MacOSController(r,{"controller_root":raw})
            app=observation("macOS")["service"]["app_process"]
            calls=[]
            def run(argv,timeout=15,allowed_status=(0,)):
                calls.append(tuple(argv))
                if argv[:2]==["/usr/bin/pgrep","-x"]:
                    found=sum(1 for call in calls if call[:2]==("/usr/bin/pgrep","-x"))
                    return subprocess.CompletedProcess(argv,1 if found==1 else 0,b"" if found==1 else b"55\n",b"")
                return subprocess.CompletedProcess(argv,0,b"",b"")
            controller._run=run
            with mock.patch("tools.interop.installed_hosts.macos._process",side_effect=[app,RuntimeError("synthetic post-acquisition failure")]):
                with self.assertRaisesRegex(RuntimeError,"post-acquisition"):
                    controller._launch_app_once()
            self.assertEqual(controller.app_process,app)
            with mock.patch("tools.interop.installed_hosts.macos._generation_present",return_value=False):
                controller.cleanup_partial()

    def test_fixed_recovery_binds_durable_ownership_before_stop(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw); ownership_path=root/"ownership.json"
            r=registered(); owned={"session_id":r.config["session_id"],"lease":r.registration["lease"],"store_id":"hub-uuid-1","state":"running","service":{"hub":{"pid":41}}}
            ownership_path.write_text(json.dumps(owned))
            private={"ownership_path":str(ownership_path)}
            calls=[]
            class Platform:
                def bind_recovery(self,value): calls.append(("bind",copy.deepcopy(value)))
                def assert_owned(self,value): calls.append(("assert",copy.deepcopy(value)))
                def stop(self): calls.append(("stop",None)); return {"session_id":r.config["session_id"],"exec_main_code_raw":"1","normal_exit":True}
                def verify_stopped(self,owned=None,initial=False): calls.append(("verify",copy.deepcopy(owned))); return {"status":"stopped"}
            platform=Platform(); output=[]
            with mock.patch.object(guest_module,"_load_private",return_value=(r,private)),mock.patch.object(guest_module,"_platform",return_value=platform),mock.patch.object(guest_module,"_write",side_effect=lambda value,*_args:output.append(value)):
                self.assertEqual(guest_module.main(["--recover-stop",str(root/"private.json"),"--budget-ms","1000"]),0)
            self.assertEqual([item[0] for item in calls],["bind","assert","stop","verify"])
            self.assertEqual(calls[1][1],owned)
            self.assertEqual(calls[-1][1]["state"],"stopped")
            self.assertEqual(json.loads(ownership_path.read_text())["stop_evidence"]["exec_main_code_raw"],"1")

    def test_peer_credential_api_failure_triggers_failed_cleanup(self):
        with tempfile.TemporaryDirectory() as raw:
            transport=FakeTransport(); session=InstalledSession.open(*inputs(Path(raw)),transport=transport,verify_local_inputs=False)
            session._peer_uid=lambda _connection: (_ for _ in ()).throw(OSError("synthetic credential failure"))
            client=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); client.connect(session.descriptor["broker_socket"]); client.close()
            deadline=time.monotonic()+3
            while not transport.closed and time.monotonic()<deadline: time.sleep(0.01)
            self.assertTrue(transport.closed)
            with self.assertRaises(CleanupError): session.close()

    def test_partial_transport_setup_removes_only_fixed_owned_members(self):
        r=registered()
        class Partial(SSHTransport):
            def __init__(self): super().__init__(); self.commands=[]
            def _run_remote(self,command,timeout,input_bytes=None,deadline=None,writer=False): self.commands.append(command); return b""
        transport=Partial(); transport.registered=r; transport.paths=remote_session_paths(r.registration,r.config["session_id"]); transport._root_created=True
        transport.close()
        self.assertTrue(transport._closed)
        self.assertEqual(len(transport.commands),1)
        self.assertIn("/bin/rm -f --",transport.commands[0])
        self.assertNotIn("/bin/cat",transport.commands[0])
        self.assertNotIn("rm -rf",transport.commands[0])


if __name__ == "__main__":
    unittest.main()
