---
type: wayfinder:task
status: closed
parent: 000-map
---
# Generate large position corpus

Blocked by: [Prove native complete direct import](106-prove-native-complete-direct-import.md).

## Question

How can the ten-million-row position case be reproduced locally without
storing a database image or connecting a generator to TeslaMate?

## Starting recommendation

Provide a checked, deterministic SQL emitter that writes only the bulk
position insert for an already restored Hub-owned corpus database.

## Resolution

`generate-large-position-corpus.sh` accepts a bounded decimal row count and
emits one deterministic PostgreSQL bulk insert to stdout. It opens no
connection and therefore cannot reach TeslaMate; the caller explicitly pipes
it only into a disposable, Hub-owned restored fixture.

A native PostgreSQL check generated 1,000 rows and found 1,002 attached
selected-car positions including the two base drive positions.
