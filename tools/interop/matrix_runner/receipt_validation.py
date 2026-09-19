# SPDX-License-Identifier: AGPL-3.0-only
"""Closed semantic parsers used by final cohort validation.

Non-matrix gate facts use the closed producer form
``{"value": ..., "observation": {"path": ..., "sha256": ...}}``.  The
bound observation document has exact fields ``schema_version``, ``kind``,
``fact``, ``status``, ``inputs``, ``outputs`` and ``observed``; both binding
lists are reopened before a gate can be accepted.  This keeps package,
bootstrap, lifecycle, authentication and historical claims tied to producer
files rather than receipt-authored Boolean assertions.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import stat
from typing import Any, Mapping

from .adapter_wire import read_bound_file, strict_json

MAX_RECORD = 8_388_608
MAX_ARTIFACT = 1_073_741_824
SCHEMAS = {"matrix":"hub-compatibility-receipt-v2","package":"hub-package-receipt-v1","bootstrap":"hub-bootstrap-receipt-v2","upgrade":"hub-upgrade-receipt-v1","private-lane":"hub-private-lane-receipt-v1","historical":"historical-evidence-v1","review":"markdown-independent-review-v1"}
SCOPES = {"matrix":"local-tested-cohort-not-published","package":"local-built-not-published","bootstrap":"local-source-candidate","upgrade":"isolated-upgrade","private-lane":"authorized-private-lane","historical":"historical-local-evidence","review":"independent-review"}
ENVELOPES = {
 "package":(1,"hub-package-receipt",{"schema_version","kind","cohort_id","cohort_inputs","status","gates","artifacts"}),
 "bootstrap":(2,"hub-bootstrap-receipt",{"schema_version","kind","cohort_id","cohort_inputs","status","gates","source_snapshots","outputs"}),
 "upgrade":(1,"hub-upgrade-receipt",{"schema_version","kind","cohort_id","cohort_inputs","status","gates","source_version","target_version"}),
 "private-lane":(1,"hub-private-lane-receipt",{"schema_version","kind","cohort_id","cohort_inputs","status","gates","lane_receipts"}),
 "historical":(1,"historical-evidence",{"schema_version","kind","cohort_id","historical_tested_inputs","status","gates","receipts","attempts"}),
}
PLATFORMS={"macos_arm64","debian13_amd64","debian13_arm64","cohort","private","historical"}
CHECK_KINDS={
 "package":{"package-install"}, "bootstrap":{"bootstrap-source"},
 "upgrade":{"upgrade-store-comparison","upgrade-package","upgrade-lifecycle","upgrade-authentication"},
 "private-lane":{"private-lane"}, "historical":{"historical-receipt"},
}
EDGE_TABLES=["edge_accumulator_states","edge_applications","edge_consumer_diagnostics","edge_lineages","edge_pending_publications","edge_sequence_dispositions"]
FACT_FIELDS={
 "package-install":{"candidate_version","package_bytes_verified","install_layout_verified"},
 "bootstrap-source":{"candidate_version","source_snapshots_verified","outputs_verified"},
 "upgrade-package":{"candidate_version"},
 "upgrade-lifecycle":{"overinstall_completed"},
 "upgrade-authentication":{"preserved_credentials_reauthenticated"},
 "private-lane":{"authorization_verified","disposition"},
 "historical-receipt":{"receipt_bindings_verified","original_failure_preserved"},
}

# A successful non-matrix gate must carry a producer observation for every
# semantic fact.  The observation is deliberately a separate, hash-bound
# document: a Boolean copied into a receipt is not an observation of an
# installation, bootstrap, authentication, or historical operation.
PRODUCER_OBSERVATION_FIELDS={"schema_version","kind","fact","status","inputs","outputs","observed"}
SOURCE_ROLES_BY_ADAPTER={
 "protocol_actual_hub":{"hub_source","protocol_source"},
 "typescript_node":{"hub_source","protocol_source","typescript_sdk_source"},
 "typescript_browser":{"hub_source","protocol_source","typescript_sdk_source"},
 "swift":{"hub_source","protocol_source","swift_sdk_source"},
 "home_assistant":{"hub_source","protocol_source","home_assistant_source"},
 "edge_v2":{"hub_source","protocol_source","edge_source"},
}
ARTIFACT_ROLES_BY_ADAPTER={
 "protocol_actual_hub":{"hub_executable","protocol_fixture_seed"},
 "typescript_node":{"hub_executable","typescript_sdk_tarball"},
 "typescript_browser":{"hub_executable","typescript_sdk_tarball"},
 "swift":{"hub_executable","swift_sdk_product"},
 "home_assistant":{"hub_executable","home_assistant_integration_archive"},
 "edge_v2":{"hub_executable","edge_executable"},
}


class ReceiptValidationError(RuntimeError): pass


def _exact(value:Any, fields:set[str], label:str)->Mapping[str,Any]:
 if not isinstance(value,dict) or set(value)!=fields: raise ReceiptValidationError(label+" fields are invalid")
 return value


def _binding(value:Any,label:str,maximum:int=MAX_ARTIFACT)->Mapping[str,str]:
 if not isinstance(value,dict) or set(value)!={"path","sha256"} or not isinstance(value["path"],str) or not Path(value["path"]).is_absolute() or re.fullmatch(r"[0-9a-f]{64}",str(value["sha256"])) is None: raise ReceiptValidationError(label+" binding is invalid")
 path=Path(value["path"])
 try:
  if path.resolve()!=path: raise ReceiptValidationError(label+" path is not canonical")
  before=path.lstat()
  if not stat.S_ISREG(before.st_mode) or before.st_nlink!=1 or before.st_uid!=os.getuid() or before.st_mode&0o077 or before.st_size>maximum: raise ReceiptValidationError(label+" file boundary is invalid")
  digest=hashlib.sha256()
  fd=os.open(path,os.O_RDONLY|getattr(os,"O_NOFOLLOW",0))
  with os.fdopen(fd,"rb") as stream:
   opened=os.fstat(stream.fileno())
   if (opened.st_dev,opened.st_ino,opened.st_size,opened.st_mtime_ns)!=(before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns): raise ReceiptValidationError(label+" changed before hashing")
   for chunk in iter(lambda:stream.read(1024*1024),b""): digest.update(chunk)
   closed=os.fstat(stream.fileno())
   if (closed.st_dev,closed.st_ino,closed.st_size,closed.st_mtime_ns)!=(before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns): raise ReceiptValidationError(label+" changed while hashing")
  after=path.lstat()
 except OSError as error: raise ReceiptValidationError(label+" cannot be read") from error
 if (before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns)!=(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns) or digest.hexdigest()!=value["sha256"]: raise ReceiptValidationError(label+" changed or has the wrong digest")
 return value


def _document(binding:Any,label:str)->Any:
 try:return strict_json(read_bound_file(binding,label=label,maximum=MAX_RECORD))
 except Exception as error: raise ReceiptValidationError(label+" is not strict JSON") from error


def _jsonl(binding:Any,label:str)->list[Mapping[str,Any]]:
 try:raw=read_bound_file(binding,label=label,maximum=MAX_RECORD)
 except Exception as error: raise ReceiptValidationError(label+" cannot be read") from error
 rows=[]
 for line in raw.splitlines():
  if not line: raise ReceiptValidationError(label+" has an empty record")
  try:value=strict_json(line)
  except Exception as error: raise ReceiptValidationError(label+" is not strict JSONL") from error
  if not isinstance(value,dict): raise ReceiptValidationError(label+" record is not an object")
  rows.append(value)
 if not rows: raise ReceiptValidationError(label+" is empty")
 return rows


def _producer_observation(fact:Any, kind:str, expected:Any, label:str)->None:
 """Require a concrete producer document behind a semantic gate fact."""
 if not isinstance(fact,dict) or set(fact)!={"value","observation"}:
  raise ReceiptValidationError(label+" fact is not a producer observation")
 if fact["value"]!=expected:
  raise ReceiptValidationError(label+" fact value is not bound to its observation")
 observation=_document(fact["observation"],label+" producer observation")
 _exact(observation,PRODUCER_OBSERVATION_FIELDS,label+" producer observation")
 if (observation["schema_version"]!=1 or observation["kind"]!="producer-observation"
     or observation["fact"]!=label or observation["status"]!="passed"
     or observation["observed"]!=expected):
  raise ReceiptValidationError(label+" producer observation is not bound")
 inputs=_list(observation["inputs"],label+" producer inputs")
 outputs=_list(observation["outputs"],label+" producer outputs")
 # An observation must name both what was inspected and what the producer
 # emitted.  The documents are opened and hashed by _list/_binding above.
 if not inputs or not outputs:
  raise ReceiptValidationError(label+" producer observation is incomplete")


def _fact_value(facts:Mapping[str,Any], name:str)->Any:
 value=facts.get(name)
 if not isinstance(value,dict) or set(value)!={"value","observation"}:
  raise ReceiptValidationError(name+" fact is not a producer observation")
 return value["value"]


def _list(value:Any,label:str,*,documents=False)->list[Mapping[str,str]]:
 if not isinstance(value,list) or not value: raise ReceiptValidationError(label+" bindings are missing")
 for item in value: (_document if documents else _binding)(item,label)
 if len({item["path"] for item in value})!=len(value): raise ReceiptValidationError(label+" bindings are duplicated")
 return value


def _cohort(context:Mapping[str,Any])->tuple[Mapping[str,Any],set[tuple[str,str]]]:
 cohort=_document(context["cohort_inputs"],"selected cohort inputs")
 if not isinstance(cohort,dict) or cohort.get("schema_version")!=2 or cohort.get("cohort_id")!=context["cohort_id"]: raise ReceiptValidationError("selected cohort identity is invalid")
 outputs={(out.get("path"),out.get("sha256")) for build in cohort.get("builds",[]) if isinstance(build,dict) for out in build.get("outputs",[]) if isinstance(out,dict)}
 return cohort,outputs


def _command(value:Any,label:str)->None:
 _exact(value,{"argv","cwd","exit_code","outcome","log"},label+" command")
 if not isinstance(value["argv"],list) or not value["argv"] or not all(isinstance(v,str) and v for v in value["argv"]) or not isinstance(value["cwd"],str) or not Path(value["cwd"]).is_absolute() or value["exit_code"]!=0 or value["outcome"]!="passed": raise ReceiptValidationError(label+" command did not pass")
 _binding(value["log"],label+" command log",MAX_RECORD)
 record=_document(value["log"],label+" command log")
 _exact(record,{"schema_version","argv","cwd","exit_code","outcome"},label+" command log record")
 if record["schema_version"]!=1 or record["argv"]!=value["argv"] or record["cwd"]!=value["cwd"] or record["exit_code"]!=value["exit_code"] or record["outcome"]!=value["outcome"]:
  raise ReceiptValidationError(label+" command log is not bound to its command")


def _execution_log(value:Any,label:str,command:list[str],exit_code:int)->None:
 """Open and bind every row execution log to the row command and result."""
 _exact(value,{"stdout","stderr","command_record","duration_ms"},label)
 if not isinstance(command,list) or not command or not all(isinstance(item,str) and item for item in command):
  raise ReceiptValidationError(label+" command is invalid")
 if type(value["duration_ms"]) is not int or value["duration_ms"]<0:
  raise ReceiptValidationError(label+" duration is invalid")
 for stream_name in ("stdout","stderr"):
  stream=value[stream_name]
  if not isinstance(stream,dict) or set(stream)!={"path","sha256","bytes","truncated"}:
   raise ReceiptValidationError(label+" "+stream_name+" metadata is invalid")
  if type(stream["bytes"]) is not int or stream["bytes"]<0 or stream["bytes"]>MAX_RECORD or type(stream["truncated"]) is not bool:
   raise ReceiptValidationError(label+" "+stream_name+" metadata is invalid")
  _binding({"path":stream["path"],"sha256":stream["sha256"]},label+" "+stream_name,MAX_RECORD)
  actual=Path(stream["path"]).stat().st_size
  if actual!=stream["bytes"] or (stream["truncated"] and actual!=MAX_RECORD):
   raise ReceiptValidationError(label+" "+stream_name+" metadata is not bound")
 command_binding=value["command_record"]
 _binding(command_binding,label+" command record",MAX_RECORD)
 record=_document(command_binding,label+" command record")
 _exact(record,{"argv","cwd","started_at","ended_at","exit_code","outcome"},label+" command record")
 if (record["argv"]!=command or not isinstance(record["cwd"],str) or not Path(record["cwd"]).is_absolute()
     or type(record["exit_code"]) is not int or record["exit_code"]!=exit_code
     or not isinstance(record["started_at"],str) or not isinstance(record["ended_at"],str)
     or record["outcome"] not in {"exited_zero","passed"}):
  raise ReceiptValidationError(label+" command record is not bound to the row")


def _identity_sets(identity:Any,label:str,adapter:str,cohort:Mapping[str,Any])->None:
 """Require the complete source/artifact role set for a fixed adapter row."""
 if not isinstance(identity,dict) or set(identity)!={"sources","artifacts"}:
  raise ReceiptValidationError(label+" is incomplete")
 sources=identity["sources"];artifacts=identity["artifacts"]
 source_roles=[item.get("role") for item in sources if isinstance(item,dict)]
 artifact_roles=[item.get("role") for item in artifacts if isinstance(item,dict)]
 if (len(source_roles)!=len(sources) or len(set(source_roles))!=len(source_roles)
     or set(source_roles)!=SOURCE_ROLES_BY_ADAPTER.get(adapter,set())):
  raise ReceiptValidationError(label+" source identity set is incomplete")
 if (len(artifact_roles)!=len(artifacts) or len(set(artifact_roles))!=len(artifact_roles)
     or set(artifact_roles)!=ARTIFACT_ROLES_BY_ADAPTER.get(adapter,set())):
  raise ReceiptValidationError(label+" artifact identity set is incomplete")
 cohort_sources={item["source_identity"].get("role"):item["source_identity"]
                 for item in cohort.get("repository_observations",[])
                 if isinstance(item,dict) and isinstance(item.get("source_identity"),dict)}
 if any(cohort_sources.get(item["role"])!=item for item in sources):
  raise ReceiptValidationError(label+" source identity is outside the admitted row set")
 cohort_outputs=[item for build in cohort.get("builds",[]) if isinstance(build,dict)
                 for item in build.get("outputs",[]) if isinstance(item,dict)]
 for item in artifacts:
  if not any(output.get("role")==item.get("role") and output.get("path")==item.get("path")
             and output.get("sha256")==item.get("sha256")
             and output.get("embedded_version")==item.get("embedded_version") for output in cohort_outputs):
   raise ReceiptValidationError(label+" artifact identity is outside the admitted row set")


def _check(kind:str,binding:Any,platform:str,tested:Mapping[str,str],cohort_outputs:set[tuple[str,str]],gate_status:str)->None:
 value=_document(binding,kind+" check")
 if kind=="upgrade-store-comparison":
  _exact(value,{"status","scope","original_tables","added_empty_tables","before_schema","after_schema","paired_credentials_preserved","allowed_change","before_database_sha256","after_database_sha256","before_database","after_database","harness_sha256"},"upgrade comparator")
  _binding(value["before_database"],"upgrade comparator before database",MAX_ARTIFACT)
  _binding(value["after_database"],"upgrade comparator after database",MAX_ARTIFACT)
  hashes=(value["before_database_sha256"],value["after_database_sha256"],value["harness_sha256"])
  if (value["status"]!="passed" or value["scope"]!="offline row-content preservation comparison only" or value["original_tables"]!=60 or value["added_empty_tables"]!=EDGE_TABLES or value["before_schema"]!=57 or value["after_schema"]!=59 or value["paired_credentials_preserved"] is not True or value["allowed_change"]!="only nondecreasing paired-device last_authenticated_at_ms" or value["before_database_sha256"]==value["after_database_sha256"] or any(re.fullmatch(r"[0-9a-f]{64}",str(item)) is None for item in hashes) or value["before_database"]["sha256"]!=value["before_database_sha256"] or value["after_database"]["sha256"]!=value["after_database_sha256"]): raise ReceiptValidationError("upgrade comparator semantics are invalid")
  return
 required={"schema_version","kind","status","platform","cohort_inputs","artifacts","facts"}
 _exact(value,required,kind+" check")
 if value["schema_version"]!=1 or value["kind"]!=kind or value["status"]!=gate_status or value["platform"]!=platform or not isinstance(value["facts"],dict) or set(value["facts"])!=FACT_FIELDS[kind]: raise ReceiptValidationError(kind+" check disposition is invalid")
 for fact_name in FACT_FIELDS[kind]:
  fact=value["facts"].get(fact_name)
  _producer_observation(fact,kind,_fact_value(value["facts"],fact_name),kind+"."+fact_name)
 _binding(value["cohort_inputs"],kind+" cohort inputs",MAX_RECORD)
 if value["cohort_inputs"]!=tested: raise ReceiptValidationError(kind+" check has foreign tested inputs")
 artifacts=_list(value["artifacts"],kind+" artifact")
 if cohort_outputs and any((item["path"],item["sha256"]) not in cohort_outputs for item in artifacts): raise ReceiptValidationError(kind+" check uses an unadmitted artifact")
 if kind in {"package-install","bootstrap-source","upgrade-package"} and _fact_value(value["facts"],"candidate_version")!="2026.36.2": raise ReceiptValidationError(kind+" candidate version is invalid")
 if kind=="package-install" and (_fact_value(value["facts"],"package_bytes_verified") is not True or _fact_value(value["facts"],"install_layout_verified") is not True): raise ReceiptValidationError("package installation evidence is incomplete")
 if kind=="bootstrap-source" and (_fact_value(value["facts"],"source_snapshots_verified") is not True or _fact_value(value["facts"],"outputs_verified") is not True): raise ReceiptValidationError("bootstrap evidence is incomplete")
 if kind=="upgrade-lifecycle" and _fact_value(value["facts"],"overinstall_completed") is not True: raise ReceiptValidationError("upgrade lifecycle is incomplete")
 if kind=="upgrade-authentication" and _fact_value(value["facts"],"preserved_credentials_reauthenticated") is not True: raise ReceiptValidationError("upgrade authentication is incomplete")
 if kind=="private-lane" and (_fact_value(value["facts"],"authorization_verified") is not True or _fact_value(value["facts"],"disposition")!=gate_status): raise ReceiptValidationError("private lane disposition is invalid")
 if kind=="historical-receipt" and (_fact_value(value["facts"],"receipt_bindings_verified") is not True or _fact_value(value["facts"],"original_failure_preserved") is not True): raise ReceiptValidationError("historical evidence is incomplete")


def _gates(kind:str,value:Any,status:str,tested:Mapping[str,str],cohort_outputs:set[tuple[str,str]])->list[str]:
 if not isinstance(value,list) or not value: raise ReceiptValidationError("gate records are missing")
 ids=[]
 for gate in value:
  _exact(gate,{"id","platform","status","command","inputs","outputs","checks"},"gate")
  if not isinstance(gate["id"],str) or not gate["id"] or gate["platform"] not in PLATFORMS or gate["status"] not in {"passed","pending"}: raise ReceiptValidationError("gate identity is invalid")
  if gate["status"]=="passed": _command(gate["command"],gate["id"])
  elif gate["command"] is not None: raise ReceiptValidationError("pending gate claims a command pass")
  inputs=_list(gate["inputs"],"gate input")
  if tested not in inputs: raise ReceiptValidationError("gate omits tested inputs")
  outputs=_list(gate["outputs"],"gate output") if gate["outputs"] else []
  if cohort_outputs and any((item["path"],item["sha256"]) not in cohort_outputs for item in outputs): raise ReceiptValidationError("gate output is outside cohort")
  if not isinstance(gate["checks"],list) or not gate["checks"]: raise ReceiptValidationError("gate checks are missing")
  kinds=[]
  for item in gate["checks"]:
   _exact(item,{"kind","binding"},"gate check binding"); kinds.append(item["kind"]); _check(item["kind"],item["binding"],gate["platform"],tested,cohort_outputs,gate["status"])
   if item["kind"]=="upgrade-store-comparison":
    comparator=_document(item["binding"],"upgrade comparator")
    harnesses=[entry for entry in inputs if entry["sha256"]==comparator["harness_sha256"] and Path(entry["path"]).name=="compare-upgrade-stores.py"]
    if len(harnesses)!=1 or harnesses[0]["path"] not in gate["command"]["argv"]: raise ReceiptValidationError("upgrade comparator harness source is not command-bound")
    for field,label in (("before_database","before database"),("after_database","after database")):
     bound=comparator[field]
     if not any(entry["path"]==bound["path"] and entry["sha256"]==bound["sha256"] for entry in inputs): raise ReceiptValidationError("upgrade comparator "+label+" is not a gate input")
  if not CHECK_KINDS[kind].issubset(kinds): raise ReceiptValidationError(kind+" gate lacks semantic checks")
  ids.append(gate["id"])
 if len(ids)!=len(set(ids)): raise ReceiptValidationError("gate ids are duplicated")
 states={gate["status"] for gate in value}
 if status=="passed" and states!={"passed"}: raise ReceiptValidationError("passed receipt has incomplete gates")
 if status=="pending" and "pending" not in states: raise ReceiptValidationError("pending receipt has no pending gate")
 return ids


def _envelope(kind:str,raw:bytes,context:Mapping[str,Any],cohort_outputs:set[tuple[str,str]])->tuple[str,list[str],Any,Any]:
 try:value=strict_json(raw)
 except Exception as error: raise ReceiptValidationError(kind+" receipt is not strict JSON") from error
 version,discriminator,fields=ENVELOPES[kind];_exact(value,fields,kind+" receipt")
 if value["schema_version"]!=version or value["kind"]!=discriminator or value["cohort_id"]!=context["cohort_id"]: raise ReceiptValidationError(kind+" receipt identity is invalid")
 allowed={"passed","pending"} if kind=="private-lane" else {"passed"}
 if value["status"] not in allowed: raise ReceiptValidationError(kind+" receipt disposition is invalid")
 current=historical=None
 if kind=="historical":
  historical=_binding(value["historical_tested_inputs"],"historical tested inputs",MAX_RECORD)
  if historical==context["cohort_inputs"]: raise ReceiptValidationError("historical evidence relabels current inputs")
  _list(value["receipts"],"historical receipt",documents=True)
  if not isinstance(value["attempts"],list) or not value["attempts"]: raise ReceiptValidationError("historical attempts are missing")
  attempt_ids=[]; failed_ids=[]; passed_ids=[]
  for attempt in value["attempts"]:
   _exact(attempt,{"id","argv","cwd","exit_code","outcome","log","prior_failed_id"},"historical attempt")
   if (not isinstance(attempt["id"],str) or not attempt["id"] or attempt["id"] in attempt_ids or not isinstance(attempt["argv"],list) or not attempt["argv"] or not all(isinstance(v,str) and v for v in attempt["argv"]) or not isinstance(attempt["cwd"],str) or not Path(attempt["cwd"]).is_absolute() or attempt["outcome"] not in {"passed","failed"} or type(attempt["exit_code"]) is not int or (attempt["outcome"]=="passed" and attempt["exit_code"]!=0) or (attempt["outcome"]=="failed" and attempt["exit_code"]==0)):
    raise ReceiptValidationError("historical attempt is invalid")
   if attempt["prior_failed_id"] is not None and (attempt["prior_failed_id"] not in failed_ids or attempt["prior_failed_id"]==attempt["id"]): raise ReceiptValidationError("historical attempt lineage is invalid")
   attempt_ids.append(attempt["id"])
   (passed_ids if attempt["outcome"]=="passed" else failed_ids).append(attempt["id"])
   _binding(attempt["log"],"historical attempt log",MAX_RECORD)
   record=_document(attempt["log"],"historical attempt log")
   _exact(record,{"schema_version","argv","cwd","exit_code","outcome"},"historical attempt log record")
   if record["schema_version"]!=1 or record["argv"]!=attempt["argv"] or record["cwd"]!=attempt["cwd"] or record["exit_code"]!=attempt["exit_code"] or record["outcome"]!=attempt["outcome"]: raise ReceiptValidationError("historical attempt log is not bound")
  if value["status"]=="passed" and (not failed_ids or not passed_ids): raise ReceiptValidationError("historical evidence lacks failed-to-passed lineage")
  gates=_gates(kind,value["gates"],value["status"],historical,set())
 else:
  current=_binding(value["cohort_inputs"],"tested cohort inputs",MAX_RECORD)
  if current!=context["cohort_inputs"]: raise ReceiptValidationError(kind+" receipt has foreign inputs")
  gates=_gates(kind,value["gates"],value["status"],current,cohort_outputs)
 if kind=="package":
  if any((v["path"],v["sha256"]) not in cohort_outputs for v in _list(value["artifacts"],"package artifact")): raise ReceiptValidationError("package artifact is outside cohort")
 elif kind=="bootstrap":
  _list(value["source_snapshots"],"bootstrap snapshot",documents=True)
  if any((v["path"],v["sha256"]) not in cohort_outputs for v in _list(value["outputs"],"bootstrap output")): raise ReceiptValidationError("bootstrap output is outside cohort")
 elif kind=="upgrade":
  if value["source_version"]==value["target_version"] or value["target_version"]!="2026.36.2": raise ReceiptValidationError("upgrade transition is invalid")
 elif kind=="private-lane": _list(value["lane_receipts"],"private lane receipt",documents=True)
 return value["status"],gates,current,historical


def _supplement(binding:Any,cell:Mapping[str,Any])->None:
 value=_document(binding,"matrix supplement")
 try:
  from jsonschema import Draft202012Validator
  schema=json.loads(Path(__file__).with_name("supplement.schema.json").read_text())
 except Exception as error: raise ReceiptValidationError("supplement schema unavailable") from error
 if next(Draft202012Validator(schema).iter_errors(value),None): raise ReceiptValidationError("matrix supplement violates schema")
 if value["cell_id"]!=cell["cell_id"] or value["completion"]["status"]!="passed": raise ReceiptValidationError("matrix supplement completion is invalid")
 controller=value["controller_evidence"];completion=value["completion"]
 observations=_document(controller["observations"],"controller observations")
 _exact(observations,{"schema_version","session_id","observations"},"controller observations")
 if observations["schema_version"]!=1 or observations["session_id"]!=value["session_id"] or not isinstance(observations["observations"],list) or not observations["observations"]: raise ReceiptValidationError("controller observations identity is invalid")
 observation_fields={"schema_version","session_id","sequence","operation","state","started_monotonic_ns","finished_monotonic_ns","observed_at_ms","result_sha256","proof_sha256","scenario_sha256","seed_sha256","store_id","store_schema_version","hub_id","service_generation","invitations","transition"}
 def validate_observation(item):
  _exact(item,observation_fields,"controller observation")
  if item["schema_version"]!=1 or item["session_id"]!=value["session_id"] or item["operation"] not in {"verify","advance-once","pair","revoke","start"} or item["state"]!="running": raise ReceiptValidationError("controller observation identity is invalid")
  if type(item["sequence"]) is not int or item["sequence"]<=0 or type(item["started_monotonic_ns"]) is not int or item["started_monotonic_ns"]<0 or type(item["finished_monotonic_ns"]) is not int or item["finished_monotonic_ns"]<item["started_monotonic_ns"] or type(item["observed_at_ms"]) is not int or item["observed_at_ms"]<=0: raise ReceiptValidationError("controller observation timing is invalid")
  for key in ("result_sha256","proof_sha256","scenario_sha256","seed_sha256"):
   if not isinstance(item[key],str) or re.fullmatch(r"[0-9a-f]{64}",item[key]) is None: raise ReceiptValidationError("controller observation digest is invalid")
  if not isinstance(item["store_id"],str) or not item["store_id"] or type(item["store_schema_version"]) is not int or item["store_schema_version"]<=0 or not isinstance(item["hub_id"],str) or not item["hub_id"] or not isinstance(item["service_generation"],str) or not item["service_generation"]: raise ReceiptValidationError("controller observation provenance is invalid")
  if not isinstance(item["invitations"],dict) or set(item["invitations"])!={"active","expired"}: raise ReceiptValidationError("controller observation invitations are invalid")
  for invitation in item["invitations"].values():
   if not isinstance(invitation,dict) or set(invitation)!={"pairing_id","expires_at_ms"} or not isinstance(invitation["pairing_id"],str) or not invitation["pairing_id"] or type(invitation["expires_at_ms"]) is not int or invitation["expires_at_ms"]<=0: raise ReceiptValidationError("controller observation invitation is invalid")
  transition=item["transition"]
  expected_transition={"verify":None,"advance-once":{"kind","from_sequence","pre_advance_verify_sequence","before_store_sha256","after_store_sha256","scenario_sha256","seed_sha256"},"pair":{"kind","from_sequence"},"revoke":{"kind","from_sequence","device_id"},"start":{"kind","from_sequence","stopped_sequence"}}[item["operation"]]
  if expected_transition is None:
   if transition is not None: raise ReceiptValidationError("verify observation has a transition")
  elif not isinstance(transition,dict) or set(transition)!=expected_transition or transition.get("kind")!=item["operation"] or type(transition.get("from_sequence")) is not int or transition["from_sequence"]<=0:
   raise ReceiptValidationError("controller observation transition is invalid")
  elif item["operation"]=="advance-once" and any(re.fullmatch(r"[0-9a-f]{64}",str(transition.get(key))) is None for key in ("before_store_sha256","after_store_sha256","scenario_sha256","seed_sha256")):
   raise ReceiptValidationError("controller advance transition is invalid")
 sequences=[item.get("sequence") for item in observations["observations"] if isinstance(item,dict)]
 if len(sequences)!=len(observations["observations"]) or any(type(item) is not int or item<=0 for item in sequences) or sequences!=sorted(set(sequences)): raise ReceiptValidationError("controller observations are not a fresh ordered sequence")
 for item in observations["observations"]: validate_observation(item)
 journal=_jsonl(controller["journal"],"controller journal")
 if journal[0].get("kind")!="opened" or journal[0].get("session_id")!=value["session_id"] or journal[-1]!={"errors":[],"kind":"closed"}: raise ReceiptValidationError("controller journal lacks an exact clean lifecycle")
 # The controller's retained operation-record is the authoritative v2 shape.
 # Compact result rows may remain as ancillary journal history, but they can
 # never promote an observation or stand in for a bound request/result.
 operation_rows=[row for row in journal if row.get("kind")=="operation-record"]
 if not operation_rows: raise ReceiptValidationError("controller journal lacks retained operation records")
 for row in operation_rows:
  _exact(row,{"kind","operation","request","request_binding","processed_binding","status","failure","state","started_monotonic_ns","finished_monotonic_ns","observed_at_ms","result_binding","result_sha256","proof","invitation","expired_invitation","advance","events_sha256"},"controller operation record")
  if row["operation"] not in {"verify","stop","start","pair","revoke","advance-once"} or row["status"] not in {"admitted","failed"} or row["state"] not in {"running","stopped"}: raise ReceiptValidationError("controller operation record identity is invalid")
  request=row["request"]
  expected_request={"op"} if row["operation"]!="revoke" else {"op","device_id"}
  if not isinstance(request,dict) or set(request)!=expected_request or request.get("op")!=row["operation"] or (row["operation"]=="revoke" and (not isinstance(request.get("device_id"),str) or not request["device_id"])): raise ReceiptValidationError("controller operation request is invalid")
  if (type(row["started_monotonic_ns"]) is not int or row["started_monotonic_ns"]<0 or type(row["finished_monotonic_ns"]) is not int or row["finished_monotonic_ns"]<row["started_monotonic_ns"] or type(row["observed_at_ms"]) is not int or row["observed_at_ms"]<=0): raise ReceiptValidationError("controller operation timing is invalid")
  for key in ("request_binding","processed_binding"):
   binding=row[key]
   if binding is not None and (not isinstance(binding,dict) or set(binding)!={"sequence","challenge","op"} or type(binding["sequence"]) is not int or binding["sequence"]<=0 or not isinstance(binding["challenge"],str) or re.fullmatch(r"[0-9a-f]{64}",binding["challenge"]) is None or binding["op"]!=row["operation"]): raise ReceiptValidationError("controller operation binding is invalid")
  if row["status"]=="admitted":
   if row["processed_binding"] is None or not isinstance(row["result_sha256"],str) or re.fullmatch(r"[0-9a-f]{64}",row["result_sha256"]) is None or not isinstance(row["result_binding"],dict) or set(row["result_binding"])!={"name","sha256"} or re.fullmatch(r"[0-9a-f]{64}",str(row["result_binding"]["sha256"])) is None: raise ReceiptValidationError("admitted controller result is unbound")
   if row["processed_binding"]!=row["request_binding"] or row["result_binding"]["sha256"]!=row["result_sha256"] or Path(row["result_binding"]["name"]).name!=row["result_binding"]["name"]: raise ReceiptValidationError("admitted controller result binding is inconsistent")
   result_path=Path(controller["journal"]["path"]).resolve().parent/row["result_binding"]["name"]
   _binding({"path":str(result_path),"sha256":row["result_sha256"]},"controller retained result")
  elif row["processed_binding"] is not None or row["result_binding"] is not None or row["result_sha256"] is not None: raise ReceiptValidationError("failed controller operation retains a result")
  if row["state"]=="running" and not isinstance(row["proof"],dict): raise ReceiptValidationError("running controller operation lacks proof")
  if row["state"]=="stopped" and row["proof"] is not None: raise ReceiptValidationError("stopped controller operation has running proof")
  if not isinstance(row["events_sha256"],str) or re.fullmatch(r"[0-9a-f]{64}",row["events_sha256"]) is None: raise ReceiptValidationError("controller event digest is invalid")
 # Derive sequence and proof digest only from the retained operation records.
 def journal_sequence(row):
  if row.get("kind")=="operation-record":
   binding=row.get("processed_binding") or row.get("request_binding")
   return binding.get("sequence") if isinstance(binding,dict) else None
  return row.get("sequence")
 def journal_proof_hash(row):
  if isinstance(row.get("proof_sha256"),str): return row["proof_sha256"]
  proof=row.get("proof")
  if isinstance(proof,dict): return hashlib.sha256(json.dumps(proof,sort_keys=True,separators=(",",":")).encode()).hexdigest()
  return None
 results=[row for row in operation_rows if type(journal_sequence(row)) is int]
 admitted_sequences=[journal_sequence(row) for row in results if row.get("status")=="admitted"]
 if admitted_sequences!=sorted(set(admitted_sequences)): raise ReceiptValidationError("controller operation sequences are duplicated or unordered")
 if any(row.get("state")=="running" and journal_sequence(row) not in sequences for row in results if row.get("status")=="admitted"): raise ReceiptValidationError("controller operation record is outside observations")
 running_results=[row for row in results if row.get("state")=="running" and row.get("status","admitted")=="admitted"]
 if not results or not running_results or max(journal_sequence(row) for row in running_results)!=max(sequences): raise ReceiptValidationError("controller journal and observations disagree")
 result_hashes={}
 for row in running_results:
  digest=journal_proof_hash(row)
  if digest is not None: result_hashes[journal_sequence(row)]=digest
 if any(result_hashes.get(item["sequence"])!=item["proof_sha256"] for item in observations["observations"]): raise ReceiptValidationError("controller observation proofs do not match the journal")
 final_stopped=_document(controller["final_stopped"],"controller final stopped")
 if not isinstance(final_stopped,dict) or set(final_stopped)!={"schema_version","status","session_id","host_id","service","listener"} or final_stopped.get("schema_version")!=1 or final_stopped.get("status")!="stopped" or final_stopped.get("session_id")!=value["session_id"] or not isinstance(final_stopped.get("host_id"),str) or not final_stopped["host_id"]: raise ReceiptValidationError("controller final stopped proof identity is invalid")
 service=final_stopped.get("service"); listener=final_stopped.get("listener")
 if (not isinstance(service,dict) or set(service)!={"state","generation","forced_escalation","normal_exit","owned_generation","cleanup_errors"} or service.get("state")!="stopped" or service.get("generation") is not None or service.get("forced_escalation") is not False or service.get("normal_exit") is not True or service.get("cleanup_errors")!=[] or not isinstance(listener,dict) or set(listener)!={"host","port","owner_pid"} or listener.get("owner_pid") is not None or type(listener.get("port")) is not int): raise ReceiptValidationError("controller final stopped proof is invalid")
 owned=service.get("owned_generation"); stop=owned.get("stop_evidence") if isinstance(owned,dict) else None
 if not isinstance(owned,dict) or set(owned)!={"service","stop_evidence"} or not isinstance(stop,dict) or stop.get("operation")!="stop" or type(stop.get("operation_sequence")) is not int or stop["operation_sequence"]<=0: raise ReceiptValidationError("controller final stopped operation binding is invalid")
 stop_evidence=final_stopped["service"].get("owned_generation",{}).get("stop_evidence",{})
 stop_results=[row for row in results if row.get("operation")=="stop" and row.get("state")=="stopped" and row.get("status","admitted")=="admitted"]
 if stop_results:
  expected_stop=max(journal_sequence(row) for row in stop_results)
 else:
  last_result=max(running_results,key=journal_sequence);expected_stop=journal_sequence(last_result)+1
 if stop_evidence.get("operation_sequence")!=expected_stop: raise ReceiptValidationError("final stopped proof is not bound to the processed guest sequence")
 if cell["client_id"]=="home_assistant" and not any(row.get("operation")=="advance-once" and row.get("state")=="running" for row in results): raise ReceiptValidationError("HA supplement lacks its runner-owned advance barrier")
 transport=_document(controller["transport_cleanup"],"controller transport cleanup")
 _exact(transport,{"schema_version","session_id","status","resources"},"controller transport cleanup")
 if transport["schema_version"]!=1 or transport["session_id"]!=value["session_id"] or transport["status"]!="passed" or not isinstance(transport["resources"],list) or not transport["resources"]: raise ReceiptValidationError("controller transport cleanup is incomplete")
 adapter_completion=_document(completion["adapter_completion"],"completion adapter completion")
 ready=_document(completion["ready"],"completion ready");ack=_document(completion["ack"],"completion ack")
 normalized=_document(completion["normalized"],"completion normalized");actor_evidence=_document(completion["actor_evidence"],"completion actor evidence")
 command=_document(completion["command_outcome"],"completion command outcome")
 _exact(adapter_completion,{"schema_version","session_id","cell_id","session_input_sha256","normalized","actor_evidence"},"adapter completion")
 ready_fields={"schema_version","type","session_id","cell_id","session_input_sha256","instance_nonce","sequence","phase","observation","evidence"}
 ack_fields={"schema_version","type","session_id","cell_id","session_input_sha256","instance_nonce","sequence","ready_sha256","phase","status","action","result"}
 _exact(ready,ready_fields,"completion ready");_exact(ack,ack_fields,"completion ack")
 common=(value["session_id"],value["cell_id"])
 if (adapter_completion["schema_version"]!=1 or (adapter_completion["session_id"],adapter_completion["cell_id"])!=common
     or ready["schema_version"]!=1 or ready["type"]!="ready" or (ready["session_id"],ready["cell_id"])!=common or ready["sequence"]!=(2 if cell["client_id"]=="home_assistant" else 1) or ready["phase"]!="evidence_ready"
     or ready["evidence"]!=completion["adapter_completion"] or ready["observation"].get("session_sequence")!=max(sequences)
     or ready["observation"].get("proof_sha256")!=result_hashes[max(sequences)]
     or ack["schema_version"]!=1 or ack["type"]!="ack" or (ack["session_id"],ack["cell_id"])!=common or ack["sequence"]!=(2 if cell["client_id"]=="home_assistant" else 1)
     or ack["session_input_sha256"]!=ready["session_input_sha256"] or ack["instance_nonce"]!=ready["instance_nonce"] or ack["phase"]!="evidence_ready"
     or ack["ready_sha256"]!=completion["ready"]["sha256"] or ack["status"]!="accepted" or ack["action"]!="close_completed"):
  raise ReceiptValidationError("completion Ready and Ack are not the exact closed lifecycle")
 close_evidence=_document(ack["result"],"completion close evidence")
 _exact(close_evidence,{"schema_version","session_id","state","journal","final_stopped","cleanup_errors","local_transport"},"completion close evidence")
 if close_evidence["schema_version"]!=1 or close_evidence["session_id"]!=value["session_id"] or close_evidence["state"]!="closed" or close_evidence["cleanup_errors"]!=[] or close_evidence["journal"]!=controller["journal"] or close_evidence["final_stopped"]!=final_stopped: raise ReceiptValidationError("completion close evidence is invalid")
 if adapter_completion["normalized"]!=completion["normalized"] or adapter_completion["actor_evidence"]!=completion["actor_evidence"] or normalized.get("cell_id")!=value["cell_id"] or actor_evidence.get("cell_id")!=value["cell_id"] or actor_evidence.get("session_id")!=value["session_id"]: raise ReceiptValidationError("completion evidence bindings disagree")
 try:
  from jsonschema import Draft202012Validator
  actor_schema=json.loads(Path(__file__).with_name("actor_evidence.schema.json").read_text())
 except Exception as error: raise ReceiptValidationError("actor evidence schema unavailable") from error
 if next(Draft202012Validator(actor_schema).iter_errors(actor_evidence),None): raise ReceiptValidationError("completion actor evidence violates schema")
 _exact(command,{"schema_version","session_id","cell_id","exit_code","outcome","logs"},"completion command outcome")
 if command["schema_version"]!=1 or (command["session_id"],command["cell_id"])!=common or command["exit_code"]!=0 or command["outcome"]!="passed" or not isinstance(command["logs"],list): raise ReceiptValidationError("completion command did not pass")
 if not value["actors"] or len({a["id"] for a in value["actors"]})!=len(value["actors"]): raise ReceiptValidationError("supplement actors are invalid")
 raw_documents={}
 for actor in value["actors"]:
  runtime=_document(actor["runtime_evidence"],"actor runtime"); manifest=_document(actor["installed_manifest"],"actor manifest")
  if (not isinstance(runtime,dict) or runtime.get("schema_version")!=1 or not isinstance(runtime.get("runtime_ref"),str) or runtime.get("runtime_ref")!=actor["runtime_ref"] or not isinstance(runtime.get("runtime_kind"),str) or not runtime.get("runtime_kind") or not isinstance(runtime.get("identity_sha256"),str) or re.fullmatch(r"[0-9a-f]{64}",runtime["identity_sha256"]) is None): raise ReceiptValidationError("actor runtime is not independently bound")
  if not isinstance(manifest,dict) or set(manifest) not in ({"schema_version","artifact_sha256","files"},{"schema_version","build_record","files"}) or manifest.get("schema_version")!=1 or not isinstance(manifest.get("files"),list) or not manifest["files"]: raise ReceiptValidationError("actor manifest is not a closed member inventory")
  for member in manifest["files"]:
   if not isinstance(member,dict) or set(member)!={"path","bytes","mode","sha256"} or not isinstance(member["path"],str) or member["path"].startswith("/") or ".." in Path(member["path"]).parts or type(member["bytes"]) is not int or member["bytes"]<0 or type(member["mode"]) is not int or re.fullmatch(r"[0-9a-f]{64}",str(member["sha256"])) is None: raise ReceiptValidationError("actor manifest member is invalid")
  if [member["path"] for member in manifest["files"]]!=sorted({member["path"] for member in manifest["files"]}): raise ReceiptValidationError("actor manifest members are not sorted and unique")
  for raw in actor["raw_evidence"]:
   document=_document(raw["binding"],"actor raw evidence")
   if not isinstance(document,dict) or document.get("schema_version")!=1: raise ReceiptValidationError("actor raw evidence lacks its schema")
   raw_documents[(actor["id"],raw["id"])]=document
  if actor["id"] in {"swift_macos","swift_linux"}:
   phases=runtime.get("phase_admissions") if isinstance(runtime,dict) else None
   fields={"ordinal","phase_id","session_sequence_before","session_sequence_after","ready_sha256"}
   ready_bindings={raw["binding"]["sha256"]:raw["binding"] for raw in actor["raw_evidence"]}
   ready_hashes=set(ready_bindings)
   if not isinstance(phases,list) or len(phases)!=6 or [p.get("ordinal") for p in phases if isinstance(p,dict)]!=list(range(1,7)) or any(set(p)!=fields or p["session_sequence_before"] not in sequences or p["session_sequence_after"] not in sequences or p["session_sequence_after"]<=p["session_sequence_before"] or p["ready_sha256"] not in ready_hashes for p in phases): raise ReceiptValidationError("Swift actor runtime lacks six bound phase admissions")
   worker_fields=ready_fields|{"actor_id"}
   for phase in phases:
    worker_ready=_document(ready_bindings[phase["ready_sha256"]],"Swift retained WorkerReady")
    _exact(worker_ready,worker_fields,"Swift WorkerReady")
    if worker_ready["schema_version"]!=1 or worker_ready["type"]!="worker_ready" or worker_ready["session_id"]!=value["session_id"] or worker_ready["cell_id"]!=value["cell_id"] or worker_ready["actor_id"]!=actor["id"] or worker_ready["phase"]!=phase["phase_id"] or worker_ready["sequence"]!=phase["ordinal"] or worker_ready["observation"].get("session_sequence")!=phase["session_sequence_before"]: raise ReceiptValidationError("Swift phase admission is not bound to its retained WorkerReady")
 claims=[{key:item[key] for key in item if key!="runtime_evidence"} for item in value["actors"]]
 if claims!=actor_evidence["actors"]: raise ReceiptValidationError("supplement actor claims differ from admitted actor evidence")
 actor_ids={actor["id"] for actor in value["actors"]};raw_ids={actor["id"]:{raw["id"] for raw in actor["raw_evidence"]} for actor in value["actors"]}
 flattened=[]
 case_ids=[]
 for case in value["case_bindings"]:
  if case["case_id"] in case_ids: raise ReceiptValidationError("supplement case bindings are duplicated")
  case_ids.append(case["case_id"])
  if not set(case["actor_ids"]).issubset(actor_ids): raise ReceiptValidationError("case binding names an unknown actor")
  invocation_ids=set()
  for invocation in case["invocations"]:
   if invocation["id"] in invocation_ids: raise ReceiptValidationError("supplement invocations are duplicated")
   invocation_ids.add(invocation["id"])
   if invocation["case_id"]!=case["case_id"] or invocation["actor_id"] not in case["actor_ids"] or invocation["evidence_id"] not in raw_ids[invocation["actor_id"]] or invocation["session_sequence_before"] not in sequences or invocation["session_sequence_after"] not in sequences or invocation["session_sequence_after"]<=invocation["session_sequence_before"]: raise ReceiptValidationError("case invocation is not bound to actor evidence and controller observations")
   raw_document=raw_documents[(invocation["actor_id"],invocation["evidence_id"])]
   request_ids=raw_document.get("request_ids")
   if not isinstance(request_ids,list) or any(request_id not in request_ids for request_id in invocation["request_ids"]): raise ReceiptValidationError("case invocation request IDs are not bound to raw evidence")
   flattened.append(invocation)
 if flattened!=actor_evidence["invocations"]: raise ReceiptValidationError("supplement invocations differ from admitted actor evidence")
 if cell["client_id"]=="swift" and not {"swift_macos","swift_linux"}.issubset({a["id"] for a in value["actors"]}): raise ReceiptValidationError("Swift supplement lacks both worker actors")
 if {c["case_id"] for c in value["case_bindings"]}!={c["id"] for c in cell["case_results"]}: raise ReceiptValidationError("supplement case coverage is incomplete")


def _matrix(raw:bytes,context:Mapping[str,Any])->tuple[str,list[str]]:
 try:
  value=strict_json(raw);from jsonschema import Draft202012Validator
  schema_path=Path(__file__).resolve().parents[3]/"docs/compatibility/receipt.schema.json"
  schema=json.loads(schema_path.read_text())
 except Exception as error: raise ReceiptValidationError("matrix receipt cannot be parsed") from error
 if next(Draft202012Validator(schema).iter_errors(value),None): raise ReceiptValidationError("matrix receipt violates schema")
 authoritative_path=Path(__file__).resolve().parents[3]/"docs/compatibility/matrix.json"
 try:
  authoritative_raw=authoritative_path.read_bytes(); authoritative=strict_json(authoritative_raw)
 except Exception as error: raise ReceiptValidationError("authoritative matrix cannot be parsed") from error
 if (value.get("matrix_id")!=authoritative.get("matrix_id")
     or value.get("matrix_sha256")!=hashlib.sha256(authoritative_raw).hexdigest()
     or value.get("product_version")!=authoritative.get("product_version")
     or value.get("profile_id")!=authoritative.get("profile",{}).get("id")
     or value.get("profile_revision")!=authoritative.get("profile",{}).get("revision")
     or value.get("profile_sha256")!=authoritative.get("profile",{}).get("manifest_sha256")
     or value.get("cohort_requirements")!=authoritative.get("cohort_requirements")):
  raise ReceiptValidationError("matrix receipt is bound to a foreign matrix or profile")
 cells=value.get("cells",[]) if isinstance(value,dict) else []
 expected_cells={cell["id"]:cell for cell in authoritative.get("cells",[]) if isinstance(cell,dict)}
 cohort,_cohort_outputs=_cohort(context)
 if (value.get("schema_version")!=2 or value.get("execution_kind")!="actual_hub_acceptance"
     or value.get("status")!="passed" or value.get("complete") is not True
     or value.get("cohort_inputs")!=context["cohort_inputs"] or len(cells)!=len(expected_cells)
     or {c.get("cell_id") for c in cells if isinstance(c,dict)}!=set(expected_cells)
     or value.get("errors")!=[]):
  raise ReceiptValidationError("matrix is not a complete installed cohort")
 summary=value.get("summary")
 if summary!={"required_cells":len(expected_cells),"passed":len(expected_cells),"failed":0,"pending":0}:
  raise ReceiptValidationError("matrix summary does not describe the admitted cells")
 for cell in cells:
  expected=expected_cells[cell["cell_id"]]
  if (cell.get("status"),cell.get("client_id"),cell.get("adapter"),cell.get("hub_target"),cell.get("required")) != ("passed",expected.get("client_id"),expected.get("adapter"),expected.get("hub_target"),True):
   raise ReceiptValidationError("matrix cell identity is not authoritative")
  if (cell.get("identity_before") is None or cell.get("identity_after") is None
      or cell.get("identity_before")!=cell.get("identity_after")
      or cell.get("runtime_expected") is None or cell.get("runtime_actual") is None
      or cell.get("runtime_expected")!=cell.get("runtime_actual")
      or not isinstance(cell.get("execution_log"),dict)
      or type(cell.get("exit_code")) is not int or cell.get("exit_code")!=0
      or not isinstance(cell.get("evidence_path"),str)
      or not isinstance(cell.get("evidence_sha256"),str)):
   raise ReceiptValidationError("matrix cell lacks independently bound runtime and identity evidence")
  _identity_sets(cell["identity_before"],"matrix cell identity",expected.get("adapter"),cohort)
  _execution_log(cell["execution_log"],"matrix cell execution log",cell.get("command"),cell["exit_code"])
  evidence_binding={"path":cell["evidence_path"],"sha256":cell["evidence_sha256"]}
  _binding(evidence_binding,"matrix cell evidence",MAX_RECORD)
  for identity in (cell["identity_before"],cell["identity_after"]):
   sources=identity.get("sources") if isinstance(identity,dict) else None
   artifacts=identity.get("artifacts") if isinstance(identity,dict) else None
   if not isinstance(sources,list) or not isinstance(artifacts,list): raise ReceiptValidationError("matrix identity evidence is incomplete")
   cohort_sources={json.dumps(item.get("source_identity"),sort_keys=True) for item in cohort.get("repository_observations",[]) if isinstance(item,dict) and isinstance(item.get("source_identity"),dict)}
   cohort_artifacts={(item.get("role"),item.get("path"),item.get("sha256"),item.get("embedded_version")) for build in cohort.get("builds",[]) if isinstance(build,dict) for item in build.get("outputs",[]) if isinstance(item,dict)}
   if any(json.dumps(source,sort_keys=True) not in cohort_sources for source in sources): raise ReceiptValidationError("matrix source identity is outside the admitted cohort")
   if any((item.get("role"),item.get("path"),item.get("sha256"),item.get("embedded_version")) not in cohort_artifacts for item in artifacts if isinstance(item,dict)): raise ReceiptValidationError("matrix artifact identity is outside the admitted cohort")
  required_cases=authoritative.get("clients",{}).get(expected.get("client_id"),{}).get("required_cases",[])
  case_results=cell.get("case_results",[])
  if ([item.get("id") for item in case_results] != required_cases
      or any(item.get("status")!="passed" for item in case_results)
      or not isinstance(cell.get("supplement"),dict)):
   raise ReceiptValidationError("matrix cell cases are incomplete")
  _supplement(cell["supplement"],cell)
 return "passed",["matrix.complete",*sorted("matrix.cell."+c["cell_id"] for c in cells)]


def _review(raw:bytes,context:Mapping[str,Any])->list[str]:
 try:text=raw.decode()
 except UnicodeDecodeError as error:raise ReceiptValidationError("review is not UTF-8") from error
 binding=context["cohort_inputs"];gates=re.findall(r"(?m)^Gate: ([A-Za-z0-9][A-Za-z0-9._-]{0,127})$",text)
 if "Status: ACCEPTED" not in text or "C0/I0/M0" not in text or f"Cohort: {context['cohort_id']}" not in text or f"Cohort inputs: {binding['path']} SHA256 {binding['sha256']}" not in text or not gates or len(gates)!=len(set(gates)): raise ReceiptValidationError("review lacks an accepted cohort verdict")
 return gates


def validate(receipt_binding:Mapping[str,Any],raw_bytes:bytes,validation_context:Mapping[str,Any])->dict[str,Any]:
 _exact(receipt_binding,{"kind","scope","path","sha256"},"receipt binding");_exact(validation_context,{"cohort_id","cohort_inputs"},"validation context")
 kind=receipt_binding["kind"]
 if kind not in SCHEMAS or receipt_binding["scope"]!=SCOPES[kind]:raise ReceiptValidationError("receipt kind or scope is invalid")
 if not isinstance(raw_bytes,bytes) or len(raw_bytes)>MAX_RECORD or hashlib.sha256(raw_bytes).hexdigest()!=receipt_binding["sha256"]:raise ReceiptValidationError("receipt bytes do not match binding")
 _,outputs=_cohort(validation_context);current=validation_context["cohort_inputs"];historical=accepted=None
 if kind=="matrix":status,gates=_matrix(raw_bytes,validation_context)
 elif kind=="review":status,gates,accepted="passed",_review(raw_bytes,validation_context),True
 else:status,gates,current,historical=_envelope(kind,raw_bytes,validation_context,outputs)
 if kind=="historical":current=None
 return {"schema_version":2,"kind":kind,"path":receipt_binding["path"],"sha256":receipt_binding["sha256"],"cohort_id":validation_context["cohort_id"],"scope":receipt_binding["scope"],"status":status,"artifact_schema":SCHEMAS[kind],"gate_ids":gates,"accepted_review_verdict":accepted,"tested_cohort_inputs":current,"historical_tested_inputs":historical}

__all__=["ReceiptValidationError","validate"]
