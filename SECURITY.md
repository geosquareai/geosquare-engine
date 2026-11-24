# Security policy

## Report a vulnerability

Do not open a public issue for a security problem.

Report it privately through GitHub security advisories:

1. Open the [Security tab](https://github.com/geosquareai/geosquare-engine/security) of this repository.
2. Choose "Report a vulnerability".
3. Describe the problem, the steps to reproduce it, and the versions you tested.

We work with you on a fix before any public disclosure.

## Supported versions

Security fixes go into the latest released version on PyPI and into the latest `main` branch.

## Scope

Examples of issues we want to hear about:

- Wrong results that pass the spec 2.0 vectors. For example, a wrong GID or a wrong packed ID.
- Input that is accepted when it should raise an error.
- A way to run code or read files through the Python, WebAssembly, DuckDB, or `gsq` code.
- A signed registry check that accepts a bad file.

Problems in the spec itself belong to `geosquare-v2`. Report those in that repository, using the same private method.
