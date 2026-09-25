# Planning research and historical inputs

> Scope update 2026-09-12: Viewer development, planning and acceptance are excluded by owner. Existing Viewer paths and receipts below are preserved historical context, not active work.


Retrieved/reviewed 2026-09-08. Local source and declared support floors remain authoritative for this programme; the sources below inform specific implementation choices, not permission to change scope.

## Primary technical sources

- [TeslaMate Docker installation](https://docs.teslamate.org/docs/installation/docker/): its documented composition combines the collector, database, Grafana and MQTT. The relevant design lesson is an operator-facing composition that supplies selected runtime dependencies. Teslatlas should retain its own SQLite architecture and optional helpers rather than import TeslaMate's database/service requirements without need. The first Teslatlas milestone is the simple bootstrap; polished composition follows later.
- [Docker Compose profiles](https://docs.docker.com/compose/how-tos/profiles/): optional profiles provide a direct mechanism for selected services. Services without profiles can form the core. Use explicit dependencies and test the selected combination; configuration parsing does not prove runtime startup or recovery.
- [Apple Distribution XML reference](https://developer.apple.com/library/archive/documentation/DeveloperTools/Reference/DistributionDefinitionRef/Chapters/Distribution_XML_Ref.html): distribution packages support a choice tree, visibility/enabled state and initial selection. Use initial selection for user-controlled choices rather than repeatedly forcing their selection. The reference is archived; validate the emitted package on the real macOS 13 target before a compatibility claim.
- [Home Assistant installation](https://www.home-assistant.io/installation/) and [macOS installation](https://www.home-assistant.io/installation/macos/): official deployment choices are HA OS and Container; the macOS route uses a VM. The Teslatlas HA integration is not itself a native daemon. A Hub package choice must identify its existing HA destination or explicitly selected supported runtime.
- [Cargo minimum Rust version](https://doc.rust-lang.org/cargo/reference/rust-version.html) and [Cargo environment variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html): compiler minimums and output/home routing are separate controls. Preserve declared floors while pinning build tools and placing caches/output under the lab. Cargo documentation was also consulted through Context7.
- [Rustup environment variables](https://rust-lang.github.io/rustup/environment-variables.html): `RUSTUP_HOME` relocates toolchain storage. Existing pinned App and Hub toolchains were retained and checked after relocation.
- [Tart FAQ](https://tart.run/faq/): local runnable VMs and downloaded OCI images are distinct storage. Retain the pinned base needed for reproducible macOS 13 guests; use bounded, explicit cleanup. The installed Tart command's help confirms copy-on-write cloning and the option to disable automatic pruning.
- [Official OpenAI subagent guidance](https://learn.chatgpt.com/docs/agent-configuration/subagents?surface=app) and [model guidance](https://developers.openai.com/api/docs/guides/latest-model): delegate independent bounded work with explicit scope and concise returns; parallel agents add token cost and parallel writers can conflict. Luna is suitable for narrow repeatable tasks. The proposed model mix is a planning recommendation, not a price/performance guarantee.

Agent Reach/Firecrawl skills guided web retrieval; the callable Firecrawl connector supplied the sources. Context7 supplied Cargo documentation. The stale `teslatlas-cargo`, `teslatlas-core-nav`, `teslatlas-ffi` skill references and `rust-docs`/`codebase-memory-mcp` capabilities were not found in the currently available skill/tool inventory. No replacement skill, CodeGraph index or global tooling was installed just for planning.

## Archived development tasks consulted

| Archived task title | Task ID | Used for |
| --- | --- | --- |
| Teslatlas Hub | `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Hub milestones, actual runtime gaps and prior pause |
| Hub-Bits | `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Cross-product contract/installed-matrix history |
| Plan Hub Docker support | `01a07f73-4957-74f2-8e8c-94487e9ac1fa` | Core-only Docker candidate and missing runtime closure |
| Teslatlas Protocol | `01a07f89-48b6-7143-919f-8b389fbfb1ec` | Canonical profile and conformance boundaries |
| Teslatlas TypeScript SDK | `01a07f89-4bdf-7890-8a1b-4d88ad49f2a8` | Packed package/toolchain/consumer evidence |
| Teslatlas Swift SDK | `01a07f89-4e6b-76d0-ae93-cf958ce1becd` | Runtime transport corrections and remaining platform tests |
| Teslatlas Viewer | `01a07f89-5148-7463-b374-7b00232e082c` | CLI/browser/bootstrap path and live UI gaps |
| Teslatlas Home Assistant | `01a07f89-54c0-75c0-bcd2-5efc87b408bf` | Actual scheduler/UI/reauthentication proof gaps |
| Teslatlas Edge | `01a07f89-57a9-7831-9162-c1dede446ee9` | Durable forwarding, native/Docker/receiver gaps |

These old tasks remain archived. New task IDs and held objectives are in [TASKS.json](TASKS.json). Historical model clauses, blocked goals and prior completion claims do not control the new plan. The source baseline and retained original receipts must still match before reusing any old verification result.

## VM consolidation follow-up

The current access reference is [VM_ACCESS.md](VM_ACCESS.md). The old artifact-tree Tart tool disappeared during this follow-up, so [Tart 2.36.0](https://github.com/openai/tart/releases/tag/2.36.0) was downloaded from its official release and verified against the release asset SHA-256 before use in lab/tooling. CLI help from installed Lima 2.2.0 and Colima documented supported rename, SSH-port and profile/data retirement operations. VM preparation and key-based access were then verified locally; no product builds/tests were run.
