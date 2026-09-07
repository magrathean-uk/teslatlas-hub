import hashlib
import json
from pathlib import Path
import tempfile
from types import MappingProxyType
import unittest
from unittest import mock

from . import installed_registry
from .adapter_wire import LoadedContract, ReviewedContract


class InstalledRegistryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self):
        self.temporary.cleanup()

    def binding(self, name, value):
        path = self.root / name
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        path.write_bytes(raw); path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def test_missing_reviewed_entry_is_pending_without_invoking_job_argv(self):
        with self.assertRaisesRegex(installed_registry.InstalledRegistryPending, "reviewed installed"):
            installed_registry.dispatch(
                {"adapter": "typescript_node"},
                {"client_id": "typescript_node"},
                {}, {}, registry=MappingProxyType({}),
            )

    def test_fixed_entry_loads_contract_and_uses_shared_installed_supervisor(self):
        session = self.binding("session.json", {"session": "fixed"})
        inventory = self.binding("inventory.json", {"registration": "fixed"})
        supplement = self.binding("supplement.json", {"schema_version": 2})
        calls = []
        contract = LoadedContract(
            manifest=MappingProxyType({
                "required_cases": ["case"],
                "actors": [{"id": "sdk_node", "kind": "packed_sdk_node", "required": True}],
            }),
            module=object(),
        )
        result = mock.Mock(exit_code=0)
        entry = installed_registry.FixedInstalledAdapter(
            adapter_id="typescript_node", client_id="typescript_node",
            contract=ReviewedContract("/fixed/contract", "a" * 64, "/fixed/validator", "b" * 64),
            required_actor_ids=("sdk_node",),
            execution_by_target=(("macos_arm64", "local"),),
            build_session_input=lambda *args: calls.append("build") or ("/private/session", {}),
            launch_adapter=lambda *args: calls.append("launch") or object(),
            admit=lambda *args: calls.append("admit"),
            build_supplement=lambda *args: calls.append("supplement") or supplement,
            execution_logs=lambda *args: calls.append("logs") or {"command_record": supplement},
        )
        job = {
            "adapter": "typescript_node",
            "installed_session": {"config": session, "registration_inventory": inventory},
            "client_execution": {"kind": "local"},
        }
        cell = {"id": "typescript_node__macos_arm64", "client_id": "typescript_node", "hub_target": "macos_arm64"}
        matrix = {"clients": {"typescript_node": {"required_cases": ["case"]}}}

        def execute(session_config, registration_inventory, **kwargs):
            self.assertEqual({"session": "fixed"}, session_config)
            self.assertEqual({"registration": "fixed"}, registration_inventory)
            kwargs["build_session_input"]({}, {})
            kwargs["launch_adapter"]("/private/session", {})
            kwargs["admit"]({}, {})
            return result

        with mock.patch.object(installed_registry, "load_reviewed_contract", return_value=contract), \
                mock.patch.object(installed_registry, "_session_input", side_effect=lambda path, value, *_: (path, value)), \
                mock.patch.object(installed_registry.installed, "execute_installed", side_effect=execute):
            dispatched = installed_registry.dispatch(
                job, cell, {}, matrix,
                registry=MappingProxyType({"typescript_node": entry}),
            )
        self.assertIs(result, dispatched.result)
        self.assertEqual(supplement, dispatched.supplement)
        self.assertEqual(["build", "launch", "admit", "supplement", "logs"], calls)


if __name__ == "__main__":
    unittest.main()
