import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,writeFile,chmod,realpath,readFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import {createHash} from 'node:crypto';
import {spawn} from 'node:child_process';
import {readInstalledLaneConfig,InstalledBroker} from './installed_contract.mjs';
import {buildInstalledEvidence} from './typescript_lane.mjs';

const sha=raw=>createHash('sha256').update(raw).digest('hex');
const DIGEST='a'.repeat(64);
const SESSION_ID='11111111-1111-4111-8111-111111111111';
const uuid='22222222-2222-4222-8222-222222222222';
const staged=(root,name)=>({id:`${name}-id`,root:{path:path.join(root,`${name}-root`),sha256:DIGEST},local:{path:path.join(root,`${name}-local`),sha256:DIGEST}});
const processIdentity={pid:41,uid:501,parent_pid:1,boot_id:'boot',start_identity:'start',executable_path:'/usr/bin/teslatlas-hub',executable_sha256:DIGEST,argv_sha256:DIGEST};
const proof=(challenge='a'.repeat(64))=>({schema_version:1,status:'verified',session_id:SESSION_ID,sequence:1,challenge,observer:{bundle_sha256:DIGEST,process:processIdentity},host:{host_id:'host',machine_identity_sha256:DIGEST,os:'Debian 13',os_version:'13',architecture:'arm64',kernel:'kernel',native_or_emulated:'native',hypervisor_evidence_sha256:DIGEST},package:{sha256:DIGEST,manifest_sha256:DIGEST,installed_version:'2026.36.2',payload_manifest_sha256:DIGEST,installation_receipt_sha256:DIGEST},service:{mode:'installed-deb-systemd',target:'teslatlas-hub.service',definition_sha256:DIGEST,state:'running',generation:'boot:start',post_probe_generation:'boot:start',supervisor:processIdentity,hub:processIdentity,process_tree_sha256:DIGEST,invocation_id:'b'.repeat(32),control_group:'/system.slice/teslatlas-hub.service',fragment_sha256:DIGEST,drop_in_manifest_sha256:DIGEST,result:'success',exec_main_code:'0',exec_main_status:0},config:{path:'/etc/teslatlas-hub/config.toml',sha256:DIGEST,data_dir:'/var/lib/teslatlas-hub/interop-matrix/run/hub',store_id:'store',store_schema_version:59,scenario_sha256:DIGEST,seed_sha256:DIGEST},listener:{host:'127.0.0.1',port:18480,owner_pid:41,socket_identity:'listener',observed_at_monotonic_ns:1},tls:{endpoint:'https://127.0.0.1:18480',certificate_der_sha256:DIGEST,verified_chain:true,verified_hostname:true,redirect_count:0},discovery:{hub_id:'hub',product_version:'2026.36.2',response_sha256:DIGEST,profile_id:'hub-http-v1@1.0.0',profile_sha256:DIGEST,validated_response_set_sha256:DIGEST}});
const invitation=(pairingId,expiresAtMs)=>({pairingId,secret:'secret',expiresAtMs,endpoint:'https://127.0.0.1:18480',tlsPin:DIGEST,pairingUri:`teslatlas-hub://pair?endpoint=https%3A%2F%2F127.0.0.1%3A18480&pairing_id=${pairingId}&secret=secret&tls_pin=${DIGEST}`});
const runningResult=(challenge='a'.repeat(64))=>({descriptor:{status:'ready',provenance:'installed-package-service',endpoint:'https://127.0.0.1:18480',hub_id:'hub',hub_pid:41,hub_started_at:'start',service_generation:'boot:start',binary_sha256:DIGEST,seed_binary_sha256:DIGEST,profile_id:'hub-http-v1@1.0.0',profile_path:'/private/profile.json',profile_sha256:DIGEST,scenario_path:'/private/scenario.json',scenario_sha256:DIGEST,certificate_path:'/private/certificate.der'},proof:proof(challenge),invitation:invitation(uuid,4102444800000),expired_invitation:invitation('33333333-3333-4333-8333-333333333333',1),events:[]});
const sessionFor=(root,socketPath)=>({schema_version:1,kind:'matrix-adapter-session',run_id:'run',cell_id:'typescript_node__macos_arm64',adapter_id:'typescript_node',client_id:'typescript_node',session_id:SESSION_ID,instance_nonce:'c'.repeat(64),header:staged(root,'header'),case_contract:staged(root,'contract'),host_session:{schema_version:1,kind:'installed-host',broker_socket:socketPath,session_id:SESSION_ID,registration_sha256:DIGEST},broker:{kind:'unix',socket_path:socketPath},inputs:{profile_manifest:staged(root,'profile'),profile_members:Array.from({length:18},(_,index)=>staged(root,`member-${index}`)),scenario:staged(root,'scenario'),certificate:staged(root,'certificate'),certificate_der_sha256:DIGEST,product_inputs:[]},actors:[{id:'node-actor',kind:'packed_sdk_node',execution:'local_worker',runtime_ref:'node',artifact_roles:['sdk'],source_roles:['sdk-source'],entrypoint_ref:'run',input_manifest:staged(root,'actor'),phase_contract:null}],outputs:{normalized:path.join(root,'normalized.json'),actor_evidence:path.join(root,'actors.json'),coordination_dir:path.join(root,'coordination'),framework_log:path.join(root,'framework.log')},bounds:{cell_timeout_ms:60000,cleanup_timeout_ms:45000,frame_bytes:1048576,evidence_bytes:8388608,framework_log_bytes:8388608}});

const listen=(socketPath,resultFactory)=>new Promise(resolve=>{const server=net.createServer(socket=>{let challenge='a'.repeat(64),buffer='';socket.write(JSON.stringify({schema_version:1,type:'challenge',session_id:SESSION_ID,sequence:0,challenge})+'\n');socket.on('data',chunk=>{buffer+=chunk;const i=buffer.indexOf('\n');if(i<0)return;const request=JSON.parse(buffer.slice(0,i));challenge='b'.repeat(64);socket.end(JSON.stringify({schema_version:1,type:'reply',session_id:request.session_id,sequence:request.sequence,challenge,result:resultFactory()})+'\n')})});server.listen(socketPath,()=>resolve(server));});

const writeConfig=async(root,session)=>{const sessionRaw=Buffer.from(JSON.stringify(session)+'\n');const sessionPath=path.join(root,'session.json');await writeFile(sessionPath,sessionRaw,{mode:0o600});const config={schema_version:2,kind:'installed-client-lane',mode:'node',session_input:{path:sessionPath,sha256:sha(sessionRaw)}};const configPath=path.join(root,'config.json');await writeFile(configPath,JSON.stringify(config)+'\n',{mode:0o600});return {config,configPath,sessionPath,sessionRaw}};

test('installed config and broker deeply bind the session and proof contract',async()=>{const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-lane-')));await chmod(root,0o700);const socketPath=path.join(root,'broker.sock');const server=await listen(socketPath,runningResult);await chmod(socketPath,0o600);const session=sessionFor(root,socketPath);const written=await writeConfig(root,session);const loaded=await readInstalledLaneConfig(written.configPath,'node');const broker=await new InstalledBroker(loaded.session).open();assert.deepEqual(await broker.request({op:'verify'}),runningResult());broker.close();server.close();
 session.actors[0].runtime_ref='';const tampered=await writeConfig(root,session);await assert.rejects(readInstalledLaneConfig(tampered.configPath,'node'),/session\.actors/);
 written.config.command='/bin/sh';await writeFile(written.configPath,JSON.stringify(written.config)+'\n',{mode:0o600});await assert.rejects(readInstalledLaneConfig(written.configPath,'node'),/fields invalid/);
});

test('run.mjs executes the strict installed entrypoint and closes after runner acknowledgement',async()=>{
 const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-entrypoint-')));await chmod(root,0o700);
 const socketPath=path.join(root,'broker.sock');const server=await listen(socketPath,runningResult);await chmod(socketPath,0o600);
 let child;
 try{
  const written=await writeConfig(root,sessionFor(root,socketPath));
  child=spawn(process.execPath,[path.resolve('tools/interop/client_lanes/run.mjs'),written.configPath],{stdio:['ignore','pipe','pipe']});
  let output='';child.stdout.on('data',chunk=>{output+=String(chunk)});child.stderr.on('data',chunk=>{output+=String(chunk)});
  const readyPath=path.join(root,'coordination','ready-000001.json');let ready=false;
  for(let index=0;index<200&&!ready;index++){try{await readFile(readyPath);ready=true}catch{await new Promise(resolve=>setTimeout(resolve,10))}}
  assert.equal(ready,true,output);
  const readyRaw=await readFile(readyPath);const resultPath=path.join(root,'close-evidence.json');const resultRaw=Buffer.from(JSON.stringify({schema_version:1,session_id:SESSION_ID,state:'closed',journal:{path:'/private/journal.json',sha256:DIGEST},final_stopped:{},cleanup_errors:[],local_transport:{}})+'\n');await writeFile(resultPath,resultRaw,{mode:0o600});
  await writeFile(path.join(root,'coordination','ack-000001.json'),JSON.stringify({schema_version:1,type:'ack',session_id:SESSION_ID,cell_id:'typescript_node__macos_arm64',session_input_sha256:sha(written.sessionRaw),instance_nonce:'c'.repeat(64),sequence:1,ready_sha256:sha(readyRaw),phase:'evidence_ready',status:'accepted',action:'close_completed',result:{path:resultPath,sha256:sha(resultRaw)}})+'\n',{mode:0o600});
  const exitCode=await new Promise(resolve=>{if(child.exitCode!==null)resolve(child.exitCode);else child.once('exit',code=>resolve(code))});assert.equal(exitCode,0,output);
 }finally{if(child&&child.exitCode===null)child.kill('SIGTERM');server.close()}
});

test('TypeScript evidence builder binds every worker case to raw evidence',()=>{
 const built=buildInstalledEvidence({
  session:{session_id:SESSION_ID,cell_id:'typescript_node__macos_arm64'},
  sessionInputSha256:DIGEST,
  actor:{id:'sdk_node',kind:'packed_sdk_node',runtime_ref:'root_node',entrypoint_ref:'sdk_node_worker',artifact_roles:['typescript_sdk_tarball'],source_roles:['typescript_sdk_source'],installed_manifest:{path:'/private/manifest.json',sha256:DIGEST}},
  cases:[{id:'discovery_identity_profile',expected:{hub_id:'hub'},actual:{hub_id:'hub'},evidence_kind:'http',request_transcript:[{method:'GET',route:'/.well-known/teslatlas-hub',status:200,request_id:'req-1',scope:'/.well-known/teslatlas-hub'}],lane:{operation:'discovery',session_sequence_before:1,session_sequence_after:2,credential_device_id:null}}],
 });
 assert.equal(built.normalized.operations.length,1);
 assert.equal(built.actorEvidence.invocations.length,1);
 assert.equal(built.actorEvidence.actors[0].raw_evidence.length,1);
 assert.deepEqual(built.rawDocuments[0].document.requests,[{method:'GET',route:'/.well-known/teslatlas-hub',status:200,request_id:'req-1',scope:'/.well-known/teslatlas-hub'}]);
});

test('run.mjs rejects a minimal acknowledgement instead of admitting it',async()=>{
 const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-minimal-')));await chmod(root,0o700);
 const socketPath=path.join(root,'broker.sock');const server=await listen(socketPath,runningResult);await chmod(socketPath,0o600);
 let child;
 try{
  const written=await writeConfig(root,sessionFor(root,socketPath));
  child=spawn(process.execPath,[path.resolve('tools/interop/client_lanes/run.mjs'),written.configPath],{stdio:['ignore','pipe','pipe']});
  let output='';child.stdout.on('data',chunk=>{output+=String(chunk)});child.stderr.on('data',chunk=>{output+=String(chunk)});
  const readyPath=path.join(root,'coordination','ready-000001.json');let ready=false;
  for(let index=0;index<200&&!ready;index++){try{await readFile(readyPath);ready=true}catch{await new Promise(resolve=>setTimeout(resolve,10))}}
  assert.equal(ready,true,output);
  await writeFile(path.join(root,'coordination','ack-000001.json'),JSON.stringify({status:'accepted',action:'close_completed'})+'\n',{mode:0o600});
  const exitCode=await new Promise(resolve=>{if(child.exitCode!==null)resolve(child.exitCode);else child.once('exit',code=>resolve(code))});assert.notEqual(exitCode,0,output);
 }finally{if(child&&child.exitCode===null)child.kill('SIGTERM');server.close()}
});

test('run.mjs bounds a missing acknowledgement by the cell deadline',async()=>{
 const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-timeout-')));await chmod(root,0o700);
 const socketPath=path.join(root,'broker.sock');const server=await listen(socketPath,runningResult);await chmod(socketPath,0o600);
 let child;
 try{
  const session=sessionFor(root,socketPath);session.bounds.cell_timeout_ms=250;const written=await writeConfig(root,session);
  child=spawn(process.execPath,[path.resolve('tools/interop/client_lanes/run.mjs'),written.configPath],{stdio:['ignore','pipe','pipe']});
  let output='';child.stdout.on('data',chunk=>{output+=String(chunk)});child.stderr.on('data',chunk=>{output+=String(chunk)});
  const readyPath=path.join(root,'coordination','ready-000001.json');let ready=false;
  for(let index=0;index<200&&!ready;index++){try{await readFile(readyPath);ready=true}catch{await new Promise(resolve=>setTimeout(resolve,10))}}
  assert.equal(ready,true,output);
  const outcome=await new Promise(resolve=>{const timer=setTimeout(()=>resolve({timeout:true}),2000);if(child.exitCode!==null){clearTimeout(timer);resolve({timeout:false,code:child.exitCode})}else child.once('exit',code=>{clearTimeout(timer);resolve({timeout:false,code})})});
  assert.equal(outcome.timeout,false,output);assert.notEqual(outcome.code,0,output);
 }finally{if(child&&child.exitCode===null)child.kill('SIGTERM');server.close()}
});

test('nested proof tampering is rejected and permanently poisons the broker',async()=>{const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-proof-')));await chmod(root,0o700);const socketPath=path.join(root,'broker.sock');const invalid=runningResult();invalid.proof.tls.endpoint='http://127.0.0.1:18480';const server=await listen(socketPath,()=>invalid);await chmod(socketPath,0o600);const broker=await new InstalledBroker({session_id:SESSION_ID,broker:{socket_path:socketPath}}).open();await assert.rejects(broker.request({op:'verify'}),/broker proof\.tls identity invalid/);await assert.rejects(broker.request({op:'verify'}),/permanently failed/);broker.close();server.close();});

test('broker rejects a proof whose sequence is not the current exchange',async()=>{const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-sequence-')));await chmod(root,0o700);const socketPath=path.join(root,'broker.sock');const invalid=runningResult();invalid.proof.sequence=2;const server=await listen(socketPath,()=>invalid);await chmod(socketPath,0o600);const broker=await new InstalledBroker({session_id:SESSION_ID,broker:{socket_path:socketPath}}).open();await assert.rejects(broker.request({op:'verify'}),/broker proof sequence binding invalid/);await assert.rejects(broker.request({op:'verify'}),/permanently failed/);broker.close();server.close();});

test('broker rejects a proof whose challenge is not the current exchange',async()=>{const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-challenge-')));await chmod(root,0o700);const socketPath=path.join(root,'broker.sock');const invalid=runningResult('d'.repeat(64));const server=await listen(socketPath,()=>invalid);await chmod(socketPath,0o600);const broker=await new InstalledBroker({session_id:SESSION_ID,broker:{socket_path:socketPath}}).open();await assert.rejects(broker.request({op:'verify'}),/broker proof challenge binding invalid/);await assert.rejects(broker.request({op:'verify'}),/permanently failed/);broker.close();server.close();});

test('strict installed wire rejects duplicate or invalid UTF-8 config and poisons malformed results',async()=>{const root=await realpath(await mkdtemp(path.join(os.tmpdir(),'installed-wire-')));await chmod(root,0o700);const duplicate=path.join(root,'duplicate.json');await writeFile(duplicate,Buffer.from('{"schema_version":2,"schema_version":2}'),{mode:0o600});await assert.rejects(readInstalledLaneConfig(duplicate,'node'),/invalid JSON/);const invalid=path.join(root,'invalid.json');await writeFile(invalid,Buffer.from([0xc3,0x28]),{mode:0o600});await assert.rejects(readInstalledLaneConfig(invalid,'node'),/invalid JSON/);
 const socketPath=path.join(root,'broker.sock');const server=await listen(socketPath,()=>({verified:true}));await chmod(socketPath,0o600);const broker=await new InstalledBroker({session_id:SESSION_ID,broker:{socket_path:socketPath}}).open();await assert.rejects(broker.request({op:'verify'}),/broker running result fields invalid/);await assert.rejects(broker.request({op:'verify'}),/permanently failed/);broker.close();server.close();});
