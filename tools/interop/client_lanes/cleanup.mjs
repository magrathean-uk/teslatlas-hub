import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import net from 'node:net';
import {laneExitCode} from './contract.mjs';
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
export const errorFact=error=>({name:/^[A-Za-z][A-Za-z0-9_]{0,63}$/.test(error?.name)?error.name:'Error',code:/^[A-Za-z0-9_]{1,80}$/.test(error?.code)?error.code:'lane_operation_failed'});
export function failure(code,evidence){return Object.assign(Error(code),{code,evidence});}
export async function bounded(operation,ms=10000){
 let timer;
 try{return await Promise.race([Promise.resolve().then(operation),new Promise((_,reject)=>{timer=setTimeout(()=>reject(failure('cleanup_timeout')),ms)})]);}
 finally{clearTimeout(timer);}
}
export async function cleanupAll(actions){
 const records=[];
 for(const [resource,operation,timeout] of actions){
  try{const evidence=await bounded(operation,timeout??10000);records.push({resource,status:'passed',...(evidence===undefined?{}:{evidence})});}
  catch(error){records.push({resource,status:'failed',error:errorFact(error),...(error.evidence===undefined?{}:{evidence:error.evidence})});}
 }
 return {status:records.every(record=>record.status==='passed')?'passed':'failed',actions:records};
}
function groupMembers(group){
 return execFileSync('ps',['-eo','pid=,pgid=,stat='],{encoding:'utf8',timeout:3000}).trim().split('\n').map(line=>line.trim().split(/\s+/)).filter(p=>p.length===3&&Number(p[1])===group&&!p[2].includes('Z')).map(p=>Number(p[0]));
}
export async function stopOwnedGroup(child,{graceMs=20000,termMs=3000,killMs=3000}={}){
 // All callers create a fresh detached process group. Never infer a group from
 // a caller-provided PID or stop only the direct parent when descendants remain.
 const exited=()=>child.exitCode!==null||child.signalCode!==null;
 const escalations=[];
 if(child.stdin&&!child.stdin.destroyed)child.stdin.end();
 let end=Date.now()+graceMs;
 while(!exited()&&Date.now()<end)await delay(25);
 let members=groupMembers(child.pid);
 for(const [signal,bound]of [['SIGTERM',termMs],['SIGKILL',killMs]]){
  if(!members.length&&exited())break;
  try{process.kill(-child.pid,signal);escalations.push(signal);}catch(error){if(error.code!=='ESRCH')throw error;}
  end=Date.now()+bound;
  do{await delay(25);members=groupMembers(child.pid);}while((members.length||!exited())&&Date.now()<end);
 }
 return {pid:child.pid,exit_code:child.exitCode,signal:child.signalCode,escalations,remaining_live_pids:members};
}
export function admitExit(outcome){
 if(!outcome||outcome.exit_code!==0||outcome.signal!==null||outcome.escalations.length||outcome.remaining_live_pids.length)throw failure('owned_process_shutdown_failed',outcome);
 return outcome;
}
export async function assertPortsClosed(ports){
 const states=[];
 for(const port of ports){
  const closed=await new Promise((resolve,reject)=>{const socket=net.createConnection({host:'127.0.0.1',port});socket.setTimeout(1000);socket.once('connect',()=>{socket.destroy();resolve(false)});socket.once('error',error=>{socket.destroy();error.code==='ECONNREFUSED'?resolve(true):reject(error)});socket.once('timeout',()=>{socket.destroy();reject(failure('listener_probe_timeout'))});});
  states.push({port,closed});
 }
 if(states.some(s=>!s.closed))throw failure('owned_listener_remains',states);
 return states;
}
export function validateHubStopped(value,expected){
 if(!value||value.status!=='stopped'||value.hub_pid!==expected?.hub_pid||value.hub_exit_code!==0||value.listener_closed!==true||value.hub_alive!==false||!Array.isArray(value.cleanup_errors)||value.cleanup_errors.length||!Array.isArray(value.events)||!value.events.some(e=>e.operation==='stop'&&e.proof?.status==='verified'&&e.proof?.hub_pid===expected.hub_pid&&e.exit_code===0&&e.listener_closed===true))throw failure('hub_stopped_evidence_invalid');
 return value;
}
export function validateRemoteCleanup(value,expectedPids){
 if(!value||value.status!=='stopped'||value.verified!==true||!Array.isArray(value.groups)||value.groups.length!==expectedPids.length||value.errors?.length||!Array.isArray(value.ports)||value.ports.some(p=>p.closed!==true))throw failure('browser_stopped_evidence_invalid',value);
 for(const pid of expectedPids){const group=value.groups.find(g=>g.pid===pid);const current=value.current_groups?.find(g=>g.pid===pid);if(!group||group.exit_code!==0||!Array.isArray(group.remaining_live_pids)||group.remaining_live_pids.length||group.status!=='passed'||!Array.isArray(group.escalations)||group.escalations.length||!Array.isArray(group.signals)||group.signals.length!==1||group.signals.some(s=>s.signal!=='SIGTERM'||!['sent','already_exited'].includes(s.outcome))||!current||!Array.isArray(current.live_pids)||current.live_pids.length)throw failure('browser_group_cleanup_failed',value);}
 if(value.ports.length!==4||[18480,18481,18483,18484].some(port=>!value.ports.some(p=>p.port===port&&p.closed===true)))throw failure('browser_forward_closure_missing');
 return value;
}
export async function commitOutcome({receipt,transport,scenario_error,cleanup},write){
 // A normalized successful receipt must never precede cleanup admission.
 await write('cleanup',cleanup);
 if(scenario_error||cleanup.status!=='passed'||!receipt){await write('failure',{status:'failed',scenario_error,cleanup});return 1;}
 const candidate=receipt.cases.find(c=>c.id==='candidate_artifact_identity');
 if(candidate?.process_evidence?.hub)candidate.process_evidence.hub.cleanup_sha256=createHash('sha256').update(JSON.stringify(cleanup,null,2)+'\n').digest('hex');
 await write('transport',transport);
 await write('receipt',receipt);
 return laneExitCode(receipt.cases);
}
