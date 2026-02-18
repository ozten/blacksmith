# Releasing Blacksmith

## Overview

Releases are driven by git tags. Pushing a `v*` tag to GitHub triggers a CI
workflow that cross-compiles binaries for four platforms, creates a GitHub
Release with the artifacts, and updates a rolling `latest` tag.

## Cutting a release

1. Make sure your working tree is clean (everything committed).
2. Run the release script:

   ```bash
   ./scripts/release.sh 0.2.0
   ```

   This will:
   - Validate the version string (must be semver: `X.Y.Z`)
   - Bump `version` in `Cargo.toml` and `blacksmith-ui/Cargo.toml`
   - Update `Cargo.lock`
   - Create a commit: `release: v0.2.0`
   - Create the git tag `v0.2.0`

3. Push the commit and tag:

   ```bash
   git push origin main v0.2.0
   ```

   The tag push triggers `.github/workflows/release.yml`.

## What the CI builds

The workflow runs a matrix build across four targets:

| Platform        | Runner          | Method  |
|-----------------|-----------------|---------|
| linux_amd64     | ubuntu-latest   | cargo   |
| linux_arm64     | ubuntu-latest   | cross   |
| darwin_amd64    | macos-13        | cargo   |
| darwin_arm64    | macos-latest    | cargo   |

Each build produces a tarball: `blacksmith_<version>_<platform>.tar.gz`
containing the `blacksmith` binary (and `blacksmith-ui` if present).

After all builds complete, the release job:
- Collects all tarballs
- Generates `checksums.txt` (SHA-256)
- Creates a GitHub Release with auto-generated release notes
- Force-updates the `latest` git tag to point at this release

## User installation

Users install via:

```bash
curl -fsSL https://raw.githubusercontent.com/ozten/blacksmith/main/scripts/install.sh | bash
```

To pin a specific version:

```bash
BLACKSMITH_VERSION=0.2.0 curl -fsSL https://raw.githubusercontent.com/ozten/blacksmith/main/scripts/install.sh | bash
```

The install script:
- Detects OS (linux/darwin) and architecture (amd64/arm64)
- Fetches the latest (or pinned) release from the GitHub API
- Downloads and extracts the tarball
- Installs to `/usr/local/bin` (or `~/.local/bin` if not writable)
- Re-signs the binary on macOS to avoid Gatekeeper delays

## Key files

| File | Purpose |
|------|---------|
| `.github/workflows/release.yml` | CI workflow triggered by version tags |
| `scripts/release.sh` | Developer script to bump version + tag |
| `scripts/install.sh` | User-facing curl installer |

## Troubleshooting

**CI fails on Linux ARM64 build**: The `cross` tool requires Docker. The
ubuntu-latest runner has Docker pre-installed, but if the `cross` version
is pinned and stale, update the install command in the workflow.

**macOS binary triggers Gatekeeper warning**: The install script re-signs
with an ad-hoc signature. If users still see warnings, they can run:
```bash
codesign --force --sign - $(which blacksmith)
```

**Tag already exists**: If you need to redo a release, delete the tag
locally and remotely first:
```bash
git tag -d v0.2.0
git push origin :refs/tags/v0.2.0
```
Then delete the GitHub Release via the web UI and re-run the release script.
