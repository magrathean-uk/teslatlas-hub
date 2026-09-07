import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import http from 'node:http';
import {cleanupAll,stopOwnedGroup,admitExit,validateHubStopped,commitOutcome,assertPortsClosed} from './cleanup.mjs';
test('first close failure still attempts second browser, SSH, server and verification',async()=>{
 const attempted=[];const server=http.createServer((_q,r)=>r.end());await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;
 const cleanup=await cleanupAll([
  ['trusted',async()=>{attempted.push('trusted');throw Error('injected close rejection')}],
  ['untrusted',async()=>{attempted.push('untrusted')}],
  ['ssh',async()=>{attempted.push('ssh')}],
  ['server',async()=>{attempted.push('server');await new Promise(r=>server.close(r))}],
  ['ports',()=>assertPortsClosed([port])],
 ]);
 assert.deepEqual(attempted,['trusted','untrusted','ssh','server']);assert.equal(cleanup.status,'failed');assert.equal(cleanup.actions.length,5);
 const writes=[];const code=await commitOutcome({receipt:{cases:[]},scenario_error:{code:'original_scenario_failure'},cleanup},async(name,value)=>writes.push({name,value}));
 assert.equal(code,1);assert.ok(!writes.some(x=>x.name==='receipt'));assert.equal(writes.find(x=>x.name==='failure').value.scenario_error.code,'original_scenario_failure');
});
test('exit7 and signal supervisor results cannot be admitted',async()=>{
 for(const script of ['process.exit(7)','process.kill(process.pid,"SIGTERM")']){
  const child=spawn(process.execPath,['-e',script],{stdio:['ignore','ignore','ignore'],detached:true});
  await new Promise(r=>child.once('exit',r));const outcome=await stopOwnedGroup(child);assert.throws(()=>admitExit(outcome));
 }
});
test('missing failed or mismatched stopped evidence prevents successful receipt',async()=>{
 for(const value of [null,{}, {status:'stopped',hub_exit_code:7,listener_closed:true}, {status:'stopped',hub_exit_code:0,listener_closed:false}])assert.throws(()=>validateHubStopped(value,{hub_pid:123}));
 const writes=[];assert.equal(await commitOutcome({receipt:{cases:[]},scenario_error:null,cleanup:{status:'failed',actions:[]}},async name=>writes.push(name)),1);assert.ok(!writes.includes('receipt'));
});
test('only completed verified cleanup permits the final receipt write',async()=>{
 const writes=[];assert.equal(await commitOutcome({receipt:{cases:[{status:'passed'}]},scenario_error:null,cleanup:{status:'passed',actions:[]},transport:{}},async name=>writes.push(name)),0);assert.deepEqual(writes,['cleanup','transport','receipt']);
});
import {validateRemoteCleanup} from './cleanup.mjs';
test('missing remote cleanup or remaining browser processes cannot pass',()=>{
 for(const value of [null,{}, {status:'stopped',verified:false,groups:[],errors:[],ports:[]}])assert.throws(()=>validateRemoteCleanup(value,[123]));
});
test('cleanup timeout still attempts later resources',async()=>{
 let later=false;const result=await cleanupAll([['hung-close',()=>new Promise(()=>{}),10],['later',async()=>{later=true}]]);assert.equal(result.status,'failed');assert.equal(later,true);
});
test('owned group cleanup reaps descendants even after a zero-exit parent',async()=>{
 const script='const{spawn}=require("node:child_process");const c=spawn(process.execPath,["-e","process.on(\\\"SIGTERM\\\",()=>{});setInterval(()=>{},1000)"],{stdio:"ignore"});c.unref();setTimeout(()=>process.exit(0),200)';
 const parent=spawn(process.execPath,['-e',script],{stdio:'ignore',detached:true});await new Promise(r=>parent.once('exit',r));
 const outcome=await stopOwnedGroup(parent,{graceMs:10,termMs:50,killMs:1000});assert.equal(outcome.exit_code,0);assert.ok(outcome.escalations.includes('SIGKILL'));assert.equal(outcome.remaining_live_pids.length,0);assert.throws(()=>admitExit(outcome));
});
import {createHash} from 'node:crypto';
test('receipt binds the exact admitted cleanup document digest',async()=>{
 const receipt={cases:[{id:'candidate_artifact_identity',status:'passed',process_evidence:{hub:{}}}]};const cleanup={status:'passed',actions:[{resource:'unit-cleanup',status:'passed'}]};
 const values={};await commitOutcome({receipt,cleanup,scenario_error:null},async(k,v)=>{values[k]=JSON.stringify(v,null,2)+'\n'});
 const expected=createHash('sha256').update(values.cleanup).digest('hex');assert.equal(JSON.parse(values.receipt).cases[0].process_evidence.hub.cleanup_sha256,expected);
});

import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const remoteProbe=String.raw`
import importlib.util,json,os,pathlib,signal,subprocess,sys,tempfile
spec=importlib.util.spec_from_file_location('browser_host',sys.argv[1]);b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)
resistant="import os,signal,time; r,w=os.pipe(); pid=os.fork(); (os.close(r),signal.signal(signal.SIGTERM,signal.SIG_IGN),os.write(w,b'1'),os.close(w),time.sleep(30)) if pid==0 else (os.close(w),os.read(r,1),print(pid,flush=True))"
ordinary="import signal,time; signal.signal(signal.SIGTERM,lambda *_:exit(0)); print('ready',flush=True); time.sleep(30)"
p=subprocess.Popen([sys.executable,'-c',resistant if sys.argv[2]=='resistant' else ordinary],stdout=subprocess.PIPE,text=True,start_new_session=True)
try:
    ready=p.stdout.readline()
    if sys.argv[2]=='resistant':p.wait(timeout=5)
    outcome=b.cleanup_groups([p]);outcome.update(status='failed' if outcome['errors'] else 'stopped',scenario_error=None)
    with tempfile.TemporaryDirectory(prefix='lane-remote-cleanup-') as root:
        path=pathlib.Path(root)/'cleanup.json';path.write_text(json.dumps(outcome));path.chmod(0o600)
        print(json.dumps({'cleanup':b.verify_cleanup(root),'parent_exit':p.returncode,'remaining':b.group_members(p.pid)}))
finally:
    try:os.killpg(p.pid,signal.SIGKILL)
    except ProcessLookupError:pass
    p.wait(timeout=5);p.stdout.close()
`;
function actualRemoteProbe(mode){
 return JSON.parse(execFileSync(process.env.LANE_TEST_PYTHON??'python3',['-c',remoteProbe,fileURLToPath(new URL('./browser-host.py',import.meta.url)),mode],{encoding:'utf8',timeout:15000,env:{...process.env,PYTHONDONTWRITEBYTECODE:'1'}}));
}
test('actual remote zero-exit parent rescue fails JS admission and receipt, then continues cleanup',async()=>{
 const probe=actualRemoteProbe('resistant');assert.equal(probe.parent_exit,0);assert.deepEqual(probe.remaining,[]);
 const remote=probe.cleanup,pid=remote.groups[0].pid;let later=false;
 const cleanup=await cleanupAll([['remote',async()=>validateRemoteCleanup(remote,[pid])],['later-resource',async()=>{later=true}]]);
 assert.equal(cleanup.status,'failed');assert.equal(later,true);assert.deepEqual(cleanup.actions[0].evidence,remote);
 assert.deepEqual(remote.groups[0].escalations,['SIGKILL']);assert.deepEqual(remote.groups[0].signals.map(s=>s.signal),['SIGTERM','SIGKILL']);
 assert.equal(remote.status,'failed');assert.equal(remote.verified,false);
 const writes=[];assert.equal(await commitOutcome({receipt:{cases:[]},scenario_error:{code:'original_failure'},cleanup},async(name,value)=>writes.push({name,value})),1);
 assert.ok(!writes.some(x=>x.name==='receipt'));assert.equal(writes.find(x=>x.name==='failure').value.scenario_error.code,'original_failure');
 // Admission must inspect the actual escalation facts even if summary flags are wrong.
 const contradictory=structuredClone(remote);contradictory.status='stopped';contradictory.verified=true;contradictory.errors=[];contradictory.groups[0].status='passed';
 assert.throws(()=>validateRemoteCleanup(contradictory,[pid]));
});
test('actual ordinary remote termination explicitly records no escalation and admits',async()=>{
 const {cleanup:remote,remaining}=actualRemoteProbe('ordinary');const pid=remote.groups[0].pid;
 assert.deepEqual(remaining,[]);assert.deepEqual(remote.groups[0].escalations,[]);assert.deepEqual(remote.groups[0].signals,[{signal:'SIGTERM',outcome:'sent'}]);
 assert.equal(validateRemoteCleanup(remote,[pid]),remote);
 const cleanup=await cleanupAll([['remote',async()=>validateRemoteCleanup(remote,[pid])]]);const writes=[];assert.equal(await commitOutcome({receipt:{cases:[]},scenario_error:null,cleanup},async name=>writes.push(name)),0);assert.ok(writes.includes('receipt'));
 for(const field of ['escalations','signals']){const missing=structuredClone(remote);delete missing.groups[0][field];assert.throws(()=>validateRemoteCleanup(missing,[pid]));}
 const inconsistent=structuredClone(remote);inconsistent.groups[0].signals.push({signal:'SIGKILL',outcome:'sent'});assert.throws(()=>validateRemoteCleanup(inconsistent,[pid]));
});
