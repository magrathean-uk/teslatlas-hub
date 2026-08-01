# Teslatlas Hub

Teslatlas Hub is the backend context that records Tesla telemetry and makes
trusted vehicle history available to Teslatlas.

## Language

**Backend Parity**:
Behavioral and data equivalence with the supported TeslaMate backend reference.
_Avoid_: Mirror, clone

**Collector**:
The participant that obtains vehicle observations from an authorized Tesla source.
_Avoid_: Poller, scraper

**Observation**:
A source-stamped vehicle fact received by the **Collector**.
_Avoid_: Sample, response

**Vehicle History**:
The durable ordered record derived from **Observations** for one **Vehicle**.
_Avoid_: Telemetry dump

**Source Database**:
The TeslaMate database from which existing history is migrated.
_Avoid_: Old database, live database

**Destination Database**:
The Hub database that owns migrated and newly collected **Vehicle History**.
_Avoid_: Copy database, new database

**Credential Handoff**:
The protected transfer of an authorized Tesla credential into Hub custody.
_Avoid_: Token copy

**Read-only Migration**:
A migration that observes and copies from TeslaMate without changing its database, service, containers, configuration, or runtime state.
_Avoid_: In-place migration

**Verification Window**:
The bounded period in which Hub collection is proven before TeslaMate is stopped.
_Avoid_: Waiting period, soak

**Cutover**:
The operator-controlled transfer of collection ownership from TeslaMate to Hub.
_Avoid_: Kill TeslaMate, shutdown

**Parity Evidence**:
Recorded proof that Hub and the TeslaMate reference agree on a declared behavior.
_Avoid_: Looks correct, seems equivalent

## Relationships

- A **Collector** records many **Observations**
- Many **Observations** produce one **Vehicle History**
- A **Source Database** is read to populate one **Destination Database**
- A **Read-only Migration** leaves the **Source Database** and TeslaMate runtime unchanged
- A **Credential Handoff** precedes the **Verification Window**
- A successful **Verification Window** permits an operator-controlled **Cutover**
- **Parity Evidence** supports a **Backend Parity** claim

## Example dialogue

> **Dev:** "Can **Read-only Migration** stop TeslaMate after copying the data?"
> **Domain expert:** "No. The **Verification Window** must first prove that the
> **Collector** is extending **Vehicle History** in the **Destination Database**,
> and only the operator may perform **Cutover**."

## Flagged ambiguities

- "mirror TeslaMate" could mean source-code, schema, or behavior duplication;
  canonical usage is **Backend Parity**, with exact compatibility surfaces
  decided by the roadmap.
- "copy the token" could imply plaintext duplication; canonical usage is
  **Credential Handoff**, whose security mechanism remains a roadmap decision.
- "migration" does not authorize service or container changes; canonical usage
  is **Read-only Migration**.
