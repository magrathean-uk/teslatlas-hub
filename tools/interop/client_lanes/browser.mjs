import {cleanupAll,stopOwnedGroup,admitExit,validateRemoteCleanup,assertPortsClosed,errorFact,failure} from './cleanup.mjs';
import {spawn,execFileSync} from 'node:child_process';
import {readFile,writeFile} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {createHash} from 'node:crypto';
import {createInterface} from 'node:readline';
import {fileURLToPath,pathToFileURL} from 'node:url';
import path from 'node:path';
import http from 'node:http';
const here=path.dirname(fileURLToPath(import.meta.url));
const digest=pem=>createHash('sha256').update(Buffer.from(pem.match(/-----BEGIN CERTIFICATE-----([\s\S]+?)-----END CERTIFICATE-----/)[1].replace(/\s/g,''),'base64')).digest('hex');
export async function runBrowser(config,payload,control){
 if(!/^\/tmp\/teslatlas-client-lanes-[a-z0-9-]+$/.test(config.remote_root))throw Error('invalid owned remote root');
 const {chromium}=await import(pathToFileURL(config.playwright_entry));
 const sshArgs=['-F',config.ssh_config,'-S','none','-o','ExitOnForwardFailure=yes'];
 // Upload only this explicit harness program, never product source or credentials.
 const remoteScript=config.remote_root+'-host.py';
 execFileSync('ssh',[...sshArgs,config.ssh_alias,`umask 077; test ! -e '${remoteScript}' && cat > '${remoteScript}'`],{input:await readFile(path.join(here,'browser-host.py')),timeout:30000});
 let remote,server,browser,untrusted;let remoteResult;let remoteStderr='';let result;let scenarioError=null;
 try{
  const files={'/sdk.js':payload.entry,'/scenarios.mjs':path.join(here,'scenarios.mjs')};
  server=http.createServer((req,res)=>{if(req.url==='/'){res.setHeader('content-type','text/html');return res.end('<!doctype html><meta charset="utf-8"><title>Owned packed SDK acceptance</title>')}if(files[req.url]){res.setHeader('content-type','text/javascript');return createReadStream(files[req.url]).pipe(res)}res.writeHead(404).end()});
  await new Promise((resolve,reject)=>server.listen(18481,'127.0.0.1',resolve).once('error',reject));
  remote=spawn('ssh',[...sshArgs,'-R',`18480:127.0.0.1:${new URL(payload.descriptor.endpoint).port}`,'-R','18481:127.0.0.1:18481','-L','18483:127.0.0.1:18483','-L','18484:127.0.0.1:18484',config.ssh_alias,`python3 '${remoteScript}'`],{stdio:['pipe','pipe','pipe'],detached:true});
  remote.stdin.on('error',()=>{});
  remote.stderr.on('data',b=>{remoteStderr=(remoteStderr+String(b)).slice(-65536)});
  const certificate=await readFile(payload.descriptor.certificate_path,'utf8');
  remote.stdin.write(JSON.stringify({root:config.remote_root,certificate})+'\n');
  remoteResult=await new Promise((resolve,reject)=>{const lines=createInterface({input:remote.stdout});const timer=setTimeout(()=>reject(Error('remote browser timeout')),45000);lines.once('line',l=>{clearTimeout(timer);resolve(JSON.parse(l))});remote.once('error',()=>{clearTimeout(timer);reject(failure('browser_ssh_spawn_failed'))});remote.once('exit',()=>{clearTimeout(timer);reject(Error('remote browser exited'))})});
  if(digest(certificate)!==digest(remoteResult.trusted.ca_export)||remoteResult.untrusted.ca_export!==null)throw Error('browser trust witness mismatch');
  browser=await chromium.connectOverCDP('http://127.0.0.1:18483');untrusted=await chromium.connectOverCDP('http://127.0.0.1:18484');
  for(const [kind,b]of [['trusted',browser],['untrusted',untrusted]]){const cdp=await b.newBrowserCDPSession();const args=(await cdp.send('Browser.getBrowserCommandLine')).arguments;if(args.some(a=>/^--(ignore-certificate-errors|allow-insecure-localhost|test-type)/.test(a))||!args.includes(`--user-data-dir=${remoteResult[kind].home}/profile`))throw Error('unsafe or unbound Chromium arguments');remoteResult[kind].arguments=args;}
  const badPage=await untrusted.contexts()[0].newPage();let rejected=false;try{await badPage.goto(payload.descriptor.endpoint+'/healthz',{timeout:10000})}catch(e){rejected=String(e).includes('ERR_CERT_AUTHORITY_INVALID')}await badPage.close();if(!rejected)throw Error('untrusted control accepted CA');
  const page=await browser.contexts()[0].newPage();const cdp=await page.context().newCDPSession(page);const sent=[],responses=[],failures=[],pending=new Map();
  cdp.on('Network.requestWillBeSent',e=>{if(e.request.url.startsWith(payload.descriptor.endpoint+'/')){sent.push(e);pending.set(e.requestId,{method:e.request.method,route:new URL(e.request.url).pathname})}});
  cdp.on('Network.responseReceived',e=>{if(!pending.has(e.requestId))return;const request=pending.get(e.requestId);const entry={...request,status:e.response.status,request_id:Object.entries(e.response.headers).find(([k])=>k.toLowerCase()==='x-request-id')?.[1]??'',scope:request.route};responses.push(entry)});
  cdp.on('Network.loadingFailed',e=>{if(pending.has(e.requestId))failures.push({...pending.get(e.requestId),error_code:e.errorText})});
  await cdp.send('Network.enable');
  await page.exposeBinding('laneControl',async(_source,op)=>control(op));
  await page.exposeBinding('laneObserve',async(_source,op,mark)=>{if(op==='begin')return {sent:sent.length,received:responses.length};await new Promise(resolve=>setTimeout(resolve,30));return {outgoing_requests:sent.length-mark.sent,transcript:responses.slice(mark.received).filter(e=>e.method!=='OPTIONS')}});
  await page.goto('http://localhost:18481/');
  const cases=await page.evaluate(async config=>{const {createHubClient}=await import('/sdk.js');const {runScenarios}=await import('/scenarios.mjs');return runScenarios(createHubClient,config,window.laneControl,window.laneObserve)},payload);
  const options=responses.filter(e=>e.method==='OPTIONS');const successful=responses.filter(e=>e.status===200&&e.method!=='OPTIONS');
  cases.push({id:'real_browser_cors',expected:{preflight_succeeded:true,cross_origin:true},actual:{preflight_succeeded:options.some(e=>e.status>=200&&e.status<300),cross_origin:new URL(page.url()).origin!==new URL(payload.descriptor.endpoint).origin},evidence_kind:'http',request_transcript:options.filter(e=>e.request_id)});
  cases.push({id:'browser_normal_tls_validation',expected:{trusted_succeeded:true,untrusted_error:'ERR_CERT_AUTHORITY_INVALID'},actual:{trusted_succeeded:successful.length>0,untrusted_error:rejected?'ERR_CERT_AUTHORITY_INVALID':'none'},evidence_kind:'http',request_transcript:successful.slice(0,1)});
  const runtime={...remoteResult.runtime,pid:remoteResult.trusted.pid,browser:await browser.version()};
  result={cases,runtime,transport_observer:{kind:'Chromium CDP Network',outgoing_requests:sent.length,responses:responses.length,failures,preflight:options,trust:remoteResult,certificate_sha256:digest(certificate)}};
 }catch(error){scenarioError=errorFact(error);}
 const cleanup=await cleanupAll([
  ['trusted-browser',async()=>{if(browser)await browser.close();},5000],
  ['untrusted-browser',async()=>{if(untrusted)await untrusted.close();},5000],
  ['browser-ssh-and-forwards',async()=>remote?admitExit(await stopOwnedGroup(remote)):undefined,30000],
  ['remote-browser-evidence',async()=>{
   if(!remote)return;
   const raw=execFileSync('ssh',[...sshArgs,config.ssh_alias,`python3 '${remoteScript}' --verify-cleanup '${config.remote_root}'`],{encoding:'utf8',timeout:15000,maxBuffer:1048576});
   const evidence=JSON.parse(raw);return validateRemoteCleanup(evidence,remoteResult?[remoteResult.trusted.pid,remoteResult.untrusted.pid]:[]);
  },16000],
  ['page-server',async()=>{if(server){server.closeAllConnections();await new Promise((resolve,reject)=>server.close(error=>error?reject(error):resolve()));}},5000],
  ['local-browser-listeners',()=>assertPortsClosed([18481,18483,18484]),5000],
  ['browser-private-log',async()=>{if(remoteStderr)await writeFile(config.local_log,remoteStderr,{mode:0o600,flag:'wx'});}],
 ]);
 if(scenarioError||cleanup.status!=='passed')throw Object.assign(failure('browser_lane_failed'),{laneDetails:{scenario_error:scenarioError,cleanup}});
 return {...result,cleanup};
}
