#!/usr/bin/env python3
"""Private stdio control of a fresh native user-process Hub; no installed service claim."""
import importlib.util,json,os,platform,signal,socket,subprocess,sys,time,hashlib,sqlite3,threading
from pathlib import Path

def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path); m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
f=module('lane_fixture',Path(__file__).resolve().parent.parent/'fixture.py')
matrix=module('lane_matrix',Path(__file__).resolve().parent.parent/'run.py')
def require_python(version):
    if version < (3,11):raise RuntimeError('Python 3.11 or later required before process creation')

def terminate_owned(process):
    # Popen retains ownership/wait identity even when readiness never succeeded.
    if process.poll() is not None:return
    process.terminate()
    try:process.wait(timeout=15)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

class Supervisor:
    def __init__(self,config):
        self.config=config;self.process=None;self.events=[];self.log=None;self.ready=None
    def setup(self):
        c=self.config;self.root=Path(c['output_dir']);stage=self.root.parent/(self.root.name+'-executables');stage.mkdir(mode=0o700)
        self.binary,bh=f.stage_executable(c['binary'],stage/'teslatlas-hub');self.seed,sh=f.stage_executable(c['seed_binary'],stage/'interop_fixture')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',c['port']))
            subprocess.run(f.seed_command(c,self.seed,self.root,c['port']),check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=60)
        with sqlite3.connect((self.root/'hub/hub.sqlite').as_uri()+'?mode=ro',uri=True) as db:self.events.append({'operation':'seed','schema_version':db.execute('PRAGMA user_version').fetchone()[0]})
        self.d=f.read_private_json(self.root/'connection.json');f.configure_allowed_origins(self.d['config_path'],c['allowed_origins'])
        self.d.update(binary_path=str(self.binary),binary_sha256=bh,seed_binary_path=str(self.seed),seed_binary_sha256=sh,profile_id=c['profile_id'],profile_path=c['profile_path'],profile_sha256=c['profile_sha256'],status='ready',provenance='synthetic-real-process')
        self.scenario,scenario_sha256=f.copy_selected_scenario(self.root,c)
        self.d.update(scenario_path=str(self.scenario),scenario_sha256=scenario_sha256)
        self.expired=self.pair(1);self.invitation=self.pair(900)
        while time.time()*1000<=self.expired['expiresAtMs']:time.sleep(.05)
        self.start()
    def pair(self,seconds):
        if self.process is not None and self.process.poll() is None:raise ValueError('pair requires stopped owned Hub')
        result=subprocess.run([str(self.binary),'--config',self.d['config_path'],'pair','--json','--label','Owned matrix lane','--expires-in-seconds',str(seconds)],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,check=True,timeout=60)
        return json.loads(result.stdout)
    def start(self):
        if self.process is not None and self.process.poll() is None:raise ValueError('already running')
        self.log=open(self.root/'serve.log','ab');os.chmod(self.root/'serve.log',0o600)
        self.process=subprocess.Popen([str(self.binary),'--config',self.d['config_path'],'serve'],stdin=subprocess.DEVNULL,stdout=self.log,stderr=self.log)
        f.wait_ready(self.process,self.d)
        self.ready=dict(self.d,**f.ready_process_fields(self.process,self.root))
        tmp=self.root/'next-ready.json';f.write_private_json(tmp,self.ready);os.replace(tmp,self.root/'ready.json')
        proof=f.native_evidence.verify(self.ready);self.events.append({'operation':'start','proof':proof})
    def stop(self):
        if self.process is None or self.process.poll() is not None:return
        try:proof=f.native_evidence.verify(self.ready)
        except (ValueError,TypeError):proof={'status':'cleanup-only','hub_pid':self.process.pid}
        terminate_owned(self.process)
        with socket.socket() as sock:closed=sock.connect_ex(('127.0.0.1',self.config['port']))!=0
        self.events.append({'operation':'stop','proof':proof,'exit_code':self.process.returncode,'listener_closed':closed});self.log.close()
    def answer(self,op):
        name=op['op']
        if name=='stop':self.stop();return {'stopped':True,'events':self.events}
        if name=='start':self.start()
        elif name=='revoke':
            import uuid
            device=str(uuid.UUID(op['device_id']));self.stop()
            subprocess.run([str(self.binary),'--config',self.d['config_path'],'control','revoke-device',device],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True,timeout=60)
            self.events.append({'operation':'revoke','device_id':device});self.start()
        elif name=='pair':self.stop();self.invitation=self.pair(900);self.start()
        elif name!='verify':raise ValueError('unknown control operation')
        proof=f.native_evidence.verify(self.ready)
        return {'descriptor':self.ready,'proof':proof,'invitation':self.invitation,'expired_invitation':self.expired,'events':self.events}
    def cleanup(self):
        errors=[]
        try:self.stop()
        except Exception as error:
            errors.append({'resource':'hub-stop','error':type(error).__name__})
            try:
                if self.process is not None:terminate_owned(self.process)
            except Exception as rescue:errors.append({'resource':'hub-rescue','error':type(rescue).__name__})
        try:
            if self.log is not None:self.log.close()
        except Exception as error:errors.append({'resource':'hub-log','error':type(error).__name__})
        if not hasattr(self,'root') or not self.root.exists():return
        closed=False;schema=None
        try:
            with socket.socket() as sock:closed=sock.connect_ex(('127.0.0.1',self.config['port']))!=0
        except Exception as error:errors.append({'resource':'hub-listener','error':type(error).__name__})
        try:
            with sqlite3.connect((self.root/'hub/hub.sqlite').as_uri()+'?mode=ro',uri=True) as db:schema=db.execute('PRAGMA user_version').fetchone()[0]
        except Exception as error:errors.append({'resource':'hub-schema','error':type(error).__name__})
        alive=self.process is not None and self.process.poll() is None
        exit_code=self.process.returncode if self.process is not None else None
        value={'status':'stopped' if not errors and not alive and exit_code==0 and closed else 'failed','hub_pid':self.process.pid if self.process else None,'hub_exit_code':exit_code,'hub_alive':alive,'events':self.events,'listener_closed':closed,'schema_version':schema,'cleanup_errors':errors}
        f.write_private_json(self.root/'stopped.json',value)
        if value['status']!='stopped':raise RuntimeError('Hub cleanup failed')

def main():
    os.umask(0o077)
    require_python(sys.version_info)
    config=f.load_config(Path(sys.argv[1]));s=Supervisor(config)
    def terminate(*_):raise SystemExit(1)
    for sig in (signal.SIGTERM,signal.SIGINT):signal.signal(sig,terminate)
    timer=threading.Timer(config['lifetime_seconds'],lambda:os.kill(os.getpid(),signal.SIGTERM));timer.daemon=True;timer.start()
    try:
        s.setup()
        for line in sys.stdin:
            try:answer=s.answer(json.loads(line));print(json.dumps(answer),flush=True)
            except Exception as e:print(json.dumps({'error':type(e).__name__}),flush=True);return 1
    finally:timer.cancel();s.cleanup()
if __name__=='__main__':
    try:sys.exit(main())
    except Exception as e:print('owned supervisor failed: '+type(e).__name__,file=sys.stderr);sys.exit(1)
