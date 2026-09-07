# SPDX-License-Identifier: AGPL-3.0-only
"""Closed semantic parsers used by final cohort validation."""
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


def _check(kind:str,binding:Any,platform:str,tested:Mapping[str,str],cohort_outputs:set[tuple[str,str]],gate_status:str)->None:
 value=_document(binding,kind+" check")
 if kind=="upgrade-store-comparison":
  _exact(value,{"status","scope","original_tables","added_empty_tables","before_schema","after_schema","paired_credentials_preserved","allowed_change","before_database_sha256","after_database_sha256","harness_sha256"},"upgrade comparator")
  hashes=(value["before_database_sha256"],value["after_database_sha256"],value["harness_sha256"])
  if value["status"]!="passed" or value["scope"]!="offline row-content preservation comparison only" or value["original_tables"]!=60 or value["added_empty_tables"]!=EDGE_TABLES or value["before_schema"]!=57 or value["after_schema"]!=59 or value["paired_credentials_preserved"] is not True or value["allowed_change"]!="only nondecreasing paired-device last_authenticated_at_ms" or value["before_database_sha256"]==value["after_database_sha256"] or any(re.fullmatch(r"[0-9a-f]{64}",str(item)) is None for item in hashes): raise ReceiptValidationError("upgrade comparator semantics are invalid")
  return
 required={"schema_version","kind","status","platform","cohort_inputs","artifacts","facts"}
 _exact(value,required,kind+" check")
 if value["schema_version"]!=1 or value["kind"]!=kind or value["status"]!=gate_status or value["platform"]!=platform or not isinstance(value["facts"],dict) or set(value["facts"])!=FACT_FIELDS[kind]: raise ReceiptValidationError(kind+" check disposition is invalid")
 _binding(value["cohort_inputs"],kind+" cohort inputs",MAX_RECORD)
 if value["cohort_inputs"]!=tested: raise ReceiptValidationError(kind+" check has foreign tested inputs")
 artifacts=_list(value["artifacts"],kind+" artifact")
 if cohort_outputs and any((item["path"],item["sha256"]) not in cohort_outputs for item in artifacts): raise ReceiptValidationError(kind+" check uses an unadmitted artifact")
 if kind in {"package-install","bootstrap-source","upgrade-package"} and value["facts"].get("candidate_version")!="2026.36.2": raise ReceiptValidationError(kind+" candidate version is invalid")
 if kind=="package-install" and (value["facts"].get("package_bytes_verified") is not True or value["facts"].get("install_layout_verified") is not True): raise ReceiptValidationError("package installation evidence is incomplete")
 if kind=="bootstrap-source" and (value["facts"].get("source_snapshots_verified") is not True or value["facts"].get("outputs_verified") is not True): raise ReceiptValidationError("bootstrap evidence is incomplete")
 if kind=="upgrade-lifecycle" and value["facts"].get("overinstall_completed") is not True: raise ReceiptValidationError("upgrade lifecycle is incomplete")
 if kind=="upgrade-authentication" and value["facts"].get("preserved_credentials_reauthenticated") is not True: raise ReceiptValidationError("upgrade authentication is incomplete")
 if kind=="private-lane" and (value["facts"].get("authorization_verified") is not True or value["facts"].get("disposition")!=gate_status): raise ReceiptValidationError("private lane disposition is invalid")
 if kind=="historical-receipt" and (value["facts"].get("receipt_bindings_verified") is not True or value["facts"].get("original_failure_preserved") is not True): raise ReceiptValidationError("historical evidence is incomplete")


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
  for attempt in value["attempts"]:
   _exact(attempt,{"id","argv","exit_code","outcome","log"},"historical attempt")
   if attempt["outcome"] not in {"passed","failed"} or type(attempt["exit_code"]) is not int: raise ReceiptValidationError("historical attempt is invalid")
   _binding(attempt["log"],"historical attempt log",MAX_RECORD)
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
 sequences=[item.get("sequence") for item in observations["observations"] if isinstance(item,dict)]
 if len(sequences)!=len(observations["observations"]) or any(type(item) is not int or item<=0 for item in sequences) or sequences!=sorted(set(sequences)): raise ReceiptValidationError("controller observations are not a fresh ordered sequence")
 journal=_jsonl(controller["journal"],"controller journal")
 if journal[0].get("kind")!="opened" or journal[0].get("session_id")!=value["session_id"] or journal[-1]!={"errors":[],"kind":"closed"}: raise ReceiptValidationError("controller journal lacks an exact clean lifecycle")
 results=[row for row in journal if row.get("kind")=="result" and type(row.get("sequence")) is int]
 running_results=[row for row in results if row.get("state")=="running"]
 if not results or not running_results or max(row["sequence"] for row in running_results)!=max(sequences): raise ReceiptValidationError("controller journal and observations disagree")
 result_hashes={row["sequence"]:row.get("proof_sha256") for row in running_results}
 if any(result_hashes.get(item["sequence"])!=hashlib.sha256(json.dumps(item,sort_keys=True,separators=(",",":")).encode()).hexdigest() for item in observations["observations"]): raise ReceiptValidationError("controller observation bytes do not match the journal")
 final_stopped=_document(controller["final_stopped"],"controller final stopped")
 if (not isinstance(final_stopped,dict) or final_stopped.get("status")!="stopped" or final_stopped.get("session_id")!=value["session_id"]
     or not isinstance(final_stopped.get("service"),dict) or final_stopped["service"].get("state")!="stopped"
     or final_stopped["service"].get("cleanup_errors")!=[] or final_stopped.get("listener",{}).get("owner_pid") is not None): raise ReceiptValidationError("controller final stopped proof is invalid")
 stop_evidence=final_stopped["service"].get("owned_generation",{}).get("stop_evidence",{})
 last_result=max(results,key=lambda row:row["sequence"]);expected_stop=last_result["sequence"] if last_result.get("state")=="stopped" else last_result["sequence"]+1
 if stop_evidence.get("operation_sequence")!=expected_stop: raise ReceiptValidationError("final stopped proof is not bound to the processed guest sequence")
 transport=_document(controller["transport_cleanup"],"controller transport cleanup")
 _exact(transport,{"schema_version","session_id","status","resources"},"controller transport cleanup")
 if transport["schema_version"]!=1 or transport["session_id"]!=value["session_id"] or transport["status"]!="passed" or not isinstance(transport["resources"],list): raise ReceiptValidationError("controller transport cleanup is incomplete")
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
     or ready["schema_version"]!=1 or ready["type"]!="ready" or (ready["session_id"],ready["cell_id"])!=common or ready["sequence"]!=1 or ready["phase"]!="evidence_ready"
     or ready["evidence"]!=completion["adapter_completion"] or ready["observation"].get("session_sequence")!=max(sequences)
     or ready["observation"].get("proof_sha256")!=result_hashes[max(sequences)]
     or ack["schema_version"]!=1 or ack["type"]!="ack" or (ack["session_id"],ack["cell_id"])!=common or ack["sequence"]!=1
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
 for actor in value["actors"]:
  runtime=_document(actor["runtime_evidence"],"actor runtime");_document(actor["installed_manifest"],"actor manifest")
  for raw in actor["raw_evidence"]:_document(raw["binding"],"actor raw evidence")
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
 for case in value["case_bindings"]:
  if not set(case["actor_ids"]).issubset(actor_ids): raise ReceiptValidationError("case binding names an unknown actor")
  for invocation in case["invocations"]:
   if invocation["case_id"]!=case["case_id"] or invocation["actor_id"] not in case["actor_ids"] or invocation["evidence_id"] not in raw_ids[invocation["actor_id"]] or invocation["session_sequence_before"] not in sequences or invocation["session_sequence_after"] not in sequences or invocation["session_sequence_after"]<=invocation["session_sequence_before"]: raise ReceiptValidationError("case invocation is not bound to actor evidence and controller observations")
   flattened.append(invocation)
 if flattened!=actor_evidence["invocations"]: raise ReceiptValidationError("supplement invocations differ from admitted actor evidence")
 if cell["client_id"]=="swift" and not {"swift_macos","swift_linux"}.issubset({a["id"] for a in value["actors"]}): raise ReceiptValidationError("Swift supplement lacks both worker actors")
 if {c["case_id"] for c in value["case_bindings"]}!={c["id"] for c in cell["case_results"]}: raise ReceiptValidationError("supplement case coverage is incomplete")


def _matrix(raw:bytes,context:Mapping[str,Any])->tuple[str,list[str]]:
 try:
  value=strict_json(raw);from jsonschema import Draft202012Validator
  schema=json.loads((Path(__file__).resolve().parents[3]/"docs/compatibility/receipt.schema.json").read_text())
 except Exception as error: raise ReceiptValidationError("matrix receipt cannot be parsed") from error
 if next(Draft202012Validator(schema).iter_errors(value),None): raise ReceiptValidationError("matrix receipt violates schema")
 cells=value.get("cells",[]) if isinstance(value,dict) else []
 if value.get("schema_version")!=2 or value.get("execution_kind")!="actual_hub_acceptance" or value.get("status")!="passed" or value.get("complete") is not True or value.get("cohort_inputs")!=context["cohort_inputs"] or len(cells)!=21 or len({c.get("cell_id") for c in cells if isinstance(c,dict)})!=21 or any(c.get("status")!="passed" or not isinstance(c.get("supplement"),dict) for c in cells): raise ReceiptValidationError("matrix is not a complete installed cohort")
 for cell in cells:_supplement(cell["supplement"],cell)
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
