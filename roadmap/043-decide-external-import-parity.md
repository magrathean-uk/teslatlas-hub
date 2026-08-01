---
type: wayfinder:task
status: closed
parent: 000-map
---
# Decide external import parity

Blocked by: [Design charging-cost parity](042-design-charge-costs.md).

## Question

Must Hub support TeslaFi, tesla-apiscraper, CSV, or other TeslaMate import
formats in addition to direct TeslaMate migration?

## Starting recommendation

Keep direct TeslaMate migration mandatory and include other importers only when
a real Teslatlas migration path depends on them.

## Resolution

Direct read-only TeslaMate PostgreSQL is the only parity importer. TeslaFi CSV,
tesla-apiscraper, generic CSV, and opaque third-party exports are out of scope.
Any future importer needs a real migration path, isolated source identity,
immutable provenance, bounded parse, explicit timezone rules, and a golden
reconciliation corpus before publication.
