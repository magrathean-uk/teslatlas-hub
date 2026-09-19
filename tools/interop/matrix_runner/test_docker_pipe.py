import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest

from . import docker_pipe


class DockerPipeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.socket_path = self.root / "broker.sock"
        self.stderr_path = self.root / "docker.stderr"

    def tearDown(self):
        self.temporary.cleanup()

    def _server(self):
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.socket_path))
        os.chmod(self.socket_path, 0o600)
        listener.listen(1)
        observed = []

        def serve():
            connection, _ = listener.accept()
            with connection:
                greeting = {"schema_version": 1, "type": "challenge", "sequence": 0}
                connection.sendall(json.dumps(greeting).encode() + b"\n")
                observed.append(connection.makefile("rb").readline())
                reply = {"schema_version": 1, "type": "reply", "sequence": 1}
                try:
                    connection.sendall(json.dumps(reply).encode() + b"\n")
                except BrokenPipeError:
                    pass
            listener.close()

        thread = threading.Thread(target=serve)
        thread.start()
        return thread, observed

    def test_exact_frames_relay_and_stderr_is_separate(self):
        thread, observed = self._server()
        code = (
            "import json,sys; g=json.loads(sys.stdin.readline()); "
            "print(json.dumps({'schema_version':1,'sequence':1,'op':'verify'}),flush=True); "
            "r=json.loads(sys.stdin.readline()); print('private log',file=sys.stderr); "
            "sys.exit(0 if g['sequence']==0 and r['sequence']==1 else 4)"
        )
        outcome = docker_pipe.run(
            socket_path=self.socket_path,
            argv=(sys.executable, "-c", code),
            cwd=self.root,
            environment={},
            stderr_path=self.stderr_path,
            timeout_seconds=5,
        )
        thread.join(2)
        self.assertEqual(0, outcome.exit_code)
        self.assertEqual("verify", json.loads(observed[0])["op"])
        self.assertEqual(b"private log\n", self.stderr_path.read_bytes())

    def test_extra_stdout_and_premature_eof_are_permanent_failures(self):
        for source, message in (
            ("import sys;sys.stdin.readline();print('not-json',flush=True)", "client stdout frame is invalid"),
            ("import sys;sys.stdin.readline();sys.exit(0)", "client stdout closed prematurely"),
        ):
            with self.subTest(source=source):
                thread, _observed = self._server()
                with self.assertRaisesRegex(docker_pipe.DockerPipeError, message):
                    docker_pipe.run(
                        socket_path=self.socket_path,
                        argv=(sys.executable, "-c", source),
                        cwd=self.root,
                        environment={},
                        stderr_path=self.stderr_path,
                        timeout_seconds=5,
                    )
                thread.join(2)
                self.socket_path.unlink(missing_ok=True)
                self.stderr_path.unlink(missing_ok=True)

    def test_stderr_is_drained_and_capped_without_blocking_the_client(self):
        thread, _observed = self._server()
        code = (
            "import json,sys; sys.stdin.readline(); "
            "sys.stderr.write('x'*(8*1024*1024+1)); sys.stderr.flush(); "
            "print(json.dumps({'schema_version':1,'sequence':1,'op':'verify'}),flush=True); "
            "sys.stdin.readline()"
        )
        with self.assertRaisesRegex(docker_pipe.DockerPipeError, "stderr exceeds bound"):
            docker_pipe.run(
                socket_path=self.socket_path,
                argv=(sys.executable, "-c", code),
                cwd=self.root,
                environment={},
                stderr_path=self.stderr_path,
                timeout_seconds=5,
            )
        thread.join(2)
        self.assertLessEqual(self.stderr_path.stat().st_size, docker_pipe.MAX_STDERR_BYTES)


if __name__ == "__main__":
    unittest.main()
