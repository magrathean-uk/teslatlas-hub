# Hub local cleanup

On 2026-09-22 the owner requested removal of all local builds, candidates, runtime
fixtures, review artifacts and Teslatlas VMs after source publication. The Hub app
and services were stopped, LaunchAgents removed, ports 21443 and 21444 verified
unbound, and the external lab deleted.

The source-controlled review-surface cache under `.impeccable/` was also removed.
The accepted implementation remains in Git history and on `main`; no binary or
accepted runtime remains locally. A future run must rebuild and retest from source.
