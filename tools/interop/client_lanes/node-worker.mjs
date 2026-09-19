import {readFileSync} from 'node:fs';
import {responseHeader} from './contract.mjs';
import {channel} from 'node:diagnostics_channel';
import {pathToFileURL} from 'node:url';
import {runScenarios} from './scenarios.mjs';
let resolveControl;const control=op=>new Promise(resolve=>{resolveControl=resolve;process.send({control:op})});
process.on('message',async message=>{
 if(message.reply){resolveControl(message.reply);return;}
 if(!message.config)return;
 try{
  const config=message.config;const endpoint=config.descriptor.endpoint;const requests=[],responses=[],failures=[];
  channel('undici:request:create').subscribe(({request})=>{if(String(request.origin)===endpoint)requests.push({method:request.method,path:request.path})});
  channel('undici:request:headers').subscribe(({request,response})=>{
   if(String(request.origin)!==endpoint)return;const id=responseHeader(response.headers,'x-request-id');
   const route=new URL(request.path,endpoint).pathname;
   responses.push({method:request.method,route,status:response.statusCode,request_id:id,scope:route});
  });
  channel('undici:request:error').subscribe(({request,error})=>{if(String(request.origin)===endpoint){const route=new URL(request.path,endpoint).pathname;failures.push({method:request.method,route,scope:route,error_code:error.code??error.name})}});
  const observe=async(op,mark)=>op==='begin'?{sent:requests.length,received:responses.length}:{outgoing_requests:requests.length-mark.sent,transcript:responses.slice(mark.received)};
  const {createHubClient}=await import(pathToFileURL(config.entry));
  const cases=await runScenarios(createHubClient,config,control,observe);process.send({cases,transport_observer:{kind:'node-undici-diagnostics-channel',outgoing_requests:requests.length,responses:responses.length,failures},runtime:{os:process.platform,architecture:process.arch,node:process.version,pid:process.pid,...(process.platform==='linux'?{distribution:readFileSync('/etc/os-release','utf8').match(/^PRETTY_NAME="(.+)"$/m)?.[1]??'Linux'}:{})}});process.disconnect();
 }catch(error){process.send({failure:{name:error.name,message:error.message}});process.disconnect();process.exitCode=1}
});
