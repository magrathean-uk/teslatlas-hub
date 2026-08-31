# DCO and contributor-assignment process

## Who needs what

| Contributor | DCO | Separate assignment |
|---|---:|---:|
| Founder covered by confirmatory deed | Yes | No self-CLA |
| Company employee with signed IP terms | Yes | No, if register confirms coverage |
| Company contractor with signed assignment | Yes | No, if register confirms coverage |
| External individual | Yes | Individual assignment required |
| External organisation | Yes | Corporate assignment required |
| Dependency/upstream patch | Preserve upstream process | Provenance/licence review |

The contributor agreements are the
[individual assignment](individual-contributor-assignment-agreement.md) and
[corporate assignment](corporate-contributor-assignment-agreement.md). The
project's DCO policy and the canonical DCO 1.1 text are in
[Developer Certificate of Origin](developer-certificate-of-origin.md) and
[Developer Certificate of Origin 1.1](developer-certificate-of-origin-1.1.md).

## DCO

Command-line commits:

```sh
git commit -s
```

The sign-off name/email must identify the actual contributor.

Enable GitHub's **Require contributors to sign off on web-based commits** setting. This covers web commits only; local commits still require `--signoff`.

## Private agreement storage

Signed agreements must be stored in Company-controlled encrypted records, not GitHub.

Suggested logical path:

`Company Secretarial/IP/Contributors/teslatlas-hub/<year>/<account>/`

The public repository may record only:

- contributor account;
- agreement type;
- effective date;
- status (`verified`);
- internal register reference.

Do not publish legal names, home addresses, signatures or identity documents unless independently appropriate.

## Merge gate

A maintainer confirms for every new pull request or maintainer commit:

- every commit has sign-off;
- contributor status is verified;
- provenance form is complete;
- dependency/licence checks pass;
- no proprietary app source was copied;
- no secret or personal data exists;
- branch is current and reviewed.

## Historical baseline

The DCO policy was added after the first public alpha commit. The existing
pre-beta history contains maintainer commits without a `Signed-off-by` trailer.
Do not rewrite the published alpha tag or shared history to manufacture a
certification that was not recorded at commit time.

Before releasing a descendant, an authorised Company maintainer must review the
historical authorship set, confirm the applicable private employment,
assignment, contractor, or founder records, and record that release decision.
See [Historical contribution record](historical-contributions.md). This
administrative review does not replace third-party provenance or licence
analysis.

## Revocation

An accepted assignment and open-source grant are not revoked by later removal from the contributor list. Future access may be removed at any time.
