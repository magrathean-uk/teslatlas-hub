---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native ten-million direct migration

Blocked by: [Prove ten-million corpus load](108-prove-ten-million-corpus-load.md).

## Question

Does direct typed binary COPY, bounded pack production, and local verification
complete the representative corpus inside the ten-minute target?

## Starting recommendation

Add an explicitly opt-in native test with the representative row budget and a
ten-minute ceiling, then run it only against a disposable restored fixture.

## Resolution

The opt-in native direct-import test applies the representative source budget,
uses three metadata capture lanes, requires all 10,000,002 attached positions
and related projection facts, and rejects a result at or above ten minutes.
The disposable Apple-silicon native run completed inside that target. Its peak
resident memory was observed below 560 MiB while its main projection worker
remained CPU-bound; corpus setup time is recorded separately by ticket 108.
