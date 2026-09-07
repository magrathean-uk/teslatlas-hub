"""Host coordinator for the installed current-Hub Swift transport matrix."""

from __future__ import annotations

import hashlib
import json
import sys
import uuid
from pathlib import Path
from types import MappingProxyType

from matrix_contract import (
    ACTOR_IDS,
    CASE_KINDS,
    CASE_OPERATIONS,
    REQUIRED_CASES,
    AdmittedActor,
    AdmittedInvocation,
    AdmissionContext,
    admit_case,
    expected_normalized_facts,
)
from matrix_wire import (
    BrokerClient,
    CoordinationChannel,
    MatrixWireError,
    canonical_json_bytes,
    file_binding,
    load_session_input,
    read_bound_json,
    strict_json,
    validate_worker_config,
    write_exclusive_json,
)


LAUNCHER_INTERFACE = MappingProxyType({
    "call": "run_worker(actor_id, worker_config_binding, phase_callback)",
    "controller_observations": "controller_observations(session_id)",
    "outcome_keys": (
        "schema_version", "kind", "actor_id", "status", "framework_exit_code",
        "timed_out", "evidence", "log", "runtime", "cleanup",
    ),
    "worker_ready_keys": (
        "schema_version", "type", "session_id", "cell_id",
        "session_input_sha256", "instance_nonce", "sequence", "actor_id",
        "phase", "observation", "evidence",
    ),
})


def _proof_sha256(proof):
    return hashlib.sha256(canonical_json_bytes(proof)).hexdigest()


class GuestProofTracker:
    """Bind the first actual guest proof sequence and require advancement."""

    def __init__(self, session_id=None):
        self.session_id = session_id
        self.sequence = None
        self.observations = {}

    def observe(self, proof, operation="verify"):
        if not isinstance(proof, dict) or proof.get("status") != "verified":
            raise MatrixWireError("guest proof is not verified")
        session_id = proof.get("session_id")
        sequence = proof.get("sequence")
        if not isinstance(session_id, str) or type(sequence) is not int or sequence <= 0:
            raise MatrixWireError("guest proof identity is invalid")
        if self.session_id is None:
            self.session_id = session_id
        if session_id != self.session_id or (self.sequence is not None and sequence <= self.sequence):
            raise MatrixWireError("guest proof did not strictly advance")
        self.sequence = sequence
        self.observations[sequence] = MappingProxyType({
            "session_id": session_id,
            "operation": operation,
            "proof_sha256": _proof_sha256(proof),
            "hub_id": proof.get("discovery", {}).get("hub_id"),
            "service_generation": proof.get("service_generation"),
            "running": True,
        })
        return sequence

    def observe_stop(self, result):
        # The public stop result has no guest sequence.  Root obtains the actual
        # processed sequence from its private journal and supplies the final
        # immutable observation view; this local tracker must not invent one.
        if not isinstance(result, dict) or set(result) != {"stopped", "events"} or result["stopped"] is not True:
            raise MatrixWireError("stop result shape is invalid")


def _read_json_binding(binding, maximum=8_388_608):
    return read_bound_json(binding, maximum=maximum, label="worker evidence")


class MatrixCoordinator:
    """Sole broker owner; registered launchers never receive its descriptor."""

    def __init__(self, session, session_input_sha256, broker):
        self.session = session
        self.session_input_sha256 = session_input_sha256
        self.broker = broker
        self.proofs = GuestProofTracker(session["session_id"])
        self.raw = {}
        self.raw_bindings = {}
        self.worker_claims = {}

    def request(self, operation, device_id=None):
        result = self.broker.request(operation, device_id=device_id)
        if operation == "stop":
            self.proofs.observe_stop(result)
        else:
            self.proofs.observe(result["proof"], operation)
        return result

    def initial_verify(self):
        """Root verifies before attachment; this is the adapter's first own observation."""
        return self.request("verify")

    def apply_phase_recipe(self, actor_id, phase_id, private_witness=None):
        recipes = {
            "bootstrap_pair": ("verify", "pair"),
            "revoke_and_pair": ("revoke", "pair"),
            "restart": ("verify", "stop", "start"),
            "outage_stop": ("stop",),
            "outage_start": ("start",),
            "final_verify": ("verify",),
        }
        if actor_id not in ACTOR_IDS or phase_id not in recipes:
            raise MatrixWireError("worker phase is not admitted")
        results = []
        for operation in recipes[phase_id]:
            device_id = None
            if operation == "revoke":
                if not isinstance(private_witness, dict) or set(private_witness) != {"device_id"}:
                    raise MatrixWireError("revoke phase lacks private device identity")
                device_id = private_witness["device_id"]
            results.append({"operation": operation, "result": self.request(operation, device_id=device_id)})
        return results

    def admit_worker(self, actor_id, evidence_binding, launcher_cleanup):
        value = _read_json_binding(evidence_binding)
        required = {"schema_version", "session_id", "cell_id", "session_input_sha256", "actor_id", "raw", "cleanup"}
        if set(value) != required or value["schema_version"] != 1 or value["session_id"] != self.session["session_id"] or value["cell_id"] != self.session["cell_id"] or value["session_input_sha256"] != self.session_input_sha256 or value["actor_id"] != actor_id:
            raise MatrixWireError("worker evidence identity is invalid")
        if value["cleanup"] != {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True}:
            raise MatrixWireError("worker cleanup failed")
        if launcher_cleanup != {"process_exited": True, "stdout_closed": True, "stderr_closed": True}:
            raise MatrixWireError("launcher cleanup failed")
        if not isinstance(value["raw"], list) or len(value["raw"]) != len(REQUIRED_CASES):
            raise MatrixWireError("worker did not emit all Swift cases")
        seen = set()
        for row in value["raw"]:
            if not isinstance(row, dict) or row.get("case_id") not in REQUIRED_CASES or row["case_id"] in seen:
                raise MatrixWireError("worker case evidence is malformed")
            seen.add(row["case_id"])
            evidence_id = actor_id + "-" + row["operation"] + "-" + row["case_id"]
            raw_path = Path(self.session["outputs"]["coordination_dir"]) / "raw" / (evidence_id + ".json")
            raw_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            raw_value = {
                "schema_version": 1, "session_id": self.session["session_id"],
                "cell_id": self.session["cell_id"], "session_input_sha256": self.session_input_sha256,
                "actor_id": actor_id, "operation": row["operation"],
                "actor_manifest_sha256": row["actor_manifest_sha256"],
                "session_sequence_before": row["session_sequence_before"],
                "session_sequence_after": row["session_sequence_after"],
                "facts": row["facts"], "requests": row["requests"],
                "cleanup": {
                    "status": "passed", "transport_resources_closed": True,
                    "auxiliary_fixture_stopped": True, "process_exited": True,
                },
            }
            binding = write_exclusive_json(raw_path, raw_value)
            self.raw[evidence_id] = MappingProxyType(raw_value)
            self.raw_bindings[evidence_id] = binding
        self.worker_claims[actor_id] = evidence_binding

    def _actors(self, runtime_evidence):
        actors = {}
        for spec in self.session["actors"]:
            runtime = runtime_evidence[spec["id"]]
            actors[spec["id"]] = AdmittedActor(
                id=spec["id"], kind=spec["kind"], runtime_ref=spec["runtime_ref"],
                entrypoint_ref=spec["entrypoint_ref"], artifact_roles=tuple(spec["artifact_roles"]),
                source_roles=tuple(spec["source_roles"]),
                installed_manifest=MappingProxyType(dict(spec["input_manifest"]["local"])),
                runtime=MappingProxyType(dict(runtime)),
            )
        return actors

    def finalize(self, runtime_evidence, controller_observations):
        if set(self.worker_claims) != set(ACTOR_IDS) or self.proofs.sequence is None:
            raise MatrixWireError("both Swift workers must complete before finalization")
        self.request("verify")
        actors = self._actors(runtime_evidence)
        invocations = []
        for case_id in REQUIRED_CASES:
            operation = CASE_OPERATIONS[case_id][0]
            for actor_id in ACTOR_IDS:
                evidence_id = actor_id + "-" + operation + "-" + case_id
                raw = self.raw[evidence_id]
                invocations.append(AdmittedInvocation(
                    id="invoke-" + evidence_id, case_id=case_id, actor_id=actor_id,
                    operation=operation,
                    session_sequence_before=raw["session_sequence_before"],
                    session_sequence_after=raw["session_sequence_after"],
                    evidence_id=evidence_id,
                    request_ids=tuple(item["request_id"] for item in raw["requests"]),
                ))
        context = AdmissionContext(
            adapter_id="swift", cell_id=self.session["cell_id"], session_id=self.session["session_id"],
            header=MappingProxyType(self.session["_header"]), scenario=MappingProxyType(self.session["_scenario"]),
            actors=MappingProxyType(actors), invocations=tuple(invocations), raw=MappingProxyType(self.raw),
            controller_observations=MappingProxyType(dict(controller_observations)),
        )
        cases = []
        for case_id in REQUIRED_CASES:
            facts = expected_normalized_facts(case_id, context)
            requests = []
            for actor_id in ACTOR_IDS:
                requests.extend(self.raw[actor_id + "-" + CASE_OPERATIONS[case_id][0] + "-" + case_id]["requests"])
            case = {"id": case_id, "status": "pending" if case_id == "installed_service_runtime" else "passed", "expected": facts, "actual": facts, "evidence_kind": CASE_KINDS[case_id], "request_transcript": requests}
            decision = admit_case(case, context)
            required_status = "pending" if case_id == "installed_service_runtime" else "passed"
            if decision.status != required_status:
                raise MatrixWireError("Swift semantic admission rejected worker evidence: " + decision.code)
            cases.append(case)
        normalized = dict(self.session["_header"]); normalized["cases"] = cases
        normalized_binding = write_exclusive_json(self.session["outputs"]["normalized"], normalized)
        claims = []
        for actor_id in ACTOR_IDS:
            actor = actors[actor_id]
            relevant = [key for key, raw in self.raw.items() if raw["actor_id"] == actor_id]
            claims.append({"id": actor.id, "kind": actor.kind, "runtime_ref": actor.runtime_ref, "entrypoint_ref": actor.entrypoint_ref, "artifact_roles": list(actor.artifact_roles), "source_roles": list(actor.source_roles), "installed_manifest": dict(actor.installed_manifest), "raw_evidence": [{"id": key, "schema_id": "swift-worker-v1", "binding": self.raw_bindings[key]} for key in relevant]})
        actor_evidence = {"schema_version": 1, "session_id": self.session["session_id"], "cell_id": self.session["cell_id"], "session_input_sha256": self.session_input_sha256, "actors": claims, "invocations": [vars(item) for item in invocations]}
        actor_binding = write_exclusive_json(self.session["outputs"]["actor_evidence"], actor_evidence)
        completion = {"schema_version": 1, "session_id": self.session["session_id"], "cell_id": self.session["cell_id"], "session_input_sha256": self.session_input_sha256, "normalized": normalized_binding, "actor_evidence": actor_binding}
        completion_binding = write_exclusive_json(Path(self.session["outputs"]["coordination_dir"]) / "adapter-completion.json", completion)
        self.broker.assert_attached()
        observation = self.proofs.observations[self.proofs.sequence]
        CoordinationChannel(self.session, self.session_input_sha256).barrier(
            {"session_sequence": self.proofs.sequence, "proof_sha256": observation["proof_sha256"]},
            completion_binding,
        )


class WorkerPhaseController:
    """Closed callback handed to the trusted runner-owned launch capability."""

    def __init__(self, coordinator, actor_id, phase_contract):
        self.coordinator = coordinator
        self.actor_id = actor_id
        self.phases = phase_contract["phases"]
        self.next_ordinal = 1

    def __call__(self, ready_binding):
        ready = _read_json_binding(ready_binding, maximum=65_536)
        required = set(LAUNCHER_INTERFACE["worker_ready_keys"])
        if not isinstance(ready, dict) or set(ready) != required:
            raise MatrixWireError("worker ready shape is invalid")
        session = self.coordinator.session
        ordinal = self.next_ordinal
        if ordinal > len(self.phases):
            raise MatrixWireError("worker emitted an extra phase")
        phase = self.phases[ordinal - 1]
        if (
            ready["schema_version"] != 1
            or ready["type"] != "worker_ready"
            or ready["session_id"] != session["session_id"]
            or ready["cell_id"] != session["cell_id"]
            or ready["session_input_sha256"] != self.coordinator.session_input_sha256
            or ready["instance_nonce"] != session["instance_nonce"]
            or ready["sequence"] != ordinal
            or ready["actor_id"] != self.actor_id
            or ready["phase"] != phase["phase_id"]
        ):
            raise MatrixWireError("worker ready identity is invalid")
        observation = ready["observation"]
        if (
            not isinstance(observation, dict)
            or set(observation) != {"session_sequence", "proof_sha256"}
            or observation["session_sequence"] != self.coordinator.proofs.sequence
            or observation["proof_sha256"] != self.coordinator.proofs.observations[self.coordinator.proofs.sequence]["proof_sha256"]
        ):
            raise MatrixWireError("worker ready observation is stale")
        witness = _read_json_binding(ready["evidence"], maximum=65_536)
        expected_witness = {"schema_version", "session_id", "actor_id", "phase_id", "device_id"}
        if (
            set(witness) != expected_witness
            or witness["schema_version"] != 1
            or witness["session_id"] != session["session_id"]
            or witness["actor_id"] != self.actor_id
            or witness["phase_id"] != phase["phase_id"]
            or (phase["phase_id"] == "revoke_and_pair") != isinstance(witness["device_id"], str)
        ):
            raise MatrixWireError("worker phase witness is invalid")
        if isinstance(witness["device_id"], str):
            try:
                parsed_device = uuid.UUID(witness["device_id"])
            except ValueError as error:
                raise MatrixWireError("worker revoke device identity is invalid") from error
            if str(parsed_device) != witness["device_id"]:
                raise MatrixWireError("worker revoke device identity is invalid")
        private_witness = {"device_id": witness["device_id"]} if isinstance(witness["device_id"], str) else None
        results = self.coordinator.apply_phase_recipe(
            self.actor_id, phase["phase_id"], private_witness=private_witness
        )
        result_binding = write_exclusive_json(
            Path(session["outputs"]["coordination_dir"]) / self.actor_id /
            ("worker-result-%06d.json" % ordinal),
            {"schema_version": 1, "session_id": session["session_id"],
             "actor_id": self.actor_id, "phase_id": phase["phase_id"],
             "results": results},
        )
        self.next_ordinal += 1
        return {
            "schema_version": 1, "type": "worker_ack",
            "session_id": session["session_id"], "cell_id": session["cell_id"],
            "session_input_sha256": self.coordinator.session_input_sha256,
            "instance_nonce": session["instance_nonce"], "sequence": ordinal,
            "ready_sha256": ready_binding["sha256"],
            "actor_id": self.actor_id, "phase": phase["phase_id"],
            "status": "accepted", "action": "continue", "result": result_binding,
        }

    def assert_complete(self):
        if self.next_ordinal != len(self.phases) + 1:
            raise MatrixWireError("worker did not complete every fixed phase")


def _validate_launcher_outcome(actor_id, outcome, worker_config_binding):
    required = set(LAUNCHER_INTERFACE["outcome_keys"])
    if not isinstance(outcome, dict) or set(outcome) != required:
        raise MatrixWireError("launcher outcome shape is invalid")
    if (
        outcome["schema_version"] != 1
        or outcome["kind"] != "swift-worker-outcome"
        or outcome["actor_id"] != actor_id
        or outcome["status"] != "passed"
        or type(outcome["framework_exit_code"]) is not int
        or outcome["framework_exit_code"] != 0
        or outcome["timed_out"] is not False
        or outcome["cleanup"] != {"process_exited": True, "stdout_closed": True, "stderr_closed": True}
    ):
        raise MatrixWireError("launcher outcome is not successful")
    config = _read_json_binding(worker_config_binding, maximum=1_048_576)
    for name in ("evidence", "log"):
        binding = outcome[name]
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
            raise MatrixWireError("launcher outcome binding is invalid")
        maximum = 8_388_608
        if file_binding(binding["path"], maximum=maximum) != binding:
            raise MatrixWireError("launcher outcome binding changed")
        expected_path = config["evidence_path" if name == "evidence" else "log_path"]
        if binding["path"] != expected_path:
            raise MatrixWireError("launcher outcome path differs from worker config")
        if name == "evidence":
            _read_json_binding(binding)
    if not isinstance(outcome["runtime"], dict):
        raise MatrixWireError("launcher runtime evidence is invalid")
    return outcome


def _build_worker_config(session, digest, actor, initial_observation):
    actor_root = Path(session["outputs"]["coordination_dir"]) / actor["id"]
    actor_root.mkdir(mode=0o700, parents=True, exist_ok=False)
    common = session["inputs"]
    product = common["product_inputs"][0]
    initial_staged = {"id": "initial_observation", "root": initial_observation,
                      "local": initial_observation}
    inputs = [initial_staged, session["header"], common["profile_manifest"], *common["profile_members"], common["scenario"],
              common["certificate"], product["staged"], product["installed_manifest"],
              actor["input_manifest"]]
    value = {
        "schema_version": 1, "kind": "matrix-actor-worker",
        "actor_id": actor["id"], "session_id": session["session_id"],
        "cell_id": session["cell_id"], "instance_nonce": session["instance_nonce"],
        "session_input_sha256": digest, "phase_contract": actor["phase_contract"],
        "inputs": inputs, "private_root": str(actor_root / "private"),
        "coordination_dir": str(actor_root),
        "evidence_path": str(actor_root / "worker-evidence.json"),
        "log_path": str(actor_root / "worker.log"),
    }
    validate_worker_config(value)
    (actor_root / "private").mkdir(mode=0o700)
    return write_exclusive_json(actor_root / "worker-config.json", value)


def run_installed(session_input_path, launcher):
    """Run the installed row using only the trusted runner launch capability.

    `launcher` has exactly two exercised methods: `run_worker(actor_id,
    worker_config_binding, phase_callback)` and
    `controller_observations(session_id)`.  It owns executable/container/argv,
    environment, supervision and independent runtime inventory.  No such
    authority is accepted from SessionInput or a worker.  The callback accepts
    the exact retained WorkerReady FileBinding, never a reconstructed object.
    The launcher exclusively writes the returned canonical WorkerAck bytes to
    ``<coordination_dir>/worker-ack-%06d.json`` with mode 0600 and fsyncs the
    file before allowing the worker to resume.
    """
    session, digest = load_session_input(session_input_path)
    header_binding = session["header"]["local"]
    header = _read_json_binding(header_binding)
    scenario_binding = session["inputs"]["scenario"]["local"]
    scenario = _read_json_binding(scenario_binding)
    session["_header"] = header
    session["_scenario"] = scenario
    broker = BrokerClient.connect(session["broker"]["socket_path"], session["session_id"])
    coordinator = MatrixCoordinator(session, digest, broker)
    try:
        coordinator.initial_verify()
        runtimes = {}
        for actor in session["actors"]:
            actor_id = actor["id"]
            phase_contract = strict_json(Path(actor["phase_contract"]["local"]["path"]).read_bytes())
            callback = WorkerPhaseController(coordinator, actor_id, phase_contract)
            observation = coordinator.proofs.observations[coordinator.proofs.sequence]
            initial_binding = write_exclusive_json(
                Path(session["outputs"]["coordination_dir"]) /
                (actor_id + "-initial-observation.json"),
                {"session_sequence": coordinator.proofs.sequence,
                 "proof_sha256": observation["proof_sha256"]},
            )
            config_binding = _build_worker_config(session, digest, actor, initial_binding)
            outcome = _validate_launcher_outcome(
                actor_id, launcher.run_worker(actor_id, config_binding, callback),
                config_binding,
            )
            callback.assert_complete()
            coordinator.admit_worker(actor_id, outcome["evidence"], outcome["cleanup"])
            runtimes[actor_id] = outcome["runtime"]
        observations = launcher.controller_observations(session["session_id"])
        coordinator.finalize(runtimes, observations)
        return 0
    finally:
        broker.close()


def main(argv=None, launcher=None):
    """Imported fixed entrypoint used by the root-owned supervised wrapper."""
    arguments = list(sys.argv[1:] if argv is None else argv)
    if len(arguments) != 1 or launcher is None:
        return 2
    try:
        return run_installed(arguments[0], launcher)
    except BaseException:
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
