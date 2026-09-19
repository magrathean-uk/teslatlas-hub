import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

from . import swift_entrypoint
from .installed import Deadline

SESSION = "11111111-1111-4111-8111-111111111111"


class SwiftEntrypointTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.session = self.root / "session.json"
        self.session.write_text("{}\n", encoding="utf-8")
        self.session.chmod(0o600)

    def tearDown(self):
        self.temporary.cleanup()

    def capability(self):
        return swift_entrypoint.SwiftLauncherCapability(
            workers={"swift_macos": lambda *_: {}, "swift_linux": lambda *_: {}},
            observations=lambda: {7: {"session_id": SESSION, "operation": "verify"}},
            session_id=SESSION, deadline=Deadline(5),
        )

    def test_wrapper_imports_only_hash_bound_module_and_passes_closed_capability(self):
        contract = self.root / "matrix_contract.py"
        contract.write_text("VALUE = 'contract'\n", encoding="utf-8")
        contract.chmod(0o600)
        wire = self.root / "matrix_wire.py"
        wire.write_text("VALUE = 'wire'\n", encoding="utf-8")
        wire.chmod(0o600)
        module = self.root / "matrix_live.py"
        module.write_text(
            "import matrix_contract, matrix_wire\n"
            "def run_installed(path, launcher):\n"
            " assert path.endswith('session.json')\n"
            " assert matrix_contract.VALUE == 'contract'\n"
            " assert matrix_wire.VALUE == 'wire'\n"
            f" assert launcher.controller_observations('{SESSION}') == {{7:{{'session_id':'{SESSION}','operation':'verify'}}}}\n"
            " return 0\n",
            encoding="utf-8",
        )
        module.chmod(0o600)
        reviewed = swift_entrypoint.ReviewedSwiftAdapter(
            path=str(module), sha256=hashlib.sha256(module.read_bytes()).hexdigest(),
            imports=(
                swift_entrypoint.ReviewedSwiftImport(
                    "matrix_contract", str(contract), hashlib.sha256(contract.read_bytes()).hexdigest()
                ),
                swift_entrypoint.ReviewedSwiftImport(
                    "matrix_wire", str(wire), hashlib.sha256(wire.read_bytes()).hexdigest()
                ),
            ),
        )
        capability = self.capability()
        self.assertEqual(0, swift_entrypoint.run(self.session, capability, reviewed))
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            capability.run_worker("swift_linux", {}, lambda value: value)
        module.write_text("changed\n", encoding="utf-8")
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            swift_entrypoint.run(self.session, capability, reviewed)

    def test_import_registry_is_exact_and_restored(self):
        module = self.root / "matrix_live.py"
        module.write_text("def run_installed(path, launcher): return 0\n", encoding="utf-8")
        module.chmod(0o600)
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            swift_entrypoint.run(
                self.session,
                self.capability(),
                swift_entrypoint.ReviewedSwiftAdapter(
                    str(module), hashlib.sha256(module.read_bytes()).hexdigest(), ()
                ),
            )
        self.assertNotIn("matrix_contract", sys.modules)
        self.assertNotIn("matrix_wire", sys.modules)

    def test_production_entrypoint_stays_pending_without_reviewed_registry(self):
        with self.assertRaises(swift_entrypoint.SwiftEntrypointPending):
            swift_entrypoint.run(self.session, self.capability(), None)


if __name__ == "__main__":
    unittest.main()
