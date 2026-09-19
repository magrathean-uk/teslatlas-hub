import hashlib
import json
from pathlib import Path
import ssl
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

    def test_certificate_der_normalization_accepts_runtime_bytes_and_hex_text_only(self):
        normalizer = getattr(installed_registry, "_certificate_der_bytes", None)
        self.assertIsNotNone(normalizer)
        if normalizer is None:
            return

        pem = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n"
        self.assertEqual(b"\x01\x02\x03", normalizer(ssl.PEM_cert_to_DER_cert(pem)))
        self.assertEqual(b"\x01\x02\x03", normalizer("010203"))
        for invalid in ("not-hex", bytearray(b"\x01\x02\x03"), None):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(
                installed_registry.InstalledRegistryError,
                "DER encoding is invalid",
            ):
                normalizer(invalid)

    def test_missing_reviewed_entry_is_pending_without_invoking_job_argv(self):
        with self.assertRaisesRegex(installed_registry.InstalledRegistryPending, "reviewed installed"):
            installed_registry.dispatch(
                {"adapter": "typescript_node"},
                {"client_id": "typescript_node"},
                {}, {}, registry=MappingProxyType({}),
            )

    def test_reviewed_entries_bind_exact_actor_contracts_and_topology(self):
        self.assertEqual(
            {"protocol_actual_hub", "typescript_node", "typescript_browser", "swift"},
            set(installed_registry.FIXED_INSTALLED_REGISTRY),
        )
        for adapter_id, actor_id, count in (
            ("protocol_actual_hub", "protocol_http", 21),
            ("typescript_node", "sdk_node", 21),
            ("typescript_browser", "sdk_browser", 23),
            ("swift", "swift_macos", 25),
        ):
            entry = installed_registry.FIXED_INSTALLED_REGISTRY[adapter_id]
            if adapter_id == "swift":
                self.assertEqual(("swift_macos", "swift_linux"), entry.required_actor_ids)
            else:
                self.assertEqual((actor_id,), entry.required_actor_ids)
            self.assertEqual(
                {"macos_arm64", "debian13_amd64", "debian13_arm64"},
                {target for target, _kind in entry.execution_by_target},
            )
            loaded = installed_registry.load_reviewed_contract(
                adapter_id, {adapter_id: entry.contract}, private=False,
            )
            self.assertEqual(count, len(loaded.manifest["required_cases"]))
            self.assertEqual(actor_id, loaded.manifest["actors"][0]["id"])
            self.assertEqual(
                {"build_session_input", "launch_adapter", "runtime_inventory",
                 "admit", "build_supplement", "execution_logs"},
                {
                    callback.__name__ for callback in (
                        entry.build_session_input, entry.launch_adapter,
                        entry.runtime_inventory, entry.admit,
                        entry.build_supplement, entry.execution_logs,
                    )
                },
            )
        protocol = installed_registry.FIXED_INSTALLED_REGISTRY["protocol_actual_hub"]
        self.assertEqual(
            (("macos_arm64", "local"), ("debian13_amd64", "local"),
             ("debian13_arm64", "local")),
            protocol.execution_by_target,
        )
        self.assertEqual(
            (("macos_arm64", "unix"), ("debian13_amd64", "unix"),
             ("debian13_arm64", "unix")),
            protocol.broker_kind_by_target,
        )
        self.assertEqual(
            {
                "build_session_input", "launch_adapter", "runtime_inventory",
                "admit", "build_supplement", "execution_logs",
            },
            {
                callback.__name__ for callback in (
                    protocol.build_session_input, protocol.launch_adapter,
                    protocol.runtime_inventory, protocol.admit,
                    protocol.build_supplement, protocol.execution_logs,
                )
            },
        )
        swift = installed_registry.FIXED_INSTALLED_REGISTRY["swift"]
        self.assertEqual(
            ("swift_macos", "swift_linux"), swift.required_actor_ids,
        )
        self.assertEqual(
            (('macos_arm64', 'local'), ('debian13_amd64', 'docker_exec_pipe'),
             ('debian13_arm64', 'docker_exec_pipe')),
            swift.execution_by_target,
        )
        self.assertEqual(
            (('macos_arm64', 'unix'), ('debian13_amd64', 'unix'),
             ('debian13_arm64', 'unix')),
            swift.broker_kind_by_target,
        )

    def test_protocol_contract_loads_as_public_reviewed_source_in_dispatch(self):
        entry = installed_registry.FIXED_INSTALLED_REGISTRY["protocol_actual_hub"]
        with mock.patch.object(
            installed_registry, "load_reviewed_contract",
            side_effect=RuntimeError("stop after contract selection"),
        ) as load:
            with self.assertRaisesRegex(RuntimeError, "stop after contract selection"):
                installed_registry.dispatch(
                    {"adapter": "protocol_actual_hub"},
                    {"client_id": "protocol_actual_hub"}, {}, {},
                )
        load.assert_called_once_with(
            "protocol_actual_hub", {"protocol_actual_hub": entry.contract},
            private=False,
        )

    def test_unreviewed_matrix_role_remains_pending(self):
        with self.assertRaisesRegex(installed_registry.InstalledRegistryPending, "reviewed installed"):
            installed_registry.dispatch(
                {"adapter": "viewer"}, {"client_id": "viewer"}, {}, {},
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
        def fixed_launch(*_args, session=None, running=None):
            calls.append(("launch", session, running))
            return object()

        entry = installed_registry.FixedInstalledAdapter(
            adapter_id="typescript_node", client_id="typescript_node",
            contract=ReviewedContract("/fixed/contract", "a" * 64, "/fixed/validator", "b" * 64),
            required_actor_ids=("sdk_node",),
            execution_by_target=(("macos_arm64", "local"),),
            build_session_input=lambda *args: calls.append("build") or ("/private/session", {}),
            launch_adapter=fixed_launch,
            admit=lambda *args: calls.append("admit"),
            build_supplement=lambda *args: calls.append("supplement") or supplement,
            execution_logs=lambda *args: calls.append("logs") or {"command_record": supplement},
            runtime_inventory=lambda *args: {"schema_version": 1, "runtime_ref": "test", "runtime_kind": "local", "identity_sha256": "c" * 64},
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
            kwargs["launch_adapter"](
                "/private/session", {}, session="root-session", running={"proof": {"sequence": 7}},
            )
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
        self.assertEqual(
            ["build", ("launch", "root-session", {"proof": {"sequence": 7}}), "admit", "supplement", "logs"], calls,
        )

    def test_fixed_entry_can_source_bind_a_broker_topology_override(self):
        """A docker worker cannot make SessionInput silently select stdio."""
        entry = installed_registry.FixedInstalledAdapter(
            adapter_id="swift", client_id="swift",
            contract=ReviewedContract("/fixed/contract", "a" * 64, "/fixed/validator", "b" * 64),
            required_actor_ids=("swift_macos", "swift_linux"),
            execution_by_target=(("debian13_amd64", "docker_exec_pipe"),),
            broker_kind_by_target=(("debian13_amd64", "unix"),),
            build_session_input=lambda *args: ("/private/session", {}),
            launch_adapter=lambda *args: object(), admit=lambda *args: None,
            build_supplement=lambda *args: {}, execution_logs=lambda *args: {},
            runtime_inventory=lambda *args: {},
        )
        self.assertEqual(
            (("debian13_amd64", "unix"),), entry.broker_kind_by_target,
        )


if __name__ == "__main__":
    unittest.main()
