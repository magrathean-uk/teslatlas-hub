# SPDX-License-Identifier: AGPL-3.0-only
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from companions.core import BootstrapError  # noqa: E402
from companions.recipes import RecipeContext  # noqa: E402
from companions.targets import (  # noqa: E402
    apply_external_state,
    prepare_target_transition,
    validate_targets,
)


class TargetTests(unittest.TestCase):
    def test_edge_activation_only_records_a_prepared_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "prefix"
            output = (
                prefix / "releases" / "release-1" / "components" / "edge" / "output"
            )
            output.mkdir(parents=True)
            (output / "teslatlas-edge").write_bytes(b"fixture")
            (prefix / "active").symlink_to("releases/release-1")
            plan = prepare_target_transition(
                prefix,
                "release-1",
                ("edge",),
                {"edge_target": "local-linux"},
                None,
            )
            apply_external_state(plan["after"], prefix.resolve())
            actions = plan["actions"]
            self.assertEqual(actions["edge"]["status"], "prepared")
            self.assertFalse(actions["edge"]["service_started"])
            marker = json.loads(
                (
                    prefix / "releases" / "release-1" / "external-actions.json"
                ).read_text()
            )
            self.assertEqual(marker["actions"], actions)

    def test_edge_toolchain_bytes_are_part_of_the_target_binding(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            go = root / "go" / "bin" / "go"
            go.parent.mkdir(parents=True)
            go.write_bytes(b"go binary")
            go.chmod(0o755)
            binding = validate_targets(
                root / "prefix",
                ("edge",),
                RecipeContext(
                    edge_target="local-linux",
                    edge_go_binary=go,
                    edge_tool_root=root,
                ),
            )
            self.assertEqual(binding["edge_go_binary"], str(go))
            self.assertEqual(len(binding["edge_go_binary_sha256"]), 64)
            self.assertEqual(binding["edge_tool_root"], str(root))

    def test_ha_link_preserves_config_and_tracks_the_atomic_active_release(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            release = (
                prefix
                / "releases"
                / "release-1"
                / "components"
                / "home-assistant"
                / "output"
                / "custom_components"
                / "teslatlas_hub"
            )
            release.mkdir(parents=True)
            (release / "manifest.json").write_text("{}\n")
            (prefix / "active").symlink_to("releases/release-1")
            config = root / "ha-config"
            config.mkdir()
            registry = config / ".storage"
            registry.mkdir()
            (registry / "core.entity_registry").write_text("preserve\n")
            context = RecipeContext(ha_config=config)

            binding = validate_targets(prefix, ("home-assistant",), context)
            self.assertEqual(binding, {"ha_config": str(config.resolve())})
            plan = prepare_target_transition(
                prefix,
                "release-1",
                ("home-assistant",),
                binding,
                None,
            )
            apply_external_state(plan["after"], prefix.resolve())
            actions = plan["actions"]
            target = config / "custom_components" / "teslatlas_hub"
            self.assertTrue(target.is_symlink())
            self.assertEqual((target / "manifest.json").read_text(), "{}\n")
            self.assertTrue(actions["home-assistant"]["restart_required"])
            self.assertEqual(
                (registry / "core.entity_registry").read_text(), "preserve\n"
            )

    def test_ha_existing_directory_is_never_overwritten(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            config = root / "ha-config"
            target = config / "custom_components" / "teslatlas_hub"
            target.mkdir(parents=True)
            (target / "owned.py").write_text("keep\n")
            with self.assertRaisesRegex(BootstrapError, "will not be overwritten"):
                validate_targets(
                    prefix,
                    ("home-assistant",),
                    RecipeContext(ha_config=config),
                )
            self.assertEqual((target / "owned.py").read_text(), "keep\n")


if __name__ == "__main__":
    unittest.main()
