"""Test the private guest-to-native-Hub forward's SSH contract."""

from __future__ import annotations

import os
import stat
import subprocess
from pathlib import Path

ROOT = Path(__file__).parents[1]
SCRIPT = ROOT / "tools" / "mac-hub-forward"


def test_hub_forward_is_loopback_only_and_starts_no_guest_command(
    tmp_path: Path,
) -> None:
    """Build only the explicit guest-loopback to Mac-loopback reverse route."""
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    capture = tmp_path / "ssh-args"
    fake_ssh = fake_bin / "ssh"
    fake_ssh.write_text(
        '#!/bin/sh\nprintf \'%s\\n\' "$@" > "$TESLATLAS_TEST_CAPTURE"\n',
        encoding="utf-8",
    )
    fake_ssh.chmod(fake_ssh.stat().st_mode | stat.S_IXUSR)
    ssh_config = tmp_path / "ssh_config"
    ssh_config.write_text("Host teslatlas-debian13-arm64\n", encoding="utf-8")
    environment = os.environ.copy()
    environment.update(
        {
            "PATH": f"{fake_bin}:{environment['PATH']}",
            "TESLATLAS_TEST_CAPTURE": str(capture),
            "TESLATLAS_MAC_SSH_CONFIG": str(ssh_config),
            "TESLATLAS_MAC_SSH_ALIAS": "teslatlas-debian13-arm64",
            "TESLATLAS_MAC_HUB_PORT": "18443",
            "TESLATLAS_GUEST_HUB_PORT": "18444",
        }
    )

    result = subprocess.run(
        [str(SCRIPT)],
        cwd=ROOT,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    arguments = capture.read_text(encoding="utf-8").splitlines()
    assert "-N" in arguments
    assert "-F" in arguments
    assert arguments[arguments.index("-F") + 1] == str(ssh_config)
    assert "-R" in arguments
    assert arguments[arguments.index("-R") + 1] == ("127.0.0.1:18444:127.0.0.1:18443")
    assert arguments[-1] == "teslatlas-debian13-arm64"
