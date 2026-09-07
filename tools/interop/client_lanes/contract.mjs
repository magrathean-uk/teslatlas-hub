import {open} from 'node:fs/promises';
import {constants} from 'node:fs';
import {isDeepStrictEqual} from 'node:util';
export async function readPrivate(path){
 if(typeof path!=='string')throw Error('private path required');
 const f=await open(path,constants.O_RDONLY|constants.O_NOFOLLOW|constants.O_NONBLOCK);
 try {const s=await f.stat();if(!s.isFile()||s.uid!==process.getuid()||(s.mode&0o077)||s.size>1048576)throw Error('invalid private descriptor');return JSON.parse(await f.readFile('utf8'));}finally{await f.close();}
}
export function makeCase(id,expected,actual,evidence_kind,request_transcript=[],process_evidence){
 const transport=evidence_kind==='http'?request_transcript.length>0&&request_transcript.every(r=>r.request_id&&Number.isInteger(r.status)):evidence_kind==='identity'?!!process_evidence&&Object.keys(process_evidence).length>0:request_transcript.length===0&&actual.outgoing_requests===0;
 return {id,status:expected!==null&&typeof expected==='object'&&!Array.isArray(expected)&&Object.keys(expected).length>0&&isDeepStrictEqual(expected,actual)&&transport&&rejectionStatusMatches(id,expected,actual,request_transcript)?'passed':'failed',expected,actual,evidence_kind,request_transcript,...(process_evidence?{process_evidence}:{})};
}
export const headerText=value=>typeof value==='string'?value:Buffer.from(value).toString('utf8');
export function responseHeader(headers,name){
 if(Array.isArray(headers)){for(let i=0;i<headers.length;i+=2)if(headerText(headers[i]).toLowerCase()===name.toLowerCase())return headerText(headers[i+1]);return '';}
 const value=Object.entries(headers).find(([key])=>key.toLowerCase()===name.toLowerCase())?.[1];return typeof value==='string'?value:'';
}

function rejectionStatusMatches(id,expected,actual,transcript){
 const required={bad_invitation:401,replayed_invitation:401,revocation:401,unknown_vehicle:404}[id];
 if(required!==undefined)return expected.http_status===required&&actual.http_status===required&&transcript.some(r=>r.status===required)&&!transcript.some(r=>r.status>=500);
 if(id==='credential_rotation_api')return expected.old_credential_status===401&&actual.old_credential_status===401&&transcript.some(r=>r.status===401)&&!transcript.some(r=>r.status>=500);
 return true;
}
export function normalizeClientRuntime(mode,runtime){
 if(!['node','browser'].includes(mode))throw Error('invalid runtime lane mode');
 const architecture={arm64:'arm64',aarch64:'arm64',x64:'amd64',x86_64:'amd64',amd64:'amd64'}[runtime.architecture];
 const os=runtime.os==='darwin'?'macOS':runtime.os==='linux'?(runtime.distribution??'Linux'):null;
 const version=mode==='node'?runtime.node:runtime.browser;
 if(!architecture||!os||typeof version!=='string'||!version)throw Error('missing observed client runtime');
 return {os,architecture,native_or_emulated:'native',service_mode:mode==='node'?'owned-node-process':'owned-chromium-process',tool_versions:{[mode==='node'?'node':'chromium']:version}};
}
export const laneExitCode=cases=>cases.some(c=>c.status==='failed')?1:0;
