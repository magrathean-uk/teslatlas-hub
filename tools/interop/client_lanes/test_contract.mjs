import test from 'node:test';
import assert from 'node:assert/strict';
import {readPrivate, makeCase} from './contract.mjs';
import {mkdtemp,writeFile,symlink,chmod} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
test('private descriptors reject public files and symlinks',async()=>{const d=await mkdtemp(path.join(os.tmpdir(),'lane-unit-'));const p=path.join(d,'a');await writeFile(p,'{}',{mode:0o644});await chmod(p,0o644);await assert.rejects(readPrivate(p));await symlink(p,path.join(d,'link'));await assert.rejects(readPrivate(path.join(d,'link')));});
test('independent mismatches and absent transport cannot pass',()=>{assert.equal(makeCase('x',{a:1},{a:2},'http',[{method:'GET',route:'/healthz',status:200,request_id:'r1'}]).status,'failed');assert.equal(makeCase('x',{}, {},'http',[]).status,'failed');});
test('typed preflight zero cannot hide observed requests',()=>{assert.equal(makeCase('x',{outgoing_requests:0,typed_error:'protocol_validation'},{outgoing_requests:1,typed_error:'protocol_validation'},'zero_request',[]).status,'failed');});
import {headerText} from './contract.mjs';
test('Undici Uint8Array headers decode as bytes, not decimal arrays',()=>{assert.equal(headerText(new TextEncoder().encode('X-Request-ID')),'X-Request-ID');});
import {responseHeader} from './contract.mjs';
test('Undici parsed object and raw array header forms retain IDs',()=>{assert.equal(responseHeader(Object.assign(Object.create(null),{'x-request-id':'actual-id'}),'x-request-id'),'actual-id');assert.equal(responseHeader([new TextEncoder().encode('X-Request-ID'),new TextEncoder().encode('other-id')],'x-request-id'),'other-id');});
test('empty or null equality cannot become assertion success',()=>{for(const value of [null,{},[],true])assert.equal(makeCase('x',value,value,'http',[{method:'GET',route:'/healthz',status:200,request_id:'r'}]).status,'failed')});
import {normalizeClientRuntime,laneExitCode} from './contract.mjs';
import {httpRejection} from './scenarios.mjs';
test('lane mode preserves observed Linux Node ARM/x64 and Chromium runtimes',()=>{
 for(const [arch,expected]of [['arm64','arm64'],['x64','amd64']]){const v=normalizeClientRuntime('node',{os:'linux',architecture:arch,node:'v26.7.0',distribution:'Debian GNU/Linux 13 (trixie)',pid:123});assert.equal(v.architecture,expected);assert.equal(v.service_mode,'owned-node-process');assert.deepEqual(v.tool_versions,{node:'v26.7.0'});}
 assert.equal(normalizeClientRuntime('node',{os:'darwin',architecture:'arm64',node:'v26.7.0'}).os,'macOS');
 assert.equal(normalizeClientRuntime('browser',{os:'linux',architecture:'aarch64',browser:'152.0',distribution:'Debian 13'}).service_mode,'owned-chromium-process');
});
test('actual scenario error capture and normalization reject500 for auth and unknown vehicle',async()=>{
 for(const [id,status]of [['bad_invitation',401],['replayed_invitation',401],['revocation',401],['unknown_vehicle',404]])for(const observed of [status,500]){
  const actual=await httpRejection(async()=>{throw Object.assign(Error('unit HTTP rejection'),{code:'hub_http_error',status:observed})});
  const c=makeCase(id,{typed_error:'hub_http_error',http_status:status},actual,'http',[{method:'POST',route:'/unit',status:observed,request_id:'unit-request'}]);
  assert.equal(c.status,observed===status?'passed':'failed');assert.equal(laneExitCode([c]),observed===status?0:1);
 }
 const stale=await httpRejection(async()=>{throw Object.assign(Error(),{code:'hub_http_error',status:500})});
 const c=makeCase('credential_rotation_api',{old_credential_error:'hub_http_error',old_credential_status:401},{old_credential_error:stale.typed_error,old_credential_status:stale.http_status},'http',[{method:'GET',route:'/v1/vehicles',status:500,request_id:'unit-request'}]);assert.equal(laneExitCode([c]),1);
});
test('HTTP500 transcript cannot corroborate a claimed401 error',()=>{
 const fact={typed_error:'hub_http_error',http_status:401};assert.equal(makeCase('bad_invitation',fact,fact,'http',[{method:'POST',route:'/v1/pairings/unit/claim',status:500,request_id:'unit-id'}]).status,'failed');
});
