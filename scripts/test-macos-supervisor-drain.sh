#!/bin/sh
set -eu

# Disposable source-equivalent shutdown harness. It runs the shipped stop_child
# function unchanged, with short explicit test budgets, never installed services.
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
exec /usr/bin/python3 - "$ROOT/packaging/macos-service/scripts/run-hub-service.sh" <<'PY'
import os
from pathlib import Path
import signal
import select
import subprocess
import sys
import tempfile
import time

source = Path(sys.argv[1]).read_text()
stop_child = source[source.index('stop_child() {'):source.index('\nfinish() {')]
assert 'stop_child "$hub_pid" 250' in source
assert 'stop_child "$receiver_pid" 5' in source

with tempfile.TemporaryDirectory(prefix='teslatlas-supervisor-drain-', dir=os.environ['TMPDIR']) as tmp:
    root = Path(tmp)
    child = root / 'child.pl'
    child.write_text('''use Time::HiRes qw(sleep);
$| = 1;
my ($mode, $marker) = @ARGV;
$SIG{TERM} = $mode eq 'delayed' ? sub { sleep 3; open(my $out, '>', $marker) or die $!; print $out "drained"; close($out); exit 0; } : 'IGNORE';
print "READY $$\\n";
sleep 15;
''')
    wrapper = root / 'wrapper.sh'
    wrapper.write_text('set -eu\n' + stop_child + '''
hub_pid=
finish() {
    trap - EXIT HUP INT TERM
    stop_child "$hub_pid" "$1"
    exit 0
}
budget=$1
shift
/usr/bin/perl "$@" &
hub_pid=$!
trap 'finish "$budget"' EXIT HUP INT TERM
while kill -0 "$hub_pid" 2>/dev/null; do /bin/sleep 1; done
wait "$hub_pid"
''')
    for mode, budget in [('delayed', 8), ('noncooperating', 1)]:
        marker = root / (mode + '.drained')
        with subprocess.Popen(['/bin/sh', str(wrapper), str(budget), str(child), mode, str(marker)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as process:
            try:
                assert select.select([process.stdout], [], [], 3)[0], 'fixture readiness timed out'
                ready = process.stdout.readline().strip()
                assert ready.startswith('READY '), ready
                child_pid = int(ready.split()[1])
                started = time.monotonic()
                process.send_signal(signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=budget + 4)
                elapsed = time.monotonic() - started
                assert process.returncode == 0, (process.returncode, stderr)
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError(f'{mode} child remains after wrapper completion')
                if mode == 'delayed':
                    assert marker.read_text() == 'drained'
                    assert elapsed >= 3, elapsed
                    assert elapsed < budget, ("cooperating child waited until escalation deadline", elapsed)
                else:
                    assert not marker.exists()
                    assert elapsed < budget + 3, elapsed
                print(f'{mode}: owned child reaped; elapsed={elapsed:.3f}s; wrapper=0')
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    process.communicate(timeout=budget + 4)
print('macOS supervisor drain fixtures passed')
PY
