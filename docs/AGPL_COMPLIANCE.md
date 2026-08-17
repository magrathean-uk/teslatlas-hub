# GNU AGPL compliance for maintainers and deployers

## Licence version

The combined Teslatlas Hub release is distributed under GNU Affero General Public License version 3 only. The canonical licence text must not be edited.

Earlier Magrathean-owned releases labelled “AGPL-3.0-or-later” remain available under the grant made for those releases. The next release should record the change to `AGPL-3.0-only` as a clarification and compatibility decision for the combined work; it does not revoke rights already granted.

## Source distributions

A source distribution must contain:

- the licence;
- applicable notices and section 7 terms;
- all source files;
- build and installation scripts;
- interface definitions;
- generated-source inputs;
- dependency metadata;
- modifications and dates;
- Corresponding Source for incorporated GPLv3 work.

## Object-code distribution

When distributing binaries, packages or images, provide equivalent access to the complete Corresponding Source at no additional charge. Put the source link next to the binary download and retain availability for the period required by the selected GNU AGPL section 6 method.

A link to an unrelated branch is not enough. Source must correspond to the exact binary.

## Network interaction

A modified version used to interact with users remotely must prominently offer those users the Corresponding Source of the version actually running, at no charge, through a standard copying method.

Recommended implementation:

- web/API discovery document containing `source_url`, version and commit;
- `/legal` endpoint returning copyright, no-warranty and licence information;
- `/source` redirect or response to an immutable source archive;
- CLI `legal` and `source` commands;
- GUI About/Legal panel where a GUI exists.

Authentication for ordinary application data must not prevent an interacting user from obtaining the source offer that GNU AGPL section 13 requires.

## Appropriate legal notices

Where an interactive interface provides menus or commands, include a prominent Legal/About item stating:

- copyright;
- no warranty;
- recipients may convey under GNU AGPL version 3;
- how to view the licence;
- how to obtain source;
- required section 7 attribution.

## Separate programs

A separate client communicating through an ordinary documented protocol is not automatically a derivative work. The analysis changes where programs share code, link into one process, exchange intimate internal structures, are distributed as one inseparable product or are designed as one combined work.

Maintain a clean boundary; do not rely on process separation as a magic defence.

## Additional terms

Only the categories allowed by GNU AGPL section 7 may be imposed. Do not add:

- non-commercial restrictions;
- bans on competitors or named organisations;
- mandatory marketing;
- royalties;
- use reporting;
- limits on migration or interoperability;
- restrictions based on field of use.

## Enforcement

Before alleging a violation:

1. preserve the exact release and evidence;
2. identify the copyright holder and covered material;
3. map each obligation to the licence text;
4. allow statutory cure where applicable;
5. avoid trade mark or contract claims disguised as copyright claims;
6. obtain counsel review before a platform takedown or litigation.
