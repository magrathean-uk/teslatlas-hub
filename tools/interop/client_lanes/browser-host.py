#!/usr/bin/env python3
"""Run only on the isolated Linux Chromium host; owns two fresh process groups."""
import hashlib,json,os,pathlib,platform,signal,subprocess,sys,time,urllib.request,socket,stat
def group_members(group):
    result=subprocess.run(['ps','-eo','pid=,pgid=,stat='],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,check=True,timeout=5)
    return [int(parts[0]) for line in result.stdout.splitlines() if len(parts:=line.split())==3 and int(parts[1])==group and 'Z' not in parts[2]]

class TerminationFailure(RuntimeError):
    def __init__(self,error,record):
        super().__init__(type(error).__name__)
        self.record=record
        self.error_type=type(error).__name__

def send_group_signal(process,sig,record):
    attempt={'signal':sig.name,'outcome':'pending'}
    record['signals'].append(attempt)
    # Retain an attempted forced rescue even if the group exits during killpg.
    if sig==signal.SIGKILL:record['escalations'].append(sig.name)
    try:
        os.killpg(process.pid,sig)
        attempt['outcome']='sent'
    except ProcessLookupError:attempt['outcome']='already_exited'
    except Exception as error:
        attempt.update(outcome='failed',error=type(error).__name__)
        raise

def terminate_group(process):
    # A zero-exit direct parent can still have a TERM-resistant descendant.
    record={'pid':process.pid,'signals':[],'escalations':[]}
    try:
        send_group_signal(process,signal.SIGTERM,record)
        end=time.monotonic()+2
        while group_members(process.pid) and time.monotonic()<end:time.sleep(.05)
        if group_members(process.pid):send_group_signal(process,signal.SIGKILL,record)
        process.wait(timeout=5)
        end=time.monotonic()+2
        while group_members(process.pid) and time.monotonic()<end:time.sleep(.05)
        record.update(exit_code=process.returncode,remaining_live_pids=group_members(process.pid))
        if record['remaining_live_pids']:raise RuntimeError('owned browser group survived cleanup')
        return record
    except Exception as error:
        record['exit_code']=process.poll()
        raise TerminationFailure(error,record) from error

def cleanup_groups(processes,terminate=terminate_group):
    groups=[];errors=[]
    for process in processes:
        try:
            record=terminate(process)
            record['status']='failed' if record['escalations'] else 'passed'
            if record['escalations']:errors.append({'resource':'browser-group','pid':process.pid,'error':'forced_rescue'})
        except Exception as error:
            record=getattr(error,'record',{'pid':process.pid,'signals':[],'escalations':[]})
            error_type=getattr(error,'error_type',type(error).__name__)
            record.update(status='failed',error=error_type)
            errors.append({'resource':'browser-group','pid':process.pid,'error':error_type})
            # Preserve prior attempts, independently rescue this group, then
            # continue all remaining groups even if the rescue itself fails.
            try:
                send_group_signal(process,signal.SIGKILL,record)
                process.wait(timeout=5)
                record.update(exit_code=process.returncode,remaining_live_pids=group_members(process.pid))
            except Exception as rescue:record['rescue_error']=type(rescue).__name__
        groups.append(record)
    return {'groups':groups,'errors':errors}

def verify_cleanup(root):
    path=pathlib.Path(root)/'cleanup.json';metadata=path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=os.getuid() or metadata.st_mode&0o077:raise RuntimeError('invalid private cleanup record')
    value=json.loads(path.read_text())
    current=[{'pid':g['pid'],'live_pids':group_members(g['pid'])} for g in value['groups']]
    ports=[]
    for port in (18480,18481,18483,18484):
        with socket.socket() as sock:ports.append({'port':port,'closed':sock.connect_ex(('127.0.0.1',port))!=0})
    value.update(current_groups=current,ports=ports,verified=value['status']=='stopped' and not value['errors'] and all(g.get('status')=='passed' and g.get('escalations')==[] and g.get('signals') and all(s.get('signal')=='SIGTERM' and s.get('outcome') in ('sent','already_exited') for s in g['signals']) for g in value['groups']) and all(not g['live_pids'] for g in current) and all(p['closed'] for p in ports))
    return value

def main():
    os.umask(0o077)
    config=json.loads(sys.stdin.readline());root=pathlib.Path(config['root']);root.mkdir(mode=0o700)
    certificate=root/'ca.pem';certificate.write_text(config['certificate']);processes=[];logs=[]
    def cleanup():
        value=cleanup_groups(processes)
        for log in logs:
            try:log.close()
            except Exception as error:value['errors'].append({'resource':'browser-log','error':type(error).__name__})
        value.update(status='failed' if value['errors'] else 'stopped',scenario_error=scenario_error)
        (root/'cleanup.json').write_text(json.dumps(value))
        return value
    scenario_error=None
    for sig in (signal.SIGTERM,signal.SIGINT):signal.signal(sig,lambda *_:sys.exit(1))
    try:
        result={}
        for kind,port in [('trusted',18483),('untrusted',18484)]:
            home=root/kind;home.mkdir(mode=0o700);db=home/'.pki/nssdb';db.mkdir(parents=True)
            subprocess.run(['certutil','-N','-d','sql:'+str(db),'--empty-password'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            if kind=='trusted':subprocess.run(['certutil','-A','-d','sql:'+str(db),'-n','Owned lane CA','-t','C,,','-i',str(certificate)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            exported=subprocess.run(['certutil','-L','-d','sql:'+str(db),'-n','Owned lane CA','-a'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
            if (kind=='trusted')!=(exported.returncode==0):raise RuntimeError('NSS trust isolation failed')
            log=open(root/(kind+'.log'),'wb');logs.append(log)
            argv=['/usr/bin/chromium','--headless','--disable-gpu','--enable-automation','--no-first-run','--no-default-browser-check','--disable-background-networking','--disable-component-update','--disable-sync','--metrics-recording-only','--disable-quic','--proxy-server=http://127.0.0.1:18489','--proxy-bypass-list=localhost;127.0.0.1','--force-webrtc-ip-handling-policy=disable_non_proxied_udp','--remote-debugging-address=127.0.0.1',f'--remote-debugging-port={port}',f'--user-data-dir={home}/profile','about:blank']
            p=subprocess.Popen(argv,env={**os.environ,'HOME':str(home),'XDG_CONFIG_HOME':str(home/'.config'),'XDG_CACHE_HOME':str(home/'.cache')},stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True);processes.append(p)
            end=time.monotonic()+30
            while True:
                if p.poll() is not None:raise RuntimeError('Chromium exited')
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/version',timeout=1) as response:version=json.load(response)
                    break
                except OSError:
                    if time.monotonic()>end:raise RuntimeError('Chromium startup timeout')
                    time.sleep(.1)
            proc=pathlib.Path(f'/proc/{p.pid}');actual=os.readlink(proc/'exe');status=(proc/'status').read_text()
            if int(next(l.split()[1] for l in status.splitlines() if l.startswith('Uid:')))!=os.getuid():raise RuntimeError('browser UID mismatch')
            result[kind]={'pid':p.pid,'executable':actual,'executable_sha256':hashlib.sha256(pathlib.Path(actual).read_bytes()).hexdigest(),'proc_stat':(proc/'stat').read_text(),'nss_database':str(db),'ca_export':exported.stdout.decode() if kind=='trusted' else None,'version':version['Browser'],'home':str(home)}
        result['runtime']={'os':'linux','architecture':platform.machine(),'distribution':platform.freedesktop_os_release()['PRETTY_NAME'],'kernel':platform.release(),'uid':os.getuid()}
        print(json.dumps(result),flush=True)
        # Parent holds stdin. EOF or bounded lifetime closes only these owned groups.
        import select
        select.select([sys.stdin],[],[],300)
    except BaseException as error:
        scenario_error=type(error).__name__
        raise
    finally:
        outcome=cleanup()
        if scenario_error is None and outcome['errors']:raise RuntimeError('browser cleanup failed')

if __name__=='__main__':
    if len(sys.argv)==3 and sys.argv[1]=='--verify-cleanup':print(json.dumps(verify_cleanup(sys.argv[2])))
    else:main()
