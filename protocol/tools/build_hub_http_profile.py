#!/usr/bin/env python3
"""Export the independently authored current-Hub wire contract (Apache-2.0).

This authoring source contains public field/route facts, not Hub implementation.
Do not generate schemas or expected values by importing server source.
"""
import argparse
import hashlib
import json
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / 'profiles/hub-http-v1/1.0.0'
ID = 'hub-http-v1@1.0.0'
DRAFT = 'https://json-schema.org/draft/2020-12/schema'
INT = {'type':'integer','minimum':-(2**63),'maximum':2**63-1}
STR = {'type':'string','maxLength':65536}
UUID = {'type':'string','format':'uuid','pattern':r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'}
NUM = {'type':'number'}
BOOL = {'type':'boolean'}

def nullable(schema):
    return {'anyOf':[schema,{'type':'null'}]}

def obj(fields, required=None):
    return {'type':'object','properties':fields,'required':list(fields) if required is None else required,'additionalProperties':False}

def schema(name, content):
    return {'$schema':DRAFT,'$id':'urn:teslatlas:hub-http-v1:1.0.0:'+name,**content}

CURRENT_GROUPS = {
'integer':'observed_at_ms since battery_level usable_battery_level speed scheduled_charging_start_time charge_limit_soc charger_phases charge_current_request charge_current_request_max center_display_state sun_roof_percent_open download_perc install_perc',
'number':'latitude longitude heading ideal_battery_range_km est_battery_range_km rated_battery_range_km charge_energy_added outside_temp inside_temp charger_power odometer time_to_full_charge charger_actual_current charger_voltage elevation power tpms_pressure_fl tpms_pressure_fr tpms_pressure_rl tpms_pressure_rr active_route_latitude active_route_longitude active_route_energy_at_arrival active_route_miles_to_arrival active_route_minutes_to_arrival active_route_traffic_minutes_delay',
'boolean':'healthy is_climate_on is_preconditioning locked sentry_mode plugged_in windows_open driver_front_window_open driver_rear_window_open passenger_front_window_open passenger_rear_window_open doors_open driver_front_door_open driver_rear_door_open passenger_front_door_open passenger_rear_door_open charge_port_door_open update_available is_user_present trunk_open frunk_open tpms_soft_warning_fl tpms_soft_warning_fr tpms_soft_warning_rl tpms_soft_warning_rr service_mode sun_roof_installed',
'string':'display_name state charging_state shift_state version update_version update_status geofence model trim_badging exterior_color wheel_type spoiler_type climate_keeper_mode active_route_destination sun_roof_state'}
DRIVE_GROUPS = {
'integer':'duration_min speed_max start_soc end_soc ascent descent',
'number':'distance_km efficiency outside_temp_avg inside_temp_avg power_max power_min start_ideal_range_km end_ideal_range_km start_latitude start_longitude end_latitude end_longitude start_rated_range_km end_rated_range_km',
'string':'start_address end_address start_geofence end_geofence'}
TYPES = {'integer':INT,'number':NUM,'boolean':BOOL,'string':STR}

def fields(groups):
    return {field:nullable(TYPES[kind]) for kind,names in groups.items() for field in names.split()}

def bundle():
    car_fields = {'id':INT,'name':STR,'model':STR}
    car_fields.update({x:nullable(STR) for x in 'vin trim_badging marketing_name exterior_color wheel_type spoiler_type firmware_version'.split()})
    car_fields.update({x:nullable(INT) for x in ('source_eid','source_vid')})
    car_fields['efficiency_wh_per_km']=nullable(NUM)
    car_fields['settings']=obj({**{x:BOOL for x in 'enabled use_streaming_api suspend_min_resolved req_not_unlocked free_supercharging lfp_battery'.split()},'suspend_after_idle_min':INT,'suspend_min':INT})
    current = obj({'vehicle_id':UUID,**fields(CURRENT_GROUPS),'car':nullable(obj(car_fields))})
    drive = obj({'id':INT,'vehicle_id':UUID,'start_date_ms':INT,'end_date_ms':INT,**fields(DRIVE_GROUPS)})
    resources = schema('resources',{'$defs':{
        'vehicles':obj({'vehicles':{'type':'array','maxItems':10000,'items':obj({'vehicle_id':UUID,'display_name':nullable(STR)})}}),
        'current':current,'drive':drive,
        'drives':obj({'items':{'type':'array','maxItems':500,'items':drive},'next_cursor':nullable({'type':'string','minLength':1,'maxLength':4096})}),
        'health':obj({'status':{'const':'ok'},'version':STR}),
        'ready':{'oneOf':[obj({'status':{'const':'ready'}}),obj({'status':{'const':'not_ready'},'reason':{'enum':['catalogue_unavailable','lifecycle_quarantined','published_content_unservable','collector_absent','collector_stale','collector_auth_terminal']}})]}}})
    discovery = schema('discovery',obj({'hub_id':UUID,'protocol':{'const':'teslatlas-sync'},'protocol_major':{'const':1},'api_versions':{'const':['1.0']},
        'capabilities':{'oneOf':[{'const':['query.vehicles','query.current']},{'const':['query.vehicles','query.current','query.drives','sync.packs']}]},
        'version':STR,'sourceUrl':{'type':'string','format':'uri','maxLength':4096},'pack_format':{'const':'sqlite-zstd'},'manifestPublicKey':{'type':'string','pattern':'^[0-9a-f]{64}$'}},
        ['hub_id','protocol','protocol_major','api_versions','capabilities','version','sourceUrl','pack_format']))
    secret={'type':'string','pattern':'^[0-9a-f]{64}$'}
    auth = schema('auth',{'$defs':{
        'invitation':obj({'pairingId':UUID,'secret':secret,'expiresAtMs':INT,'endpoint':{'type':'string','format':'uri','maxLength':4096},'tlsPin':{'type':'string','pattern':'^[0-9a-f]{64}$'},'pairingUri':{'type':'string','pattern':'^teslatlas-hub://pair\\?','maxLength':16384}}),
        'claim_request':obj({'secret':STR,'device_name':STR}),
        'claim':obj({'device_id':UUID,'access_token':secret,'expires_at_ms':INT})}})
    errors=schema('errors',obj({'error':obj({'code':{'enum':['invalid_query','invalid_time_range','invalid_limit','invalid_cursor','vehicle_not_found','service_unavailable']},'message':STR})}))
    routes = [
        ('discovery','GET','/.well-known/teslatlas-hub',False,[200,503]),
        ('health','GET','/healthz',False,[200]),('ready','GET','/readyz',False,[200,503]),
        ('vehicles','GET','/v1/vehicles',True,[200,401,503]),
        ('current','GET','/v1/vehicles/{vehicle_id}/current',True,[200,401,404,503]),
        ('drives','GET','/v1/vehicles/{vehicle_id}/drives',True,[200,304,400,401,404,503]),
        ('claim','POST','/v1/pairings/{pairing_id}/claim',False,[200,400,401,404,415,422,503]),
        ('rotate','POST','/v1/device/rotate',True,[200,401,404,503])]
    profile={'profile_id':ID,'status':'candidate','contract_version':'1.0.0','product_version_binding':'independent; see ecosystem compatibility manifests',
        'license':'Apache-2.0','schema_dialect':DRAFT,'max_response_bytes':1048576,
        'capabilities':['query.vehicles','query.current','query.drives','sync.packs'],
        'unsupported':{'rich_protocol_versions':'1.0/1.1/1.2 rich profiles are separate contracts','commands':'no public vehicle command route','sse':'no public event stream or epoch','metadata':'no public metadata CRUD','revisions':'no public current or drive snapshot revision','charges':'no public charge query; retained in sync packs','protocol_version_header':'not emitted or required'},
        'additive_policy':'This version is a frozen bounded shape. Unknown fields fail acceptance; a new profile may explicitly allow optional additions. Clients may ignore unknown fields without assuming new capabilities.',
        'integers':{'wire':'signed 64-bit JSON integers','javascript':'decode losslessly or reject values outside [-9007199254740991,9007199254740991] before normalization; never silently round IDs'},
        'cursor':{'opaque':True,'binding':['Hub installation cursor key','vehicle UUID','exact from_ms','exact to_ms'],'order':['start_date_ms descending','id descending'],'time_window':'from_ms inclusive, to_ms exclusive; default 0 through signed64 maximum','limit':{'default':100,'min':1,'max':500},'handling':'Preserve byte-for-byte, percent-encode once as query value, never parse or log; limit may change. Reuse against another Hub or time window must fail.'},
        'pairing':{'invitation_command':'teslatlas-hub --config PRIVATE_CONFIG pair --json','claim_fields':['secret','device_name'],'path_field':'pairingId','lifetime':'expiresAtMs epoch milliseconds; default invitation lifetime 900 seconds, owner-configurable; single use. Bearer expires_at_ms is returned by claim/rotate. Rotation invalidates prior bearer.','trust':'endpoint plus tlsPin (SHA256 of DER leaf certificate); validate TLS identity before sending secret. Fixture uses a private CA trust file and verifies hostname. pairingUri duplicates secret; keep whole invitation private.'},
        'routes':[{'kind':kind,'method':method,'path':path,'bearer':auth_required,'statuses':statuses,'etag_required':kind=='drives',**({'error_503_forms':['empty authentication-store failure','JSON service_unavailable query failure']} if kind=='drives' else {})} for kind,method,path,auth_required,statuses in routes],
        'sync_regression_appendix':'sync-regression.json'}
    ref=lambda kind: 'auth.schema.json#/$defs/claim' if kind in ('claim','rotate') else ('discovery.schema.json' if kind=='discovery' else 'resources.schema.json#/$defs/'+kind)
    paths={}
    for kind,method,path,authenticated,statuses in routes:
        parameters=[{'name':name,'in':'path','required':True,'schema':UUID} for name in ('vehicle_id','pairing_id') if '{'+name+'}' in path]
        if kind=='drives':
            parameters += [{'name':name,'in':'query','schema':val} for name,val in {'from_ms':{'type':'integer','minimum':0,'maximum':2**63-1},'to_ms':{'type':'integer','minimum':0,'maximum':2**63-1},'limit':{'type':'integer','minimum':1,'maximum':500,'default':100},'cursor':{'type':'string','maxLength':4096}}.items()]
            parameters.append({'name':'If-None-Match','in':'header','schema':STR})
        operation={'operationId':kind,'responses':{},'parameters':parameters,'security':[{'pairedBearer':[]}] if authenticated else []}
        if kind=='claim':operation['requestBody']={'required':True,'content':{'application/json':{'schema':{'$ref':'auth.schema.json#/$defs/claim_request'}}}}
        for status in statuses:
            response={'description': 'Successful response' if status==200 else 'Route-specific error or conditional response; see profile cases'}
            if status==200:response['content']={'application/json':{'schema':{'$ref':ref(kind)}}}
            elif kind=='drives' and status in (400,404,503) or kind=='discovery' and status==503:
                response['content']={'application/json':{'schema':{'$ref':'errors.schema.json'}}}
            elif kind=='ready' and status==503:response['content']={'application/json':{'schema':{'$ref':ref(kind)}}}
            if kind=='drives' and status==503:
                response['description']='Authentication-store outage: empty response body; query-originated outage: application/json service_unavailable envelope. Nonempty bodies must match the JSON error schema.'
                response['x-empty-body-allowed']=True
            if kind=='drives' and status in (200,304):response['headers']={'ETag':{'required':True,'schema':STR},'Cache-Control':{'required':True,'schema':{'const':'no-store'}}}
            operation['responses'][str(status)]=response
        paths[path]={method.lower():operation}
    openapi={'openapi':'3.1.0','info':{'title':'Current Teslatlas Hub public HTTP profile','version':'1.0.0'},'paths':paths,'components':{'securitySchemes':{'pairedBearer':{'type':'http','scheme':'bearer'}}}}
    vehicle='11111111-1111-4111-8111-111111111111'
    examples={'discovery':{'hub_id':vehicle,'protocol':'teslatlas-sync','protocol_major':1,'api_versions':['1.0'],'capabilities':profile['capabilities'],'version':'2026.36.2','sourceUrl':'https://example.invalid/source','pack_format':'sqlite-zstd'},
        'vehicles':{'vehicles':[{'vehicle_id':vehicle,'display_name':None}]},
        'current':{**{key:None for key in current['properties']},'vehicle_id':vehicle},
        'drives':{'items':[{**{key:None for key in drive['properties']},'id':101,'vehicle_id':vehicle,'start_date_ms':1788565900000,'end_date_ms':1788565960000}],'next_cursor':None},
        'health':{'status':'ok','version':'2026.36.2'},'ready':{'status':'ready'},
        'claim':{'device_id':vehicle,'access_token':'0'*64,'expires_at_ms':1788567300000},
        'invitation':{'pairingId':vehicle,'secret':'0'*64,'expiresAtMs':1788567300000,'endpoint':'https://example.invalid','tlsPin':'0'*64,'pairingUri':'teslatlas-hub://pair?'+urllib.parse.urlencode({'endpoint':'https://example.invalid','pairing_id':vehicle,'secret':'0'*64,'tls_pin':'0'*64})}}
    units={'observed_at_ms':'epoch milliseconds','since':'epoch milliseconds','scheduled_charging_start_time':'epoch seconds',
        'speed':'km/h, rounded integer from provider mph','odometer':'km, provider miles converted and rounded to 0.01 km','ideal_battery_range_km':'km, provider miles converted and rounded to 0.01 km','est_battery_range_km':'km, provider miles converted and rounded to 0.01 km','rated_battery_range_km':'km, provider miles converted and rounded to 0.01 km',
        'active_route_miles_to_arrival':'miles, provider value unchanged','active_route_minutes_to_arrival':'minutes','active_route_traffic_minutes_delay':'minutes',
        'charge_energy_added':'kWh','charger_power':'kW','power':'kW','time_to_full_charge':'hours','elevation':'metres','charger_actual_current':'amperes','charger_voltage':'volts','charge_current_request':'amperes','charge_current_request_max':'amperes','charger_phases':'count, zero/nonpositive means unknown',
        'active_route_energy_at_arrival':'percent state of charge','heading':'degrees','center_display_state':'provider integer state code'}
    def current_unit(name,kind):
        if name in units:return units[name]
        if 'latitude' in name or 'longitude' in name:return 'degrees'
        if name in ('inside_temp','outside_temp'):return 'degrees Celsius'
        if name.startswith('tpms_pressure_'):return 'bar'
        if name in ('battery_level','usable_battery_level','charge_limit_soc','sun_roof_percent_open','download_perc','install_perc'):return 'percent'
        return 'boolean' if kind=='boolean' else 'text or provider enum'
    semantics={'profile_id':ID,'null_rule':'Null means unavailable, unknown, or not derivable. Preserve null and numeric zero distinctly; never infer a zero timestamp. Current owner/stream merge ignores null overlays. No freshness guarantee beyond observed_at_ms.',
        'source_trace':['Hub current_state build_current_vehicle_summary: provider fields, rounded mph/miles conversions, latest admitted epoch milliseconds','Hub public_query PublicDrive projection: direct materialised values','Hub runtime/lifecycle drive_and_charge: end-start milliseconds divided by 60000; current speed and odometer conversions feed drive aggregates','Hub import/teslamate projection: stored km, minutes, Celsius and kW passed through; efficiency is absent in live/import projections'],
        'current':{'vehicle_id':{'unit':'plain UUID','nullable':False},'car':{'unit':'optional materialised identity; source_eid/source_vid/id signed64, efficiency_wh_per_km Wh/km, settings durations minutes','nullable':True}}}
    for kind,names in CURRENT_GROUPS.items():
        for name in names.split():semantics['current'][name]={'unit':current_unit(name,kind),'nullable':True,'source':'Hub admitted observation/lifecycle/materialised identity'}
    semantics['drive']={}
    for name in drive['properties']:
        unit='text'
        if name=='vehicle_id':unit='plain UUID'
        elif name=='id':unit='signed64 identifier, preserve losslessly'
        elif name.endswith('_date_ms'):unit='epoch milliseconds'
        elif name.endswith('_km'):unit='km'
        elif name=='duration_min':unit='minutes, integer'
        elif name=='speed_max':unit='km/h, integer'
        elif name in ('ascent','descent'):unit='metres'
        elif name in ('start_soc','end_soc'):unit='percent'
        elif 'temp' in name:unit='degrees Celsius'
        elif 'power' in name:unit='kW'
        elif 'latitude' in name or 'longitude' in name:unit='degrees'
        elif name=='efficiency':unit='legacy projection numeric field, no current producer assigns a value; clients must preserve null, no unit claim is made'
        semantics['drive'][name]={'unit':unit,'nullable':name not in ('id','vehicle_id','start_date_ms','end_date_ms'),'source':'materialised projection; no public-query conversion'}
    cases=[{'id':kind+'-success','kind':kind,'method':method,'route':path,'status':200} for kind,method,path,_,_ in routes]
    cases += [{'id':code,'kind':'drives','method':'GET','route':'/v1/vehicles/{vehicle_id}/drives'+query,'status':400,'error_code':code} for code,query in [('invalid_limit','?limit=0'),('invalid_time_range','?from_ms=20&to_ms=10'),('invalid_query','?from=2026-09-01'),('invalid_cursor','?cursor={other_vehicle_cursor}')]]
    cases += [{'id':'drive-conditional','kind':'drives','status':304,'body':'empty'}, {'id':'pairing-replay','kind':'claim','status':401,'body':'empty'}, {'id':'missing-bearer','kind':'vehicles','status':401,'body':'empty'}, {'id':'bad-bearer','kind':'vehicles','status':401,'body':'empty'}, {'id':'rotated-bearer','kind':'vehicles','status':401,'body':'empty'}, {'id':'cursor-time-binding','kind':'drives','status':400,'error_code':'invalid_cursor'}, {'id':'later-current','kind':'current','status':200}, {'id':'scenario-page-2','kind':'drives','status':200}, {'id':'scenario-page-3','kind':'drives','status':200}, {'id':'current-empty','kind':'current','status':200}, {'id':'current-unknown','kind':'current','status':404,'body':'empty'}, {'id':'rotated-bearer-valid','kind':'vehicles','status':200}]
    sync={'profile_id':ID,'scope':'Existing sync transport remains separate from query profile schema validation. These regression gates remain mandatory for sync clients.',
        'routes':['GET /v1/vehicles/{vehicle_id}/sync/manifest','GET /v1/vehicles/{vehicle_id}/sync/noop','GET /v1/packs/sha256/{object_name}'],
        'schema_2_2':'Full snapshots only; explicit x-teslatlas-supported-schemas: 2.2 required; incompatible/missing negotiation returns 406. Do not request delta-v2 for schema 2.2.',
        'manifest':'Verify x-teslatlas-manifest-signature as Ed25519 over exact downloaded bytes using discovery manifestPublicKey before parsing/using object references. Preserve existing binding and sequence checks.',
        'pack':'Verify content-addressed SHA256, declared compressed length and decompression limits. Resume single byte Range using matching strong ETag/If-Range and validate Content-Range; never join incompatible objects.',
        'noop':'Preserve signed schema 2.2 no-op contract, exact manifest/object bindings, and hash verification; this is not an empty invented snapshot.',
        'regression_sources':['hub/tests/tls_import_e2e.rs','hub/src/api/server/tests.rs','hub/src/sync/updates_delivery.rs'],
        'acceptance_boundary':'Passing query conformance does not prove sync ingestion, schema-2.2 client correctness, migration preservation, or platform matrix completion.'}
    output={'profile.json':profile,'openapi.json':openapi,'discovery.schema.json':discovery,'resources.schema.json':resources,'auth.schema.json':auth,'errors.schema.json':errors,'field-semantics.json':semantics,'cases.json':{'profile_id':ID,'cases':cases},'sync-regression.json':sync}
    output.update({'examples/'+name+'.json':example for name,example in examples.items()})
    return {name:(json.dumps(value,ensure_ascii=False,sort_keys=True,indent=2)+'\n').encode() for name,value in output.items()}

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--check',action='store_true');args=parser.parse_args()
    files=bundle()
    files['SHA256SUMS']=''.join(hashlib.sha256(data).hexdigest()+'  '+name+'\n' for name,data in sorted(files.items())).encode()
    if args.check:
        wrong=[name for name,data in files.items() if not (ROOT/name).is_file() or (ROOT/name).read_bytes()!=data]
        if wrong:raise SystemExit('stale current-Hub artifacts: '+', '.join(wrong))
    else:
        for name,data in files.items():
            (ROOT/name).parent.mkdir(parents=True,exist_ok=True);(ROOT/name).write_bytes(data)
    print('hub-http-v1@1.0.0 sha256='+hashlib.sha256(files['SHA256SUMS']).hexdigest())

if __name__=='__main__':main()
