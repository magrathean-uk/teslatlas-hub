# Contributing to Teslatlas Hub

## Before contributing

Discuss substantial architecture, protocol, storage, authentication, migration, API, licensing or branding changes before implementation. A maintainer may reject a contribution that creates security, legal, compatibility or support risk even if the code works.

## Licence and contributor agreement

By submitting a contribution, you certify the Developer Certificate of Origin below and agree that the contribution is licensed under GNU Affero General Public License version 3 only, together with applicable section 7 notices.

Before a non-trivial contribution is merged, the contributor must sign the applicable Individual or Corporate Contributor Licence Agreement. The CLA gives Magrathean UK Ltd sufficient rights to maintain, enforce and, where expressly covered, dual-license the contribution. It does not permit Magrathean to claim authorship falsely.

## Developer Certificate of Origin

By adding `Signed-off-by: Name <email>` to each commit, you certify that:

1. the contribution was created by you, or you have the right to submit it under the stated licence;
2. the contribution is based on earlier work that you reasonably believe is appropriately licensed and you have identified it;
3. you understand the contribution and sign-off are public and may be retained indefinitely;
4. you have not included confidential information, secrets, personal data or employer-owned material without authority.

Use:

```text
git commit -s
```

## Provenance declaration

Every pull request must state:

- whether code was written from scratch;
- every source, repository, document, model or implementation consulted;
- whether generated code or AI assistance was used;
- whether any code was copied, translated or adapted;
- applicable third-party licences;
- whether similar code exists in the proprietary Teslatlas repository;
- whether the contributor is acting within employment or contracting duties.

“Publicly visible” does not mean “free to copy”.

## TeslaMate compatibility work

A contribution based on TeslaMate must identify the exact upstream revision and paths. Preserve applicable upstream notices and mark modifications. Do not use TeslaMate names or logos as product branding.

## Security and privacy

Do not include live credentials, VINs, precise journeys, production databases or other personal data. Use synthetic fixtures. Security issues must be reported under `SECURITY.md`, not in a public pull request.

## Code requirements

A contribution must include:

- tests for success and failure paths;
- migration and rollback instructions where state changes;
- bounded resource use;
- least-privilege behaviour;
- stable error handling without secret leakage;
- documentation and release-note impact;
- SPDX/provenance classification;
- dependency and licence review for every new package.

## Acceptance

Submission does not guarantee review or acceptance. Maintainers may request restructuring, provenance evidence or a separate specification before code review.
