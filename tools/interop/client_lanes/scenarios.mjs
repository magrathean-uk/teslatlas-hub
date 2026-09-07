export async function httpRejection(action){
 try{await action();return {typed_error:'no_error',http_status:null};}
 catch(error){return {typed_error:error.code??error.name,http_status:Number.isInteger(error.status)?error.status:null};}
}
// Shared actual-client operations. Expected vectors come from the independent scenario,
// never seed implementation or recorded responses. No transport is supplied to the SDK.
export async function runScenarios(createHubClient,config,control,observe){
 let credential;const credentials={load:()=>credential,save:v=>{credential=v},clear:()=>{credential=undefined}};
 const client=createHubClient({endpoint:config.descriptor.endpoint,expectedHubId:config.descriptor.hub_id,credentials});
 const cases=[];const scenario=config.scenario;const ids=scenario.vehicle_ids;
 const capture=async(id,expected,action,kind='http')=>{
  const admission=await control({op:'verify'});const begin=await observe('begin');let actual;
  try{actual=await action()}catch(e){actual={unexpected_error:e.code??e.name}}
  const facts=await observe('end',begin);
  if(kind==='zero_request')actual={outgoing_requests:facts.outgoing_requests,...actual};
  cases.push({id,expected,actual,evidence_kind:kind,request_transcript:facts.transcript,...(['credential_lifecycle_reauth','revocation','endpoint_restart','outage_recovery'].includes(id)?{process_evidence:{hub:admission.proof,lifecycle:admission.events}}:{})});return actual;
 };
 const errorCode=async action=>{try{await action();return 'no_error'}catch(e){return e.code??e.name}};
 await capture('discovery_identity_profile',{hub_id:config.descriptor.hub_id,api_versions:['1.0'],protocol:'teslatlas-sync',protocol_major:1,pack_format:'sqlite-zstd',version:'2026.36.2'},async()=>{const v=(await client.discover()).value;return {hub_id:v.hubId,api_versions:v.apiVersions,protocol:v.protocol,protocol_major:v.protocolMajor,pack_format:v.packFormat,version:v.version}});
 await capture('unauthenticated_discovery',{discovery:200,health:200,readiness:200,credential_absent:true},async()=>({discovery:(await client.discover()).metadata.status,health:(await client.health()).metadata.status,readiness:(await client.readiness()).metadata.status,credential_absent:credential===undefined}));
 await capture('expired_invitation',{outgoing_requests:0,typed_error:'protocol_validation'},async()=>({typed_error:await errorCode(()=>client.claimPairing(config.expired_invitation,'Expired'))}),'zero_request');
 const bad=structuredClone(config.invitation);bad.secret=(bad.secret[0]==='a'?'b':'a')+bad.secret.slice(1);const uri=new URL(bad.pairingUri);uri.searchParams.set('secret',bad.secret);bad.pairingUri=uri.href;
 await capture('bad_invitation',{typed_error:'hub_http_error',http_status:401},()=>httpRejection(()=>client.claimPairing(bad,'Bad invitation')));
 await capture('real_auth',{claimed:200,vehicles:scenario.vehicles.map(v=>({vehicle_id:v.vehicle_id,display_name:v.display_name}))},async()=>{const claim=await client.claimPairing(config.invitation,'Actual packed SDK lane');const vehicles=await client.vehicles();return {claimed:claim.metadata.status,vehicles:vehicles.value.vehicles.map(v=>({vehicle_id:v.vehicleId,display_name:v.displayName}))}});
 await capture('replayed_invitation',{typed_error:'hub_http_error',http_status:401},()=>httpRejection(()=>client.claimPairing(config.invitation,'Replay')));
 await capture('unknown_vehicle',{typed_error:'hub_http_error',http_status:404},()=>httpRejection(()=>client.current('33333333-3333-4333-8333-333333333333')));
 await capture('exact_current_values',{...scenario.current,empty_vehicle_observed_at_ms:null},async()=>{const v=(await client.current(ids[0])).value;const empty=(await client.current(ids[1])).value;const result={};for(const key of Object.keys(scenario.current)){const camel=key.replace(/_([a-z])/g,(_,c)=>c.toUpperCase());result[key]=v[camel];}return {...result,empty_vehicle_observed_at_ms:empty.observedAtMs}});
 let pages;
 await capture('drives_three_page_order',{pages:scenario.drive_pages_at_limit_2},async()=>{const a=await client.drives(ids[0],{limit:2});const b=await client.drives(ids[0],{limit:2,cursor:a.value.nextCursor});const c=await client.drives(ids[0],{limit:2,cursor:b.value.nextCursor});pages=[a,b,c];return {pages:pages.map(p=>p.value.items.map(v=>Number(v.id)))}});
 if(pages){
  await capture('drives_terminal_cursor',{next_cursor:null,ids:scenario.drive_pages_at_limit_2[2]},async()=>{const p=await client.drives(ids[0],{limit:2,cursor:pages[1].value.nextCursor});return {next_cursor:p.value.nextCursor,ids:p.value.items.map(v=>Number(v.id))}});
  await capture('drives_etag_304',{kind:'notModified',post_304_ids:scenario.drive_pages_at_limit_2[1]},async()=>{const p=await client.drives(ids[0],{limit:2,ifNoneMatch:pages[0].metadata.etag});const q=await client.drives(ids[0],{limit:2,cursor:pages[0].value.nextCursor});return {kind:p.kind,post_304_ids:q.value.items.map(v=>Number(v.id))}});
  for(const [id,vehicle,options]of [['drives_wrong_vehicle_cursor',ids[1],{}],['drives_wrong_filter_cursor',ids[0],{fromMs:1}]])await capture(id,{outgoing_requests:0,typed_error:'protocol_validation'},async()=>({typed_error:await errorCode(()=>client.drives(vehicle,{limit:2,cursor:pages[0].value.nextCursor,...options}))}),'zero_request');
 }
 // Unsupported current-Hub surface is absent by contract; invoking it must fail
 // locally. Do not fabricate a capability set or alter discovery to force this.
 await capture('unsupported_operation_zero_requests',{outgoing_requests:0},async()=>{const code=await errorCode(()=>client.charges(ids[0]));if(code!=='TypeError')throw Error('unsupported call did not reject');return {}},'zero_request');
 await capture('credential_rotation_api',{rotated:true,same_device:true,vehicles:200,old_credential_error:'hub_http_error',old_credential_status:401},async()=>{const old=credential;await client.rotateDevice();const rotated=credential.accessToken!==old.accessToken;const same=credential.deviceId===old.deviceId;const vehicles=await client.vehicles();const stale=createHubClient({endpoint:config.descriptor.endpoint,expectedHubId:config.descriptor.hub_id,credentials:{load:()=>old,save:()=>{},clear:()=>{}}});try{const rejection=await httpRejection(()=>stale.vehicles());return {rotated,same_device:same,vehicles:vehicles.metadata.status,old_credential_error:rejection.typed_error,old_credential_status:rejection.http_status}}finally{stale.dispose()}});
 await control({op:'revoke',device_id:credential.deviceId});
 await capture('revocation',{typed_error:'hub_http_error',http_status:401},()=>httpRejection(()=>client.vehicles()));
 const rePair=await control({op:'pair'});
 await capture('credential_lifecycle_reauth',{new_device:true,vehicles:200},async()=>{const previous=credential.deviceId;await client.logout();await client.claimPairing(rePair.invitation,'Reauthentication');return {new_device:credential.deviceId!==previous,vehicles:(await client.vehicles()).metadata.status}});
 const before=await control({op:'verify'});await control({op:'stop'});const after=await control({op:'start'});
 await capture('endpoint_restart',{same_hub:true,new_process:true,vehicles:200},async()=>({same_hub:(await client.discover()).value.hubId===config.descriptor.hub_id,new_process:before.descriptor.hub_pid!==after.descriptor.hub_pid,vehicles:(await client.vehicles()).metadata.status}));
 await control({op:'stop'});let outage;try{await client.vehicles({signal:AbortSignal.timeout(3000)});outage=false}catch{outage=true}await control({op:'start'});
 await capture('outage_recovery',{outage_observed:true,vehicles:200},async()=>({outage_observed:outage,vehicles:(await client.vehicles()).metadata.status}));
 client.dispose();return cases;
}
