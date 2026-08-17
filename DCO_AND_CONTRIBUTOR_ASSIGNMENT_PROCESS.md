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

`Company Secretarial/IP/Contributors/Teslatlas-Hub/<year>/<account>/`

The public repository may record only:

- contributor account;
- agreement type;
- effective date;
- status (`verified`);
- internal register reference.

Do not publish legal names, home addresses, signatures or identity documents unless independently appropriate.

## Merge gate

A maintainer confirms:

- every commit has sign-off;
- contributor status is verified;
- provenance form is complete;
- dependency/licence checks pass;
- no proprietary app source was copied;
- no secret or personal data exists;
- branch is current and reviewed.

## Revocation

An accepted assignment and open-source grant are not revoked by later removal from the contributor list. Future access may be removed at any time.
