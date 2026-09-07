// SPDX-License-Identifier: AGPL-3.0-only
import {createHash} from 'node:crypto';
import {open,realpath,stat} from 'node:fs/promises';
import net from 'node:net';
import path from 'node:path';

const FRAME_BYTES=1048576;
const OPERATION_MS=Object.freeze({verify:30000,stop:60000,start:60000,pair:160000,revoke:160000});
const sha=raw=>createHash('sha256').update(raw).digest('hex');
const exact=(value,keys,label)=>{if(!value||typeof value!=='object'||Array.isArray(value)||Object.keys(value).sort().join('\0')!==[...keys].sort().join('\0'))throw Error(`${label} fields invalid`);return value};
async function privateBytes(filename,maximum){
 if(typeof filename!=='string'||!path.isAbsolute(filename)||await realpath(filename)!==filename)throw Error('private path invalid');
 const handle=await open(filename,'r');try{const before=await handle.stat();if(!before.isFile()||before.uid!==process.getuid()||(before.mode&0o077)||before.size>maximum||before.nlink!==1)throw Error('private file boundary invalid');const raw=await handle.readFile();const after=await handle.stat();if(raw.length>maximum||before.dev!==after.dev||before.ino!==after.ino||before.size!==after.size||before.mtimeMs!==after.mtimeMs)throw Error('private file changed during read');return raw;}finally{await handle.close()}
}
export async function readInstalledLaneConfig(filename,expectedMode){
 const raw=await privateBytes(filename,1048576);let value;try{value=JSON.parse(raw)}catch{throw Error('installed lane config invalid JSON')}
 exact(value,['schema_version','kind','mode','session_input'],'installed lane config');
 if(value.schema_version!==2||value.kind!=='installed-client-lane'||value.mode!==expectedMode)throw Error('installed lane config identity invalid');
 exact(value.session_input,['path','sha256'],'session input binding');
 const sessionRaw=await privateBytes(value.session_input.path,1048576);if(sha(sessionRaw)!==value.session_input.sha256)throw Error('session input digest mismatch');
 let session;try{session=JSON.parse(sessionRaw)}catch{throw Error('session input invalid JSON')}
 const fields=['schema_version','kind','run_id','cell_id','adapter_id','client_id','session_id','instance_nonce','header','case_contract','host_session','broker','inputs','actors','outputs','bounds'];exact(session,fields,'session input');
 const adapter=`typescript_${expectedMode}`;if(session.schema_version!==1||session.kind!=='matrix-adapter-session'||session.adapter_id!==adapter||session.client_id!==adapter||!session.cell_id.startsWith(adapter+'__')||session.broker?.kind!=='unix'||session.broker.socket_path!==session.host_session?.broker_socket||session.host_session?.session_id!==session.session_id)throw Error('session input lane binding mismatch');
 return Object.freeze({config:Object.freeze(value),session:Object.freeze(session),sessionRaw,sessionSha256:sha(sessionRaw)});
}

export class InstalledBroker {
 constructor(session){this.session=session;this.socket=null;this.buffer=Buffer.alloc(0);this.challenge=null;this.sequence=0;this.pending=false;}
 async open(){if(this.socket)throw Error('broker already open');const deadline=Date.now()+10000;this.socket=net.createConnection({path:this.session.broker.socket_path});await new Promise((resolve,reject)=>{const timer=setTimeout(()=>{cleanup();this.socket.destroy();reject(Error('broker greeting timed out'))},Math.max(1,deadline-Date.now()));const cleanup=()=>{clearTimeout(timer);this.socket.off('connect',connected);this.socket.off('error',failed)};const connected=()=>{cleanup();resolve()};const failed=error=>{cleanup();reject(error)};this.socket.once('connect',connected);this.socket.once('error',failed)});const greeting=await this.#frame(Math.max(1,deadline-Date.now()));exact(greeting,['schema_version','type','session_id','sequence','challenge'],'broker greeting');if(greeting.schema_version!==1||greeting.type!=='challenge'||greeting.session_id!==this.session.session_id||greeting.sequence!==0||!/^[0-9a-f]{64}$/.test(greeting.challenge))throw Error('broker greeting identity invalid');this.challenge=greeting.challenge;return this;}
 async request(operation){if(this.pending||!this.socket)throw Error('broker request state invalid');exact(operation,operation.op==='revoke'?['op','device_id']:['op'],'broker operation');if(!Object.hasOwn(OPERATION_MS,operation.op))throw Error('broker operation invalid');this.pending=true;try{const request={schema_version:1,session_id:this.session.session_id,sequence:++this.sequence,challenge:this.challenge,...operation};this.socket.write(JSON.stringify(request)+'\n');const reply=await this.#frame(OPERATION_MS[operation.op]);const keys=reply.type==='reply'?['schema_version','type','session_id','sequence','challenge','result']:['schema_version','type','session_id','sequence','challenge','error'];exact(reply,keys,'broker reply');if(reply.schema_version!==1||reply.session_id!==this.session.session_id||reply.sequence!==this.sequence||reply.challenge===this.challenge||!/^[0-9a-f]{64}$/.test(reply.challenge))throw Error('broker reply identity invalid');this.challenge=reply.challenge;if(reply.type!=='reply')throw Error('broker operation failed');return reply.result;}finally{this.pending=false}}
 close(){this.socket?.end();this.socket=null;}
 async #frame(timeoutMs){const deadline=Date.now()+timeoutMs;while(true){const newline=this.buffer.indexOf(10);if(newline>=0){if(newline>FRAME_BYTES)throw Error('broker frame exceeds bound');const raw=this.buffer.subarray(0,newline);this.buffer=this.buffer.subarray(newline+1);let value;try{value=JSON.parse(raw)}catch{throw Error('broker frame invalid')};return value}const remaining=deadline-Date.now();if(remaining<=0){this.socket.destroy();throw Error('broker frame timed out')}const chunk=await new Promise((resolve,reject)=>{const timer=setTimeout(()=>{cleanup();this.socket.destroy();reject(Error('broker frame timed out'))},remaining);const data=value=>{cleanup();resolve(value)};const end=()=>{cleanup();resolve(null)};const error=value=>{cleanup();reject(value)};const cleanup=()=>{clearTimeout(timer);this.socket.off('data',data);this.socket.off('end',end);this.socket.off('error',error)};this.socket.once('data',data);this.socket.once('end',end);this.socket.once('error',error)});if(chunk===null)throw Error('premature broker EOF');this.buffer=Buffer.concat([this.buffer,chunk]);if(this.buffer.length>FRAME_BYTES+1)throw Error('broker frame exceeds bound')}}
}
