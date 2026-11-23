# Release the Python package

The Python package is `geosquare-engine` on PyPI. The release workflow is `.github/workflows/release.yml`.

Releases are permanent. PyPI does not allow the same version twice. Check everything on TestPyPI first.

## Two ways to publish

| Way | Trigger | Goes to |
|---|---|---|
| Run the workflow by hand | Actions, then Run workflow, on a branch | TestPyPI |
| Push a tag `py-v<version>` | A tag on `main` | PyPI |

The tag must match the version in `bindings/python/pyproject.toml`. The workflow stops if they differ.

## Steps

1. Create a branch. Set the new version in three places:
   - `core/Cargo.toml`
   - `bindings/python/Cargo.toml`
   - `bindings/python/pyproject.toml`
2. Refresh the lock files so they match the new version:

   ```zsh
   cd core && cargo metadata --format-version 1 --offline >/dev/null
   cd ../bindings/python && cargo metadata --format-version 1 --offline >/dev/null
   ```

   Check that both lock files still pass `cargo metadata --locked`.
3. Open a pull request. Describe what changed since the last release. Say if the change breaks users, such as a new minimum Python version.
4. Run the workflow by hand on the branch. It publishes to TestPyPI.
5. Install from TestPyPI into a clean Python 3.11 environment. Import the package and run a small check.
6. After the pull request is merged, create the tag on the merge commit:

   ```zsh
   git tag -a py-v0.2.0 <merge-commit-sha> -m "geosquare-engine 0.2.0"
   git push origin refs/tags/py-v0.2.0
   ```

   Push only the tag. Do not push `main` with it.
7. The workflow builds the wheels for Linux, macOS, and Windows, runs the tests, and publishes to PyPI.
8. Install from PyPI into a clean environment and check the version.

## Python floor

Version 0.2.0 requires Python 3.11 or newer. Older Python versions cannot install it. Say this in the release notes.

## If something goes wrong

- A failed build does not publish. Fix the cause, then tag a new version.
- A version that is already on PyPI cannot be replaced. Publish the next version.
