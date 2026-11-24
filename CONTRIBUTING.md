# Contributing

Thanks for helping with geosquare-engine. This page shows how to set up, test, and send a change.

The full developer guide is in [docs/development.md](docs/development.md).

## Before you start

- The grid rules live in `geosquare-v2`. Change the spec there first. Do not add grid rules here.
- Every binding must pass every test vector in `vectors/spec-2.0/`. Do not edit those files by hand.
- Invalid input must raise an error. Do not repair, cast, or wrap input silently.

## Set up

You need Rust, Python 3.11 or newer, and Node.js 22 or newer.

```zsh
git clone https://github.com/geosquareai/geosquare-engine.git
cd geosquare-engine
```

## Run the tests

Run the tests for the part you change. CI runs all of them.

| Part | Command |
|---|---|
| Rust core | `cd core && cargo test --locked` |
| Python binding | Build a wheel, then test it. See [docs/development.md](docs/development.md#python-binding) |
| WebAssembly binding | `cd bindings/wasm && npm run build && npm test` |
| DuckDB extension | `cd bindings/duckdb && cargo test --release --locked && ./build.sh` |
| `gsq` CLI | Build the DuckDB extension, then `python -m pip install ./cli pytest` and run `GSQ_EXTENSION=<path to the extension> python -m pytest -q cli/tests` |

The Python and WebAssembly tests run the same spec 2.0 vectors. Keep both green.

## Branches and pull requests

1. Branch from the latest `main`. Use a short name that says the task, for example `fix/detect-empty-column`.
2. Keep one change per pull request. Each feature gets its own commit.
3. Update the docs in the same pull request when behaviour changes. Write English first.
4. Open a pull request against `main`. Describe what changed and how you tested it.
5. Wait for CI to pass. A maintainer reviews and merges. Do not push to `main` directly.

## Code style

- Run `cargo fmt --check` and `cargo clippy --all-targets --locked -- -D warnings` for Rust changes.
- Keep new code small. Add a test for each new branch of logic.

## Security issues

Do not open a public issue for a security problem. See [SECURITY.md](SECURITY.md).

## Licence

By contributing, you agree that your change is released under the Apache-2.0 licence in [LICENSE](LICENSE).
