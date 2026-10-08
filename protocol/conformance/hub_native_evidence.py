"""Read-only native fixture evidence shared by the launcher and consumer.

Process identity uses the OS executable identity plus UID, parent, start time
and command. This verifies retained run evidence; it is not code signing or
protection against another process deliberately acting as the same OS owner.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys

REQUIRED={'binary_path','seed_binary_path','binary_sha256','seed_binary_sha256',
    'hub_pid','launcher_pid','hub_started_at','launcher_started_at','ready_path',
    'config_path','endpoint','status','provenance'}


def private_bytes(path, maximum=65536):
    try:
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
        with os.fdopen(fd,'rb') as stream:
            metadata=os.fstat(stream.fileno())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=os.getuid() or metadata.st_mode & 0o077:
                raise ValueError('private evidence file invalid')
            raw=stream.read(maximum+1)
        if len(raw)>maximum:raise ValueError('private evidence exceeds bound')
        return raw
    except OSError as error:raise ValueError('private evidence unavailable') from error


def process_identity(pid):
    if type(pid) is not int or pid<=0:raise ValueError('invalid fixture process identifier')
    try:
        result=subprocess.run(['ps','-ww','-p',str(pid),'-o','uid=,ppid=,lstart=,stat=,command='],
            stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,timeout=5,env={**os.environ,'LC_ALL':'C'})
        parts=result.stdout.strip().split(None,8)
        if result.returncode or len(parts)!=9 or 'Z' in parts[7]:raise ValueError('fixture process is not live')
        if int(parts[0])!=os.getuid():raise ValueError('fixture process owner mismatch')
        if sys.platform=='darwin':
            library=ctypes.CDLL('/usr/lib/libproc.dylib')
            library.proc_pidpath.argtypes=[ctypes.c_int,ctypes.c_void_p,ctypes.c_uint32]
            library.proc_pidpath.restype=ctypes.c_int
            buffer=ctypes.create_string_buffer(4096)
            if library.proc_pidpath(pid,buffer,len(buffer))<=0:raise ValueError('native executable identity unavailable')
            executable=os.fsdecode(buffer.value)
        elif sys.platform.startswith('linux'):
            executable=os.readlink('/proc/'+str(pid)+'/exe')
        else:raise ValueError('native process evidence unsupported on this platform')
        return {'pid':pid,'parent_pid':int(parts[1]),'started_at':' '.join(parts[2:7]),
            'command':parts[8],'executable':executable}
    except (OSError,subprocess.SubprocessError) as error:raise ValueError('fixture process evidence unavailable') from error


def staged_digest(path,expected):
    if not isinstance(path,str) or not Path(path).is_absolute():raise ValueError('absolute staged executable reference required')
    if not isinstance(expected,str) or re.fullmatch('[0-9a-f]{64}',expected) is None:
        raise ValueError('strict staged executable digest required')
    try:
        parent=Path(path).parent
        meta=parent.lstat()
        if not stat.S_ISDIR(meta.st_mode) or meta.st_uid!=os.getuid() or meta.st_mode & 0o077:
            raise ValueError('private staged executable directory required')
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
        with os.fdopen(fd,'rb') as stream:
            meta=os.fstat(stream.fileno())
            if not stat.S_ISREG(meta.st_mode) or meta.st_uid!=os.getuid() or stat.S_IMODE(meta.st_mode)!=0o500:
                raise ValueError('immutable private staged executable required')
            digest=hashlib.file_digest(stream,'sha256').hexdigest()
        if digest!=expected:raise ValueError('staged executable digest mismatch')
        return digest
    except OSError as error:raise ValueError('staged executable unavailable') from error


def verify(config):
    import tomllib
    if not isinstance(config,dict) or not REQUIRED<=config.keys():raise ValueError('missing native fixture prerequisites')
    for prefix in ('binary','seed_binary'):staged_digest(config[prefix+'_path'],config[prefix+'_sha256'])
    ready_path=config['ready_path']
    if not isinstance(ready_path,str) or not Path(ready_path).is_absolute():raise ValueError('absolute readiness witness required')
    root=Path(ready_path).parent
    try:
        meta=root.lstat()
        if not stat.S_ISDIR(meta.st_mode) or meta.st_uid!=os.getuid() or meta.st_mode & 0o077:
            raise ValueError('owned private fixture root required')
        if Path(ready_path).name!='ready.json' or (root/'stopped.json').exists():raise ValueError('fixture readiness is stale')
        witness=json.loads(private_bytes(ready_path))
        if not isinstance(witness,dict) or any(witness.get(key)!=config[key] for key in REQUIRED):
            raise ValueError('descriptor does not match retained readiness witness')
        # Bind all remaining supplied configuration references too, including
        # profile/scenario, invitation and update paths, to launcher output.
        if any(key not in witness or witness[key]!=value for key,value in config.items()):
            raise ValueError('descriptor differs from launcher readiness evidence')
        if config['status']!='ready' or config['provenance']!='synthetic-real-process':raise ValueError('live synthetic readiness required')
        hub=process_identity(config['hub_pid']);launcher=process_identity(config['launcher_pid'])
        if hub['parent_pid']!=config['launcher_pid']:raise ValueError('Hub is not owned by fixture launcher')
        if hub['started_at']!=config['hub_started_at'] or launcher['started_at']!=config['launcher_started_at']:
            raise ValueError('fixture process identity is stale')
        if Path(hub['executable']).resolve()!=Path(config['binary_path']).resolve():raise ValueError('running native executable mismatch')
        expected_command=' '.join([config['binary_path'],'--config',config['config_path'],'serve'])
        if hub['command']!=expected_command:raise ValueError('running Hub command mismatch')
        if Path(config['config_path'])!=root/'config.toml':raise ValueError('owned Hub configuration required')
        settings=tomllib.loads(private_bytes(config['config_path']).decode())
        if settings.get('data_dir')!=str(root/'hub') or settings.get('tls',{}).get('public_url')!=config['endpoint']:
            raise ValueError('endpoint does not match owned Hub configuration')
        if config['endpoint']!='https://'+settings.get('bind',''):
            raise ValueError('endpoint does not match owned listener')
        # Check process identity again after file reads to catch termination or
        # exec during preflight. Native network readiness is checked separately.
        if process_identity(config['hub_pid'])!=hub or process_identity(config['launcher_pid'])!=launcher:
            raise ValueError('fixture process changed during verification')
        return {'status':'verified','hub_pid':config['hub_pid'],'launcher_pid':config['launcher_pid'],
            'hub_started_at':hub['started_at'],'binary_sha256':config['binary_sha256'],
            'seed_binary_sha256':config['seed_binary_sha256']}
    except (OSError,KeyError,TypeError,json.JSONDecodeError) as error:raise ValueError('native fixture evidence invalid') from error
