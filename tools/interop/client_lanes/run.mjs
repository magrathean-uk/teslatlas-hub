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
const here=path.dirname(fileURLToPath(import.meta.url));const workspace=path.resolve(here,'../../../..');
const sha=b=>createHash('sha256').update(b).digest('hex');
async function privateBoundary(value){
 if(typeof value!=='string'||!path.isAbsolute(value))throw Error('absolute private path required');
 const parent=await realpath(path.dirname(value));const relative=path.relative(workspace,parent);
 if(relative===''||(!relative.startsWith('..'+path.sep)&&relative!=='..'&&!path.isAbsolute(relative)))throw Error('private output or descriptor cannot reside in source');
 const metadata=await stat(parent);if(metadata.uid!==process.getuid()||(metadata.mode&0o077))throw Error('owner-only private parent required');
}
await privateBoundary(process.argv[2]);
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
if(packed.witness.packageVersion!=='2026.36.2'||packed.witness.installedMemberCount!==80)throw Error('unexpected packed cohort');
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
 const identityExpected={tarball_sha256:config.tarball_sha256,hub_sha256:config.hub_sha256,package_version:'2026.36.2',installed_members:80};
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
