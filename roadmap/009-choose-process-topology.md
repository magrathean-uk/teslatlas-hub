---
type: wayfinder:task
status: closed
parent: 000-map
---
# Choose the process topology

Blocked by: [Build the completion evidence matrix](008-build-completion-evidence-matrix.md).

## Question

Should collection, projection, storage, sync serving, migration, and repair run
inside one supervised process or as isolated workers?

## Starting recommendation

Use one installable service with strongly isolated supervised workers and
bounded queues unless fault-injection proves process isolation is required.

## Resolution

Use one always-on serving process and explicitly started isolated Hub jobs.
The service owns serving, readiness, and the SQLite store; compatibility
collection and TeslaMate import are manually started systemd oneshots with
least-credential access. They may fail independently without killing serving,
and SQLite transactions serialize shared durable state. No broker, worker
daemon, implicit collector, or automatic repair is introduced. An always-on
collector remains deferred until fault-injection proves no-loss replay and
truthful readiness under concurrent serving.
