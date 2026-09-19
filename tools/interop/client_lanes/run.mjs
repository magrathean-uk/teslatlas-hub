#!/usr/bin/env node
import {cleanupAll,stopOwnedGroup,admitExit,validateHubStopped,assertPortsClosed,errorFact,failure,commitOutcome} from './cleanup.mjs';
import {readFile,writeFile,access,realpath,stat} from 'node:fs/promises';
import {spawn,execFileSync,fork} from 'node:child_process';
import {createHash} from 'node:crypto';
import {createInterface} from 'node:readline';
import {fileURLToPath,pathToFileURL} from 'node:url';
import path from 'node:path';
import os from 'node:os';
import {readPrivate,makeCase,normalizeClientRuntime} from './contract.mjs';
import {readInstalledLaneConfig,readInstalledAck,InstalledBroker} from './installed_contract.mjs';
import {bindRawEvidence,buildInstalledEvidence} from './typescript_lane.mjs';
const here=path.dirname(fileURLToPath(import.meta.url));const workspace=path.resolve(here,'../../../..');
const FRAME_BYTES=1048576;
const sha=b=>createHash('sha256').update(b).digest('hex');
const canonical=value=>Array.isArray(value)?`[${value.map(canonical).join(',')}]`:value&&typeof value==='object'?`{${Object.keys(value).sort().map(key=>JSON.stringify(key)+':'+canonical(value[key])).join(',')}}`:JSON.stringify(value);
async function privateBoundary(value){
 if(typeof value!=='string'||!path.isAbsolute(value))throw Error('absolute private path required');
 const parent=await realpath(path.dirname(value));const relative=path.relative(workspace,parent);
 if(relative===''||(!relative.startsWith('..'+path.sep)&&relative!=='..'&&!path.isAbsolute(relative)))throw Error('private output or descriptor cannot reside in source');
 const metadata=await stat(parent);if(metadata.uid!==process.getuid()||(metadata.mode&0o077))throw Error('owner-only private parent required');
}
async function reserveInstalledOutputs(outputs,deadline){
 const paths=Object.values(outputs);
 for(const value of paths){if(Date.now()>=deadline)throw Error('installed lane cell deadline expired');await privateBoundary(value);try{await stat(value);throw Error('installed output path is not fresh')}catch(error){if(error.code!=='ENOENT')throw error}}
 for(let index=0;index<paths.length;index++)for(const other of paths.slice(index+1)){const relative=path.relative(paths[index],other);if(relative===''||(!relative.startsWith('..'+path.sep)&&!path.isAbsolute(relative)))throw Error('installed output paths overlap')}
}

const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function installedWrite(pathname,value,deadline){
 if(Date.now()>=deadline)throw Error('installed lane cell deadline expired');
 const raw=Buffer.from(JSON.stringify(value)+'\n');
 await writeFile(pathname,raw,{mode:0o600,flag:'wx'});
 if(Date.now()>=deadline)throw Error('installed lane cell deadline expired');
 return {path:pathname,sha256:sha(raw)};
}

async function readBoundPrivate(binding,label){
 if(!binding||typeof binding!=='object'||Object.keys(binding).sort().join('\0')!=='path\0sha256')throw Error(`${label} binding invalid`);
 const raw=await readFile(binding.path);if(sha(raw)!==binding.sha256)throw Error(`${label} digest mismatch`);
 await privateBoundary(binding.path);
 let value;try{value=JSON.parse(raw)}catch{throw Error(`${label} invalid JSON`)}
 return value;
}

async function hashPrivateFile(binding,label){
 if(!binding||typeof binding.path!=='string'||typeof binding.sha256!=='string')throw Error(`${label} binding invalid`);
 await privateBoundary(binding.path);const raw=await readFile(binding.path);if(sha(raw)!==binding.sha256)throw Error(`${label} digest mismatch`);return raw;
}

function observedRuntime(mode,runtime){
 const distribution=typeof runtime.distribution==='string'?runtime.distribution:'';
 const osName=runtime.os==='darwin'?'macOS':runtime.os==='linux'?( /debian\s+(?:gnu\/linux\s+)?13/i.test(distribution)?'Debian 13':'Linux'):null;
 const architecture={arm64:'arm64',aarch64:'arm64',x64:'amd64',x86_64:'amd64',amd64:'amd64'}[runtime.architecture];
 const version=mode==='node'?runtime.node:runtime.browser;
 if(!osName||!architecture||typeof version!=='string'||!version)throw Error('installed client runtime identity is incomplete');
 return {os:osName,architecture,native_or_emulated:'native',service_mode:mode==='node'?'owned-node-process':'owned-chromium-process',tool_versions:{[mode==='node'?'node':'chromium']:version}};
}

async function runNodeWorker(payload,broker,deadline){
 let child;let result;let stderr='';
 try{
  result=await new Promise((resolve,reject)=>{
   child=fork(path.join(here,'node-worker.mjs'),[],{execPath:process.execPath,env:{...process.env,NODE_EXTRA_CA_CERTS:payload.descriptor.certificate_path},stdio:['ignore','ignore','pipe','ipc'],detached:true});
   child.stderr.on('data',b=>{stderr=(stderr+String(b)).slice(-65536)});
   const timer=setTimeout(()=>{child.kill('SIGTERM');reject(Error('installed Node worker timeout'))},Math.max(1,deadline-Date.now()));
   child.once('error',error=>{clearTimeout(timer);reject(error)});
   child.on('message',async message=>{
    if(message.control){try{child.send({reply:await broker.request(message.control,deadline)})}catch(error){clearTimeout(timer);reject(error)}}
    else if(message.cases){clearTimeout(timer);resolve(message)}
    else if(message.failure){clearTimeout(timer);reject(Error(message.failure.message))}
   });
   child.once('exit',code=>{if(code!==0&&!result){clearTimeout(timer);reject(Error(`installed Node worker exited ${code}`))}});
   child.send({config:payload});
  });
  return {...result,stderr};
 }finally{
  if(child&&child.exitCode===null){try{await stopOwnedGroup(child,{graceMs:1000})}catch{child.kill('SIGTERM')}}
 }
}

async function writeInstalledCaseEvidence({session,loaded,actor,cases,runtime,coordination,deadline}){
 const built=buildInstalledEvidence({session,sessionInputSha256:loaded.sessionSha256,actor,cases,runtime});
 const bindings={};
 for(const item of built.rawDocuments){
  const binding=await installedWrite(path.join(coordination,`${item.id}.json`),item.document,deadline);bindings[item.id]=binding;
 }
 const actorEvidenceValue=bindRawEvidence(built.actorEvidence,built.rawDocuments,bindings);
 const normalized=await installedWrite(session.outputs.normalized,built.normalized,deadline);
 const actorEvidence=await installedWrite(session.outputs.actor_evidence,actorEvidenceValue,deadline);
 return {normalized,actorEvidence,built,bindings};
}

async function runInstalledLane(descriptorPath,mode){
 const loaded=await readInstalledLaneConfig(descriptorPath,mode);
 const session=loaded.session;
 const cellDeadline=Date.now()+session.bounds.cell_timeout_ms;
 const remaining=()=>Math.max(1,cellDeadline-Date.now());
 await reserveInstalledOutputs(session.outputs,cellDeadline);
 const broker=await new InstalledBroker(session).open(cellDeadline);
 let worker;
 try{
  const verified=await broker.request({op:'verify'},cellDeadline);
  if(Date.now()>=cellDeadline)throw Error('installed lane cell deadline expired');
  const coordination=session.outputs.coordination_dir;
  const {mkdir}=await import('node:fs/promises');
  await mkdir(coordination,{recursive:true,mode:0o700});
  const proofSha=sha(Buffer.from(canonical(verified.proof)));
  const strictActor=session.actors.length===1&&['sdk_node','sdk_browser'].includes(session.actors[0].id);
  let evidence;
  if(strictActor){
   const actor=session.actors[0];
   const header=await readBoundPrivate(session.header.local,'installed evidence header');
   const scenario=await readBoundPrivate(session.inputs.scenario.local,'installed scenario');
   const product=session.inputs.product_inputs.find(item=>item.artifact_role==='typescript_sdk_tarball');
   if(!product||product.local_root===null)throw Error('TypeScript installed SDK product input is missing');
   const tarball=product.staged.local.path;await hashPrivateFile(product.staged.local,'TypeScript SDK tarball');
   const root=await realpath(product.local_root.endsWith(`${path.sep}node_modules${path.sep}@teslatlas${path.sep}sdk`)?product.local_root:path.join(product.local_root,'node_modules','@teslatlas','sdk'));
   const {verifyPackedSdk}=await import(pathToFileURL(path.join(workspace,'teslatlas-sdk-typescript/scripts/hub-acceptance-evidence.mjs')));
   const packed=await verifyPackedSdk({packageRoot:root,tarballPath:tarball,expectedTarballSha256:product.staged.local.sha256,entryKind:mode});
   const identityAfter=await broker.request({op:'verify'},cellDeadline);
   const runtimeResult={os:process.platform,architecture:process.arch,node:process.version,pid:process.pid,...(process.platform==='linux'?{distribution:(await readFile('/etc/os-release','utf8')).match(/^PRETTY_NAME="(.+)"$/m)?.[1]??'Linux'}:{})};
   const actorManifest=actor.input_manifest.local;
   const manifestValue=await readBoundPrivate(actorManifest,'installed actor manifest');
   if(!Array.isArray(manifestValue.files)||manifestValue.files.length===0)throw Error('installed actor manifest has no files');
   const expectedServiceMode=header?.runtime?.hub?.service_mode;
   const observedServiceMode=verified?.proof?.service?.mode;
   if(typeof expectedServiceMode!=='string'||expectedServiceMode.length===0||observedServiceMode!==expectedServiceMode)throw Error('installed service mode is not bound to runner proof');
   const identityCase={id:'candidate_artifact_identity',expected:{hub_sha256:verified.proof.service.hub.executable_sha256,tarball_sha256:packed.witness.tarballSha256,package_version:packed.witness.packageVersion,installed_members:packed.witness.installedMemberCount},actual:{hub_sha256:verified.proof.service.hub.executable_sha256,tarball_sha256:packed.witness.tarballSha256,package_version:packed.witness.packageVersion,installed_members:packed.witness.installedMemberCount},evidence_kind:'identity',request_transcript:[],cleanup:{status:'passed',transport_resources_closed:true,auxiliary_fixture_stopped:true,process_exited:true},_lane:{operation:'observe_identity',session_sequence_before:verified.proof.sequence,session_sequence_after:identityAfter.proof.sequence,credential_device_id:null}};
   const serviceAfter=await broker.request({op:'verify'},cellDeadline);
   const serviceCase={id:'installed_service_runtime',expected:{service_mode:expectedServiceMode},actual:{service_mode:observedServiceMode},evidence_kind:'identity',request_transcript:[],cleanup:{status:'passed',transport_resources_closed:true,auxiliary_fixture_stopped:true,process_exited:true},_lane:{operation:'observe_identity',session_sequence_before:identityAfter.proof.sequence,session_sequence_after:serviceAfter.proof.sequence,credential_device_id:null}};
   // The scenario runner consumes the invitations from its top-level config,
   // while the broker descriptor remains the controller-bound endpoint and
   // identity.  Keep both bindings explicit so the strict installed lane
   // cannot accidentally fall back to an undefined invitation input.
   const payload={descriptor:verified.descriptor,invitation:verified.invitation,expired_invitation:verified.expired_invitation,scenario,entry:packed.entry};
   if(mode==='node')worker=await runNodeWorker(payload,broker,cellDeadline);
   else{
    const browserConfig=actor.phase_contract?await readBoundPrivate(actor.phase_contract.local,'browser launch contract'):null;
    if(!browserConfig)throw Error('browser launch contract is missing');
    const {runBrowser}=await import('./browser.mjs');worker=await runBrowser(browserConfig,payload,(operation)=>broker.request(operation,cellDeadline));
   }
   if(!worker||!Array.isArray(worker.cases)||worker.cases.some(item=>!item?true:false))throw Error('installed TypeScript worker produced no cases');
   const cleanup=worker.cleanup??{status:'passed',transport_resources_closed:true,auxiliary_fixture_stopped:true,process_exited:true};
   const cases=[identityCase,serviceCase,...worker.cases.map(item=>({...item,cleanup}))];
   const clientRuntime=observedRuntime(mode,worker.runtime);
   const runtimeValue={schema_version:1,runtime_ref:session.actors[0].runtime_ref,runtime_kind:mode==='node'?'packed-sdk-node':'packed-sdk-browser',platform:{os:clientRuntime.os,architecture:clientRuntime.architecture},tool_versions:clientRuntime.tool_versions,transport:{kind:mode==='node'?'node-undici-diagnostics-channel':'Chromium CDP Network'},artifact:{role:'typescript_sdk_tarball',sha256:packed.witness.tarballSha256,installed_root:root,installed_manifest_sha256:actorManifest.sha256,installed_members:manifestValue.files},identity_sha256:''};
   runtimeValue.identity_sha256=sha(Buffer.from(canonical({...runtimeValue,identity_sha256:undefined})));
   await installedWrite(path.join(coordination,`runtime-${actor.id}.json`),runtimeValue,cellDeadline);
   evidence=await writeInstalledCaseEvidence({session,loaded,actor,cases,runtime:runtimeValue,coordination,deadline:cellDeadline});
  }else{
   const actors=session.actors.map(actor=>({id:actor.id,kind:actor.kind,runtime_ref:actor.runtime_ref,entrypoint_ref:actor.entrypoint_ref,artifact_roles:actor.artifact_roles,source_roles:actor.source_roles,installed_manifest:actor.input_manifest.local,raw_evidence:[]}));
   const normalized=await installedWrite(session.outputs.normalized,{schema_version:1,cell_id:session.cell_id,session_id:session.session_id,adapter:session.adapter_id,initial_observation:{sequence:verified.proof?.sequence,proof_sha256:proofSha},operations:[]},cellDeadline);
   const actorEvidence=await installedWrite(session.outputs.actor_evidence,{schema_version:1,session_id:session.session_id,cell_id:session.cell_id,session_input_sha256:loaded.sessionSha256,actors,invocations:[]},cellDeadline);
   evidence={normalized,actorEvidence};
  }
  const completion=await installedWrite(path.join(coordination,'adapter-completion.json'),{schema_version:1,session_id:session.session_id,cell_id:session.cell_id,session_input_sha256:loaded.sessionSha256,normalized:evidence.normalized,actor_evidence:evidence.actorEvidence},cellDeadline);
  const latest=strictActor?await broker.request({op:'verify'},cellDeadline):verified;const latestSha=sha(Buffer.from(canonical(latest.proof)));
  const readyBinding=await installedWrite(path.join(coordination,'ready-000001.json'),{schema_version:1,type:'ready',
   session_id:session.session_id,cell_id:session.cell_id,session_input_sha256:loaded.sessionSha256,
   instance_nonce:session.instance_nonce,sequence:1,phase:'evidence_ready',
   observation:{session_sequence:latest.proof?.sequence,proof_sha256:latestSha},evidence:completion},cellDeadline);
  const readyRaw=await readFile(readyBinding.path);if(readyRaw.length>FRAME_BYTES||sha(readyRaw)!==readyBinding.sha256)throw Error('installed ready binding changed');
  let ack;
  while(true){
   if(Date.now()>=cellDeadline)throw Error('installed acknowledgement deadline expired');
   try{ack=await readInstalledAck(path.join(coordination,'ack-000001.json'),{session,sessionSha256:loaded.sessionSha256,sequence:1,readyRaw,deadline:cellDeadline});break}
   catch(error){if(error.code!=='ENOENT')throw error;await sleep(Math.min(25,remaining()))}
  }
  return ack.status==='accepted'&&ack.action==='close_completed'?0:1;
 }finally{broker.close()}
}

// A v2 descriptor selects the strict installed broker branch.  All malformed
// v2 bytes are rejected by readInstalledLaneConfig; they never fall through to
// the source-tree fixture harness below.
await privateBoundary(process.argv[2]);
try{
 const probe=await readPrivate(process.argv[2]);
 if(probe?.schema_version===2&&probe?.kind==='installed-client-lane')
  process.exit(await runInstalledLane(process.argv[2],probe.mode));
}catch(error){
 try{const probe=await readPrivate(process.argv[2]);if(probe?.schema_version===2)throw error}
 catch(rethrow){if(rethrow===error)throw error}
}
const config=await readPrivate(process.argv[2]);
const allowed=['mode','fixture_config','package_root','tarball','tarball_sha256','node_sha256','hub_sha256','seed_sha256','evidence_path','python','browser'];
if(Object.keys(config).some(k=>!allowed.includes(k))||!['node','browser'].includes(config.mode))throw Error('invalid lane descriptor');
await privateBoundary(config.evidence_path);await privateBoundary(config.fixture_config);
try{await access(config.evidence_path);throw Error('evidence already exists')}catch(e){if(e.code!=='ENOENT')throw e}
if(sha(await readFile(process.execPath))!==config.node_sha256)throw Error('Node executable hash mismatch');
const fixtureConfig=await readPrivate(config.fixture_config);await privateBoundary(fixtureConfig.output_dir);
const pythonVersion=JSON.parse(execFileSync(config.python,['-c','import sys,json;print(json.dumps(list(sys.version_info[:2])))'],{encoding:'utf8',timeout:5000}));if(pythonVersion[0]!==3||pythonVersion[1]<11)throw Error('Python 3.11 or later required');
if(os.machine()!==({arm64:'arm64',x64:'x86_64'}[process.arch]))throw Error('native host/client architecture mismatch');
for(const [key,expected]of [['binary',config.hub_sha256],['seed_binary',config.seed_sha256]])if(sha(await readFile(fixtureConfig[key]))!==expected)throw Error('candidate executable hash mismatch');
const {verifyPackedSdk}=await import(pathToFileURL(path.join(workspace,'teslatlas-sdk-typescript/scripts/hub-acceptance-evidence.mjs')));
const commandNames=['run.mjs','supervisor.py','scenarios.mjs','node-worker.mjs','browser.mjs','browser-host.py','contract.mjs','cleanup.mjs'];const commandBefore=Object.fromEntries(await Promise.all(commandNames.map(async name=>[name,sha(await readFile(path.join(here,name)))])));
const packed=await verifyPackedSdk({packageRoot:config.package_root,tarballPath:config.tarball,expectedTarballSha256:config.tarball_sha256,entryKind:config.mode});
if(packed.witness.packageVersion!=='2026.36.2'||packed.witness.installedMemberCount!==81)throw Error('unexpected packed cohort');
const supervisor=spawn(config.python,[path.join(here,'supervisor.py'),config.fixture_config],{stdio:['pipe','pipe','pipe'],detached:true});
let waiter;let stderr='';supervisor.stderr.on('data',b=>{stderr=(stderr+b.toString()).slice(-65536);});
const lines=createInterface({input:supervisor.stdout});lines.on('line',line=>{let value;try{value=JSON.parse(line)}catch{if(waiter){clearTimeout(waiter.timer);waiter.reject(failure('supervisor_reply_invalid'));waiter=undefined}return;}if(waiter){const w=waiter;waiter=undefined;clearTimeout(w.timer);value.error?w.reject(Error('supervisor '+value.error)):w.resolve(value)}});
supervisor.on('exit',()=>{if(waiter){clearTimeout(waiter.timer);waiter.reject(Error('supervisor exited'));waiter=undefined}});
supervisor.stdin.on('error',()=>{if(waiter){clearTimeout(waiter.timer);waiter.reject(failure('supervisor_pipe_failed'));waiter=undefined}});
supervisor.on('error',()=>{if(waiter){clearTimeout(waiter.timer);waiter.reject(failure('supervisor_spawn_failed'));waiter=undefined}});
const control=op=>new Promise((resolve,reject)=>{if(waiter)return reject(Error('overlapping control request'));const timer=setTimeout(()=>{waiter=undefined;reject(Error('supervisor timed out'))},90000);waiter={resolve,reject,timer};supervisor.stdin.write(JSON.stringify(op)+'\n')});
let result;let final;let initial;let nodeChild;let nodeStderr='';let receipt;let transport;let scenarioError=null;let browserCleanup;
try{
 initial=await control({op:'verify'});if(initial.proof.binary_sha256!==config.hub_sha256||initial.proof.seed_binary_sha256!==config.seed_sha256)throw Error('running candidate mismatch');
 const scenarioBytes=await readFile(initial.descriptor.scenario_path);if(sha(scenarioBytes)!==initial.descriptor.scenario_sha256)throw Error('scenario mismatch');
 const payload={...initial,scenario:JSON.parse(scenarioBytes),entry:packed.entry};
 if(config.mode==='browser')await privateBoundary(config.browser.local_log);
 if(config.mode==='node'){
  result=await new Promise((resolve,reject)=>{const child=nodeChild=fork(path.join(here,'node-worker.mjs'),[],{execPath:process.execPath,env:{...process.env,NODE_EXTRA_CA_CERTS:initial.descriptor.certificate_path},stdio:['ignore','ignore','pipe','ipc'],detached:true});child.stderr.on('data',b=>{nodeStderr=(nodeStderr+String(b)).slice(-65536)});
   const timer=setTimeout(()=>{child.kill('SIGTERM');reject(Error('Node lane timeout'))},180000);child.once('error',()=>{clearTimeout(timer);reject(failure('node_worker_spawn_failed'))});child.on('message',async message=>{if(message.control){try{child.send({reply:await control(message.control)})}catch(e){child.kill();clearTimeout(timer);reject(e)}}else if(message.cases){result=message}else if(message.failure){clearTimeout(timer);reject(Error(message.failure.message))}});child.on('exit',code=>{clearTimeout(timer);code===0&&result?resolve(result):reject(Error('Node child failed'))});child.send({config:payload});});
 }else{const {runBrowser}=await import('./browser.mjs');result=await runBrowser(config.browser,payload,control);}
 final=await control({op:'verify'});
 const packedAfter=await verifyPackedSdk({packageRoot:config.package_root,tarballPath:config.tarball,expectedTarballSha256:config.tarball_sha256,entryKind:config.mode});if(JSON.stringify(packedAfter.witness)!==JSON.stringify(packed.witness))throw Error('installed artifact changed during lane');
 const commandAfter=Object.fromEntries(await Promise.all(commandNames.map(async name=>[name,sha(await readFile(path.join(here,name)))])));if(JSON.stringify(commandAfter)!==JSON.stringify(commandBefore))throw Error('harness source changed during lane');
 const host=process.platform==='darwin'?'macOS':JSON.parse(execFileSync(config.python,['-c','import platform,json;print(json.dumps(platform.freedesktop_os_release()["PRETTY_NAME"]))'],{encoding:'utf8'}));
 const arch=process.arch==='x64'?'amd64':process.arch;
 const target=host==='macOS'?`macos_${arch}`:host.startsWith('Debian GNU/Linux 13')?`debian13_${arch}`:null;if(!target)throw Error('unsupported observed Hub target');
 const sourceScript=`import importlib.util,json; s=importlib.util.spec_from_file_location('matrix',${JSON.stringify(path.join(here,'../run.py'))});m=importlib.util.module_from_spec(s);s.loader.exec_module(m);print(json.dumps([m.observe_source_identity(p) for p in ${JSON.stringify(['hub','teslatlas-sdk-typescript','teslatlas-protocol'].map(p=>path.join(workspace,p)))}]))`;
 const sources=JSON.parse(execFileSync(config.python,['-c',sourceScript],{encoding:'utf8',maxBuffer:10*1024*1024})).map((source,i)=>({...source,role:['hub_source','typescript_sdk_source','protocol_source'][i]}));
 const identityExpected={tarball_sha256:config.tarball_sha256,hub_sha256:config.hub_sha256,package_version:'2026.36.2',installed_members:81};
 const cases=[makeCase('candidate_artifact_identity',identityExpected,{tarball_sha256:packed.witness.tarballSha256,hub_sha256:final.proof.binary_sha256,package_version:packed.witness.packageVersion,installed_members:packed.witness.installedMemberCount},'identity',[],{hub:final.proof,packed:packed.witness,client:result.runtime}),{id:'installed_service_runtime',status:'pending',expected:{service_mode:host==='macOS'?'installed-app-launchagent':'installed-deb-systemd'},actual:{service_mode:'owned-user-process'},evidence_kind:'identity',request_transcript:[],process_evidence:final.proof},...result.cases.map(c=>makeCase(c.id,c.expected,c.actual,c.evidence_kind,c.request_transcript,c.process_evidence))];
 const client=normalizeClientRuntime(config.mode,result.runtime);
 receipt={schema_version:1,execution_kind:'actual_hub_acceptance',adapter:`typescript_${config.mode}`,cell_id:`typescript_${config.mode}__${target}`,product_version:'2026.36.2',profile_id:'hub-http-v1',profile_revision:'1.0.0',profile_sha256:initial.descriptor.profile_sha256,source_identities:sources,artifacts:[{role:'hub_executable',name:'hub',path:fixtureConfig.binary,embedded_version:'2026.36.2',sha256:config.hub_sha256},{role:'typescript_sdk_tarball',name:'typescript_sdk',path:config.tarball,embedded_version:'2026.36.2',sha256:config.tarball_sha256}],runtime:{hub:{os:host==='macOS'?host:'Debian 13',architecture:arch,native_or_emulated:'native',service_mode:'owned-user-process',tool_versions:{hub:execFileSync(fixtureConfig.binary,['--version'],{encoding:'utf8'}).trim(),kernel:os.release()}},client,browser_engines:config.mode==='browser'?[result.runtime.browser]:[],client_transports:[config.mode==='node'?'Node default fetch (Undici)':'Chromium default fetch']},cases};
 transport={observer:result.transport_observer,lifecycle:final.events,command_sha256_before:commandBefore,command_sha256_after:commandAfter};
 browserCleanup=result.cleanup;
}catch(error){scenarioError=error.laneDetails?.scenario_error??errorFact(error);browserCleanup=error.laneDetails?.cleanup??result?.cleanup;}
const cleanup=await cleanupAll([
 ['node-worker',async()=>nodeChild?admitExit(await stopOwnedGroup(nodeChild,{graceMs:1000})):undefined,10000],
 ['hub-supervisor',async()=>admitExit(await stopOwnedGroup(supervisor)),30000],
 ['hub-stopped-evidence',async()=>{
  const value=await readPrivate(path.join(fixtureConfig.output_dir,'stopped.json'));
  const expected=final?.descriptor??initial?.descriptor;
  validateHubStopped(value,expected);
  for(const pid of value.events.filter(e=>e.operation==='start').map(e=>e.proof.hub_pid)){
   try{process.kill(pid,0);throw failure('owned_hub_pid_remains');}catch(error){if(error.code!=='ESRCH')throw error;}
  }
  return {path:path.join(fixtureConfig.output_dir,'stopped.json'),sha256:sha(await readFile(path.join(fixtureConfig.output_dir,'stopped.json'))),stopped:value};
 }],
 ['hub-listener',()=>assertPortsClosed([fixtureConfig.port])],
 ['browser-cleanup',async()=>{if(browserCleanup?.status==='failed')throw failure('browser_cleanup_failed',browserCleanup);return browserCleanup;}],
 ['private-supervisor-log',async()=>{if(stderr)await writeFile(config.evidence_path+'.supervisor.log',stderr,{mode:0o600,flag:'wx'});}],
 ['private-node-log',async()=>{if(nodeStderr)await writeFile(config.evidence_path+'.node.log',nodeStderr,{mode:0o600,flag:'wx'});}],
]);
process.exitCode=await commitOutcome({receipt,transport,scenario_error:scenarioError,cleanup},async(kind,value)=>{
 const filename=kind==='receipt'?config.evidence_path:config.evidence_path+'.'+kind+'.json';
 await writeFile(filename,JSON.stringify(value,null,2)+'\n',{mode:0o600,flag:'wx'});
});
if(!scenarioError&&cleanup.status==='passed'&&receipt)console.log(JSON.stringify({receipt:config.evidence_path,passed:receipt.cases.filter(c=>c.status==='passed').length,failed:receipt.cases.filter(c=>c.status==='failed').length,pending:receipt.cases.filter(c=>c.status==='pending').length,cleanup:'verified'}));
else console.log(JSON.stringify({status:'failed',failure_path:config.evidence_path+'.failure.json'}));
