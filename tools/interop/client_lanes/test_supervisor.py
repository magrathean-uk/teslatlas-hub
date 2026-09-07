import importlib.util,subprocess,sys,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('lane_supervisor',Path(__file__).with_name('supervisor.py'));s=importlib.util.module_from_spec(spec);spec.loader.exec_module(s)
class CleanupTests(unittest.TestCase):
    def test_owned_child_is_reaped_even_without_readiness(self):
        p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])
        try:
            s.terminate_owned(p)
            self.assertIsNotNone(p.poll())
        finally:
            if p.poll() is None:p.kill();p.wait()
    def test_unsupported_python_rejected_before_seed(self):
        self.assertRaises(RuntimeError,s.require_python,(3,9))
        s.require_python((3,14))

spec2=importlib.util.spec_from_file_location('browser_host',Path(__file__).with_name('browser-host.py'));b=importlib.util.module_from_spec(spec2)
class BrowserCleanupTests(unittest.TestCase):
    def test_parent_exit_does_not_leave_term_resistant_descendant(self):
        spec2.loader.exec_module(b)
        p=subprocess.Popen([sys.executable,'-c','import os,signal,time; pid=os.fork(); print(pid,flush=True) if pid else None; exit(0) if pid else (signal.signal(signal.SIGTERM,signal.SIG_IGN),time.sleep(30))'],stdout=subprocess.PIPE,text=True,start_new_session=True)
        try:
            p.stdout.readline();p.wait(timeout=5)
            result=b.terminate_group(p)
            self.assertEqual(result['remaining_live_pids'],[])
        finally:
            import os,signal
            try:os.killpg(p.pid,signal.SIGKILL)
            except ProcessLookupError:pass
            p.stdout.close()



class IndependentRemoteCleanupTests(unittest.TestCase):
    def test_first_group_failure_still_terminates_second_and_retains_error(self):
        spec2.loader.exec_module(b)
        processes=[subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'],start_new_session=True) for _ in range(2)]
        calls=[]
        def injected(process):
            calls.append(process.pid)
            if process is processes[0]:raise RuntimeError('injected first-group failure')
            return b.terminate_group(process)
        try:
            result=b.cleanup_groups(processes,terminate=injected)
            self.assertEqual(calls,[p.pid for p in processes]);self.assertEqual(result['groups'][0]['status'],'failed');self.assertEqual(result['groups'][1]['status'],'passed')
            self.assertTrue(all(p.poll() is not None for p in processes));self.assertTrue(result['errors'])
            self.assertEqual(result['groups'][0]['escalations'],['SIGKILL']);self.assertEqual(result['groups'][0]['signals'],[{'signal':'SIGKILL','outcome':'sent'}])
        finally:
            for p in processes:
                if p.poll() is None:p.kill();p.wait()

class RemoteSignalEvidenceTests(unittest.TestCase):
    def test_signal_failure_preserves_attempt_and_rescue_outcome(self):
        from unittest.mock import patch
        import signal
        spec2.loader.exec_module(b)
        p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'],start_new_session=True)
        original=b.os.killpg
        def fail_term(pid,sig):
            if sig==signal.SIGTERM:raise PermissionError('injected signal failure')
            return original(pid,sig)
        try:
            with patch.object(b.os,'killpg',fail_term):result=b.cleanup_groups([p])
            group=result['groups'][0]
            self.assertEqual(group['status'],'failed');self.assertEqual(group['remaining_live_pids'],[])
            self.assertEqual(group['signals'],[{'signal':'SIGTERM','outcome':'failed','error':'PermissionError'},{'signal':'SIGKILL','outcome':'sent'}])
            self.assertEqual(group['escalations'],['SIGKILL']);self.assertTrue(result['errors'])
        finally:
            if p.poll() is None:p.kill();p.wait()

if __name__=='__main__':unittest.main()
