# Independence and interoperability

## Independent project

Teslatlas Hub is maintained by MAGRATHEAN UK LTD and is not an official
product of Tesla, of any vehicle manufacturer or of any software it can import
from.

It is built for new Teslatlas deployments and includes optional compatibility for operator-controlled legacy data. Compatibility is not the project's brand identity.

## What interoperability permits

Subject to applicable law and binding contracts, software may:

- implement a documented protocol;
- observe and test program behaviour;
- use facts, data formats and methods of operation;
- read data the operator is authorised to access;
- create a substitute implementation;
- use a third-party name truthfully to describe compatibility.

Copyright does not ordinarily protect ideas and methods of operation as such. It does protect original expression and may protect a substantial part even where literal copying is incomplete.

## What requires separate permission or analysis

Do not assume interoperability permits:

- copying source expression, tests, documentation or creative database selection;
- using logos or confusing product names;
- bypassing access control;
- breaching credentials or service terms;
- redistributing a third-party database;
- using confidential or leaked specifications;
- presenting a product as official;
- using a service provider's systems after access has been revoked.

## Importing from other software

Hub reads another program's database only to import the operator's own history.
The importer may use that database's table and column names, which are needed
to read it; it must not copy that program's code.

Hub names no other vehicle-data software in user-facing text, documentation,
its API, its command line, its app or new legal text, apart from some internal
file and format names that later releases replace. It recognises a database it
can import by the database's schema, not by a product name. Earlier published
records keep their wording.

The importer should use a database account that can only read; import never
changes the source database. Hub must not redistribute the other program or
claim support from its maintainers.

## Proprietary client

The proprietary Teslatlas app may communicate with the Hub through a stable public protocol. It must not copy or link GNU AGPL code unless Magrathean has a separate licence covering every relevant right.

Shared protocol facts should be maintained in a neutral specification. Shared source code must have a documented dual-licensing basis.

## Public statements

Use:

- "independent";
- "imports from a source database that Hub recognises by its schema";
- the independence statement in NOTICE.

Avoid:

- "official";
- the name of any software Hub imports from, alone or combined with ours;
- "compatible with" followed by another project's name;
- "authorised by" Tesla or by the makers of any software Hub imports from;
- "clean room" unless the evidence supports that precise claim;
- "100% compatible" without a defined and tested scope.
