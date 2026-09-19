"""Current-Hub raw HTTP contract validation. Independent of server and SDKs."""
import argparse
import importlib.util
import hashlib
import json
import math
import os
from pathlib import Path
import re
import ssl
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

from jsonschema import Draft202012Validator, FormatChecker

PROFILE_ID='hub-http-v1@1.0.0'
DEFAULT_PROFILE=Path(__file__).resolve().parents[1]/'profiles/hub-http-v1/1.0.0'
MAX_BYTES=1048576

_native_spec=importlib.util.spec_from_file_location('hub_native_evidence',Path(__file__).with_name('hub_native_evidence.py'))
native_evidence=importlib.util.module_from_spec(_native_spec)
_native_spec.loader.exec_module(native_evidence)


class AcceptanceError(Exception):
    """Messages are fixed labels; never interpolate server data or credentials."""

def verify_native_fixture(config):
    try:
        return native_evidence.verify(config)
    except ValueError as error:
        raise AcceptanceError(str(error)) from error

def strict_json(raw):
    def unique(pairs):
        result={}
        for key,value in pairs:
            if key in result:raise ValueError('duplicate member')
            result[key]=value
        return result
    def reject(_):raise ValueError('non-finite number')
    def finite(value):
        parsed=float(value)
        if not math.isfinite(parsed):raise ValueError('overflowing number')
        return parsed
    return json.loads(raw,object_pairs_hook=unique,parse_constant=reject,parse_float=finite)

def read_private(path):
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as stream:
        meta=os.fstat(stream.fileno())
        if not stat.S_ISREG(meta.st_mode) or meta.st_uid!=os.getuid() or meta.st_mode & 0o077:
            raise AcceptanceError('private file permissions invalid')
        raw=stream.read(65537)
    if len(raw)>65536:raise AcceptanceError('private file exceeds limit')
    return strict_json(raw)

def load_profile(root,expected_hash=None):
    root=Path(root)
    sums=(root/'SHA256SUMS').read_bytes()
    digest=hashlib.sha256(sums).hexdigest()
    if expected_hash is not None and digest!=expected_hash:raise AcceptanceError('profile digest mismatch')
    listed=[]
    for line in sums.decode('ascii').splitlines():
        value,name=line.split('  ',1)
        if not re.fullmatch('[0-9a-f]{64}',value) or name.startswith('/') or '..' in Path(name).parts:
            raise AcceptanceError('invalid profile manifest')
        if name in listed or hashlib.sha256((root/name).read_bytes()).hexdigest()!=value:
            raise AcceptanceError('profile file digest mismatch')
        listed.append(name)
    required={'profile.json','openapi.json','discovery.schema.json','resources.schema.json','auth.schema.json','errors.schema.json','field-semantics.json','cases.json','sync-regression.json'}
    if not required<=set(listed):raise AcceptanceError('incomplete profile manifest')
    profile=strict_json((root/'profile.json').read_bytes())
    if profile.get('profile_id')!=PROFILE_ID:raise AcceptanceError('unsupported profile identity')
    return dict(profile,profile_sha256=digest)

def validate_raw(root,kind,status,headers,raw):
    """Return bounded diagnostic labels only; raw secret-bearing values never appear."""
    headers={k.lower():v for k,v in headers.items()}
    if len(raw)>MAX_BYTES:return ['response exceeds byte limit']
    if kind=='drives' and status in (200,304):
        etag=headers.get('etag','')
        if re.fullmatch(r'"[^"\x00-\x20\x7f]+"',etag) is None:return ['drive ETag missing or invalid']
        if headers.get('cache-control')!='no-store':return ['drive Cache-Control must be no-store']
    if kind=='drives' and status==503 and raw==b'':return []
    empty={
        'vehicles':{401,503},'current':{401,404,503},'drives':{401,304},
        'claim':{401,404,503},'rotate':{401,404,503}}
    if status in empty.get(kind,set()):return [] if raw==b'' else ['expected empty response body']
    if kind=='claim' and status in (400,415,422):
        # Axum extractor errors are bounded text, not the public error envelope.
        return [] if raw and headers.get('content-type','').startswith('text/plain') else ['expected extractor text error']
    if status==200:
        if kind=='discovery':name='discovery';definition=None
        elif kind in ('claim','rotate','invitation'):name='auth';definition='claim' if kind=='rotate' else kind
        elif kind in ('vehicles','current','drives','health','ready'):name='resources';definition=kind
        else:return ['unknown route kind']
    elif kind=='drives' and status in (400,404,503) or kind=='discovery' and status==503:
        name='errors';definition=None
    elif kind=='ready' and status==503:name='resources';definition='ready'
    else:return ['status not specified by profile']
    if not headers.get('content-type','').split(';')[0].strip()=='application/json':return ['expected JSON content type']
    try:
        value=strict_json(raw)
        document=strict_json((Path(root)/(name+'.schema.json')).read_bytes())
        if definition:document={**document,'$ref':'#/$defs/'+definition}
        errors=list(Draft202012Validator(document,format_checker=FormatChecker()).iter_errors(value))
        if errors:return ['body violates '+kind+' schema']
        if kind=='invitation':
            uri=urllib.parse.urlsplit(value['pairingUri'])
            expected={'pairing_id':[value['pairingId']],'secret':[value['secret']],'endpoint':[value['endpoint']],'tls_pin':[value['tlsPin']]}
            if uri.scheme!='teslatlas-hub' or uri.netloc!='pair' or uri.path or uri.fragment or urllib.parse.parse_qs(uri.query,strict_parsing=True)!=expected:
                return ['invitation URI does not match its fields']
        if kind=='ready' and value['status']!=('ready' if status==200 else 'not_ready'):
            return ['readiness status mismatch']
        if kind=='drives' and status in (400,404,503):
            allowed={400:{'invalid_limit','invalid_query','invalid_time_range','invalid_cursor'},404:{'vehicle_not_found'},503:{'service_unavailable'}}
            if value['error']['code'] not in allowed[status]:return ['error code does not match status']
        return []
    except (ValueError,UnicodeError,TypeError,KeyError):return ['invalid JSON response']

class DenyRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,*args,**kwargs):return None

def run_network(config):
    """A one-use acceptance session consumes a supported CLI invitation privately."""
    needed={'endpoint','profile_id','profile_path','profile_sha256','invitation_path','certificate_path','scenario_path','scenario_sha256','binary_sha256','seed_binary_sha256','provenance','update_request_path','update_receipt_path'}
    if not isinstance(config,dict) or not needed<=config.keys():raise AcceptanceError('missing endpoint, profile or synthetic prerequisites')
    if config['profile_id']!=PROFILE_ID or config['provenance']!='synthetic-real-process':raise AcceptanceError('synthetic profile binding required')
    verified_native=verify_native_fixture(config)
    url=urllib.parse.urlsplit(config['endpoint'])
    if url.scheme!='https' or url.hostname not in ('127.0.0.1','localhost','::1') or url.username or url.password or url.query or url.fragment or url.path not in ('','/'):
        raise AcceptanceError('owned loopback HTTPS endpoint required')
    profile=load_profile(config['profile_path'],config['profile_sha256'])
    scenario_bytes=Path(config['scenario_path']).read_bytes()
    if hashlib.sha256(scenario_bytes).hexdigest()!=config['scenario_sha256']:raise AcceptanceError('scenario digest mismatch')
    scenario=strict_json(scenario_bytes)
    if scenario.get('name')!='two-vehicles-five-drives' or scenario.get('provenance')!='synthetic-only' or 'later_current' not in scenario:
        raise AcceptanceError('complete synthetic scenario required')
    invitation=read_private(config['invitation_path'])
    if validate_raw(config['profile_path'],'invitation',200,{'content-type':'application/json'},json.dumps(invitation).encode()):
        raise AcceptanceError('CLI invitation shape invalid')
    if invitation['endpoint']!=config['endpoint'] or invitation['expiresAtMs']<=int(time.time()*1000):raise AcceptanceError('invitation endpoint or expiry invalid')
    pem=Path(config['certificate_path']).read_text()
    der=ssl.PEM_cert_to_DER_cert(pem)
    if hashlib.sha256(der).hexdigest()!=invitation['tlsPin']:raise AcceptanceError('invitation TLS pin mismatch')
    context=ssl.create_default_context(cafile=config['certificate_path'])
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),urllib.request.HTTPSHandler(context=context),DenyRedirects())
    cases=[]
    captures=[]
    def exchange(case,kind,path,expected=200,bearer=None,body=None,headers=None):
        request_headers=dict(headers or {})
        if bearer is not None:request_headers['Authorization']='Bearer '+bearer
        payload=None
        if body is not None:
            payload=json.dumps(body).encode();request_headers['Content-Type']='application/json'
            if kind=='claim' and len(payload)>profile['max_claim_request_bytes']:
                raise AcceptanceError('claim request exceeds profile byte limit')
        request=urllib.request.Request(config['endpoint']+path,data=payload,headers=request_headers)
        try:
            response=opener.open(request,timeout=5)
        except urllib.error.HTTPError as error:response=error
        with response:
            raw=response.read(MAX_BYTES+1)
            response_headers=dict(response.headers.items())
            if response.status!=expected:raise AcceptanceError(case+': unexpected HTTP status')
            problems=validate_raw(config['profile_path'],kind,response.status,response_headers,raw)
            if problems:raise AcceptanceError(case+': '+problems[0])
        cases.append({'case_id':case,'kind':kind,'status':expected,'raw_bytes':len(raw),'raw_sha256':hashlib.sha256(raw).hexdigest()})
        # Only nonsecret synthetic resources are captured. Cursor and credential
        # responses receive byte hashes, never body logging or normalization.
        if kind in ('current','vehicles','health','ready','discovery') and expected==200:
            captures.append({'case_id':case,'kind':kind,'body_utf8':raw.decode(),'headers':{k:v for k,v in response_headers.items() if k.lower() in ('content-type','cache-control')}})
        return (strict_json(raw) if raw else None),{k.lower():v for k,v in response_headers.items()}
    def require(condition,label):
        if not condition:raise AcceptanceError(label)
    discovery,_=exchange('discovery-success','discovery','/.well-known/teslatlas-hub')
    require(discovery['hub_id']==config.get('hub_id'),'Hub identity mismatch')
    require(discovery['capabilities']==profile['capabilities'],'fixture capabilities incomplete')
    health,_=exchange('health-success','health','/healthz')
    require(health['version']==discovery['version'],'health product version mismatch')
    exchange('ready-success','ready','/readyz')
    exchange('missing-bearer','vehicles','/v1/vehicles',401)
    exchange('bad-bearer','vehicles','/v1/vehicles',401,bearer='synthetic-invalid-token')
    verified_native=verify_native_fixture(config)
    claim_path='/v1/pairings/'+invitation['pairingId']+'/claim'
    claim_body={'secret':invitation['secret'],'device_name':'Synthetic current-profile client'}
    claim,_=exchange('claim-success','claim',claim_path,body=claim_body)
    token=claim['access_token']
    exchange('pairing-replay','claim',claim_path,401,body=claim_body)
    vehicles,_=exchange('vehicles-success','vehicles','/v1/vehicles',bearer=token)
    require(vehicles['vehicles']==scenario['vehicles'],'scenario vehicle identity or Unicode mismatch')
    vehicle,other=scenario['vehicle_ids']
    current_path='/v1/vehicles/'+vehicle+'/current'
    current,_=exchange('current-success','current',current_path,bearer=token)
    for key,value in scenario['current'].items():require(current[key]==value,'initial current scenario mismatch: '+key)
    empty,_=exchange('current-empty','current','/v1/vehicles/'+other+'/current',bearer=token)
    require(empty['observed_at_ms'] is None,'empty observation null was lost')
    exchange('current-unknown','current','/v1/vehicles/33333333-3333-4333-8333-333333333333/current',404,bearer=token)
    drive_path='/v1/vehicles/'+vehicle+'/drives'
    cursor=None;all_items=[];first_cursor=None
    for index,expected_ids in enumerate(scenario['drive_pages_at_limit_2']):
        query={'limit':2}
        if cursor is not None:query['cursor']=cursor
        path=drive_path+'?'+urllib.parse.urlencode(query)
        page,headers=exchange('drives-success' if index==0 else 'scenario-page-'+str(index+1),'drives',path,bearer=token)
        require([item['id'] for item in page['items']]==expected_ids,'drive page ordering mismatch')
        all_items.extend(page['items']);cursor=page['next_cursor']
        if index==0:
            first_cursor=cursor
            exchange('drive-conditional','drives',path,304,bearer=token,headers={'If-None-Match':headers['etag']})
    require(cursor is None,'final page cursor must be null')
    for item in all_items:
        expected=scenario['drives'][str(item['id'])]
        for key,value in expected.items():require(item[key]==value,'drive scenario field mismatch: '+key)
    equal=[item['start_date_ms'] for item in all_items if item['id'] in scenario['equal_start_time_ids']]
    require(len(equal)==2 and equal[0]==equal[1],'equal-start-time witness absent')
    for case,query,code in [('invalid_limit','limit=0','invalid_limit'),('invalid_time_range','from_ms=20&to_ms=10','invalid_time_range'),('invalid_query','from=2026-09-01','invalid_query')]:
        value,_=exchange(case,'drives',drive_path+'?'+query,400,bearer=token)
        require(value['error']['code']==code,'wrong query error code')
    for case,path,params in [('invalid_cursor','/v1/vehicles/'+other+'/drives',{'limit':2,'cursor':first_cursor}),('cursor-time-binding',drive_path,{'limit':2,'from_ms':1,'cursor':first_cursor})]:
        value,_=exchange(case,'drives',path+'?'+urllib.parse.urlencode(params),400,bearer=token)
        require(value['error']['code']=='invalid_cursor','cursor binding error absent')
    # A private marker requests one harness-owned append. No HTTP seed route.
    fd=os.open(config['update_request_path'],os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    os.close(fd)
    deadline=time.monotonic()+30
    while not Path(config['update_receipt_path']).exists():
        if time.monotonic()>deadline:raise AcceptanceError('owned scenario update timed out')
        time.sleep(0.1)
    update=read_private(config['update_receipt_path'])
    require(update.get('status')=='passed' and update.get('charge')==scenario['charge'],'reopened store charge preservation failed')
    later,_=exchange('later-current','current',current_path,bearer=token)
    for key,value in scenario['later_current'].items():require(later[key]==value,'later current scenario mismatch: '+key)
    rotated,_=exchange('rotate-success','rotate','/v1/device/rotate',bearer=token,body={})
    require(rotated['device_id']==claim['device_id'] and rotated['access_token']!=token,'rotation did not replace bearer')
    exchange('rotated-bearer','vehicles','/v1/vehicles',401,bearer=token)
    exchange('rotated-bearer-valid','vehicles','/v1/vehicles',bearer=rotated['access_token'])
    verified_native=verify_native_fixture(config)
    return {'schema_version':1,'status':'passed','provenance':'synthetic-real-process','profile_id':PROFILE_ID,'profile_sha256':profile['profile_sha256'],
        'scenario_sha256':config['scenario_sha256'],'hub_product_version':discovery['version'],'binary_sha256':config['binary_sha256'],'seed_binary_sha256':config['seed_binary_sha256'],
        'native_evidence':verified_native,'runs':len(cases),'passed':len(cases),'failed':0,'cases':cases,'synthetic_resource_captures':captures,
        'store_reopen':{'status':'passed','charge':update['charge']},'acceptance_boundary':'one native fixture; platform, clients and sync regression acceptance remain separate'}

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--config',default=os.environ.get('TESLATLAS_HUB_HTTP_CONFIG'));parser.add_argument('--json',action='store_true');args=parser.parse_args()
    try:
        if not args.config:raise AcceptanceError('private actual-Hub run config required')
        result=run_network(read_private(args.config))
        print(json.dumps(result,ensure_ascii=False,sort_keys=True,separators=(',',':')))
        return 0
    except AcceptanceError as error:
        print(json.dumps({'status':'failed','profile_id':PROFILE_ID,'error':str(error)}));return 1
    except Exception:
        # Paths, HTTP payloads, TLS and parser diagnostics may contain secrets.
        print(json.dumps({'status':'failed','profile_id':PROFILE_ID,'error':'actual-Hub acceptance failed; inspect private fixture evidence'}));return 1

if __name__=='__main__':raise SystemExit(main())
