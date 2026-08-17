# Corresponding Source availability

## Release binaries

Every binary distribution of Teslatlas Hub must be accompanied by clear, equivalent access to the complete Corresponding Source for that exact binary at no additional charge. A generic link to the moving `main` branch is not sufficient when it does not reproduce the distributed object code.

Publish together:

- the exact source archive;
- the Git commit and signed tag;
- build and installation scripts;
- lockfiles and vendored source where used;
- interface-definition and generated-source inputs required to rebuild;
- dependency notices and an SBOM;
- checksums and signature-verification instructions;
- installation information required by GNU AGPL section 6 where applicable.

Keep source access available for as long as the corresponding object code is offered and for any longer period required by the selected GNU AGPL section 6 method.

## Network deployments

A modified version that permits remote network interaction must prominently offer every remote user an opportunity to obtain the Corresponding Source of the version actually running, free of charge, using a standard means of copying it.

The source endpoint or legal screen should identify:

- running version and commit;
- source archive URL;
- licence and additional terms;
- copyright and required attribution;
- modification notice where the operator changed the program.

Do not serve a stale upstream archive for a modified deployment.

## CLI and local UI

Interactive interfaces must preserve an appropriate legal-notice route. Recommended commands:

```text
teslatlas-hub legal
teslatlas-hub licence
teslatlas-hub source
```

A graphical controller should provide an **About / Legal / Source** view with equivalent information.
