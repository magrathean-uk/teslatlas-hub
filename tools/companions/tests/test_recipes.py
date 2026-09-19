# SPDX-License-Identifier: AGPL-3.0-only
from __future__ import annotations

import hashlib
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from companions.core import BootstrapError  # noqa: E402
from companions.recipes import (  # noqa: E402
    KNOWN_REPOSITORIES,
    SDK_TARBALL_SHA256,
    RecipeContext,
    _recipe_environment,
    _run,
    check_recipe_environment,
    fixed_commands,
    validate_component_set,
    validate_publication_status,
    verify_built_output,
)


def sdk_component(version: str = "2026.36.2", sha256: str = SDK_TARBALL_SHA256):
    return SimpleNamespace(
        product_version=version,
        artifacts={
            "package_filename": f"teslatlas-sdk-{version}.tgz",
            "package_sha256": sha256,
        },
    )


class RecipeTests(unittest.TestCase):
    def test_registry_contains_only_the_five_active_companion_repositories(self) -> None:
        self.assertEqual(
            KNOWN_REPOSITORIES,
            {
                "protocol": "https://github.com/magrathean-uk/teslatlas-protocol.git",
                "sdk-typescript": "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
                "sdk-swift": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
                "home-assistant": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
                "edge": "https://github.com/magrathean-uk/teslatlas-edge.git",
            },
        )

    @patch("companions.recipes.shutil.which", return_value=None)
    def test_missing_required_tool_fails_before_build(self, _which) -> None:
        with self.assertRaisesRegex(BootstrapError, "uv"):
            check_recipe_environment("protocol", RecipeContext())

    @patch("companions.recipes.platform.system", return_value="Darwin")
    def test_edge_rejects_a_mac_target(self, _system) -> None:
        with self.assertRaisesRegex(BootstrapError, "Linux"):
            check_recipe_environment("edge", RecipeContext(edge_target="local-linux"))

    @patch("companions.recipes.platform.system", return_value="Linux")
    @patch("companions.recipes._version")
    @patch("companions.recipes.shutil.which", side_effect=lambda name: f"/tools/{name}")
    def test_edge_accepts_only_a_content_bound_go_override_inside_its_tool_root(
        self, _which, version, _system
    ) -> None:
        version.side_effect = lambda command, _context: {
            "cargo": "cargo 1.98.0",
            "rustc": "rustc 1.98.0 (fixture)",
            "go": "go version go1.27.0 linux/arm64",
        }[Path(command[0]).name]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            go = root / "go" / "bin" / "go"
            go.parent.mkdir(parents=True)
            go.write_bytes(b"official go fixture")
            go.chmod(0o755)
            context = RecipeContext(
                edge_target="local-linux",
                edge_go_binary=go,
                edge_tool_root=root,
            )
            tools = check_recipe_environment("edge", context)
            self.assertEqual(
                tools["go_binary_sha256"],
                hashlib.sha256(go.read_bytes()).hexdigest(),
            )
            environment = _recipe_environment(context)
            self.assertEqual(environment["GO_BINARY"], str(go))
            self.assertEqual(environment["RUNNER_TOOL_CACHE"], str(root))

            outside = root.parent / "outside-go"
            outside.write_bytes(go.read_bytes())
            outside.chmod(0o755)
            with self.assertRaisesRegex(BootstrapError, "tool root"):
                check_recipe_environment(
                    "edge",
                    RecipeContext(
                        edge_target="local-linux",
                        edge_go_binary=outside,
                        edge_tool_root=root,
                    ),
                )

    def test_home_assistant_requires_an_explicit_config_environment(self) -> None:
        with self.assertRaisesRegex(BootstrapError, "HA config"):
            check_recipe_environment("home-assistant", RecipeContext())

    def test_node_bin_is_used_when_the_pinned_npm_launcher_resolves_node(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            node_bin = Path(temporary) / "node-bin"
            node_bin.mkdir()
            node = node_bin / "node"
            node.write_text(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo v26.7.0; else echo 11.19.0; fi\n",
                encoding="utf-8",
            )
            node.chmod(0o755)
            npm = node_bin / "npm"
            npm.write_text("#!/usr/bin/env node\n", encoding="utf-8")
            npm.chmod(0o755)

            self.assertEqual(
                check_recipe_environment(
                    "sdk-typescript", RecipeContext(node_bin=node_bin)
                ),
                {"node": "26.7.0", "npm": "11.19.0"},
            )

    def test_recipe_subprocess_timeout_is_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with (root / "command.log").open("wb") as log:
                with self.assertRaisesRegex(BootstrapError, "timed out"):
                    _run(
                        [sys.executable, "-c", "import time; time.sleep(30)"],
                        root,
                        log,
                        RecipeContext(timeout_seconds=1),
                    )

    def test_deferred_or_unknown_components_are_rejected(self) -> None:
        with self.assertRaisesRegex(BootstrapError, "unknown components"):
            validate_component_set(("deferred-product",))
        validate_component_set(KNOWN_REPOSITORIES)

    def test_source_publication_status_must_match_the_selected_mode(self) -> None:
        with self.assertRaisesRegex(BootstrapError, "candidate"):
            validate_publication_status("candidate", "published")
        with self.assertRaisesRegex(BootstrapError, "local-unpublished"):
            validate_publication_status("published", "local-unpublished")
        validate_publication_status("candidate", "local-unpublished")
        validate_publication_status("accepted", "local-unpublished")
        validate_publication_status("accepted", "published")
        validate_publication_status("published", "published")

    @patch("companions.recipes._tool", side_effect=lambda name, _context: name)
    def test_sdk_recipe_imports_the_newly_packed_artifact(self, _tool) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            output = root / "output"
            commands = fixed_commands(
                "sdk-typescript",
                root / "source",
                output,
                RecipeContext(),
                sdk_component(),
            )
            package = output / "teslatlas-sdk-2026.36.2.tgz"
            smoke = output / ".sdk-package-smoke"
            entrypoint = (
                smoke / "node_modules" / "@teslatlas" / "sdk" / "dist" / "index.js"
            ).as_uri()
            self.assertEqual(
                commands,
                [
                    ["npm", "ci"],
                    ["npm", "run", "build"],
                    ["npm", "pack", "--pack-destination", str(output)],
                    [
                        "npm",
                        "install",
                        "--prefix",
                        str(smoke),
                        "--ignore-scripts",
                        "--package-lock=false",
                        "--no-save",
                        str(package),
                    ],
                    [
                        "node",
                        "-e",
                        f"import({entrypoint!r})",
                    ],
                ],
            )

    @patch("companions.recipes.platform.machine", return_value="x86_64")
    @patch("companions.recipes.platform.system", return_value="Linux")
    @patch("companions.recipes._tool", side_effect=lambda name, _context: name)
    def test_edge_recipe_uses_locked_linux_bridge_target_and_output(
        self, _tool, _system, _machine
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            commands = fixed_commands(
                "edge",
                root / "source",
                root / "output",
                RecipeContext(edge_target="local-linux"),
            )
            self.assertEqual(commands[0], ["cargo", "build", "--locked", "--release"])
            self.assertEqual(
                commands[1],
                ["./target/release/teslatlas-edge", "--help"],
            )
            self.assertEqual(
                commands[2],
                [
                    "./scripts/build-fleet-telemetry-bridge.sh",
                    "--target",
                    "linux-amd64",
                    "--output",
                    str(root / "output" / "fleet-telemetry-bridge"),
                ],
            )


if __name__ == "__main__":
    unittest.main()
