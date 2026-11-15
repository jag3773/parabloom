# Releasing Parabloom

A release is a `vMAJOR.MINOR.PATCH` tag on `main`. You start one from the
Actions tab, with no git commands, or by pushing the tag yourself. Either way
`.github/workflows/release.yml` checks the release, runs the full CI suite,
builds the binaries, publishes a GitHub release, and then publishes the crate
to [crates.io](https://crates.io/crates/parabloom). Nobody builds or uploads
release files by hand.

## What a release contains

| File | Contents |
|---|---|
| `parabloom-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 binary, built on Ubuntu 24.04 |
| `parabloom-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz` | Linux arm64 binary, built on Ubuntu 24.04 |
| `parabloom-vX.Y.Z-x86_64-apple-darwin.tar.gz` | macOS Intel binary, macOS 11 and later |
| `parabloom-vX.Y.Z-aarch64-apple-darwin.tar.gz` | macOS Apple silicon binary, macOS 11 and later |
| `parabloom-vX.Y.Z-source.tar.gz` | Complete source with every dependency vendored, including MuPDF |
| `SHA256SUMS` | SHA-256 checksums of the archives |

Each binary archive holds `parabloom`, `README.md`, `LICENSE` and
`CHANGELOG.md`. The archives also get a GitHub build-provenance attestation,
which `gh attestation verify` checks.

The same version is published to crates.io, so `cargo install parabloom
--locked` works. That builds MuPDF from source on the user's machine.

The source archive is there because the binaries statically link MuPDF,
which is AGPL-3.0. Whoever distributes a binary must offer the corresponding
source, and attaching it to the release does that. It builds offline with
`cargo build --release --frozen`.

## Cutting a release

1. **Pick the version.** Follow [Semantic Versioning](https://semver.org/).
   While the version is `0.x`, a change that alters output or command-line
   behaviour bumps the minor version.

2. **Open a release pull request** from a branch off `main`:
   - Set `version` in `Cargo.toml`. Then run `cargo check` (without
     `--locked`) so `Cargo.lock` picks up the new version. The release build
     uses `--locked` and fails if the two disagree.
   - In `CHANGELOG.md`, rename `## [Unreleased]` to
     `## [X.Y.Z] - YYYY-MM-DD` and add a new, empty `## [Unreleased]` above it.
   - Run `python3 tools/release.py check vX.Y.Z`.

   Because the pull request changes `Cargo.toml` and `CHANGELOG.md`, the
   Release workflow does a dry run on it. It builds every binary, the source
   archive and the crate package (`cargo publish --dry-run`) but publishes
   nothing. Wait for it to pass.

3. **Merge** the pull request.

4. **Start the release** from the Actions tab: choose **Release**, then
   **Run workflow**, leave the branch as `main`, enter the version (`X.Y.Z`,
   without the `v`), and run it. The workflow releases the latest commit on
   `main`. It creates the `vX.Y.Z` tag itself, only after every build has
   passed.

   If you prefer git, pushing the tag does the same thing:

   ```bash
   git switch main
   git pull
   git tag -a vX.Y.Z -m "Parabloom X.Y.Z"
   git push origin vX.Y.Z
   ```

5. **Watch the Release workflow** under the repository's Actions tab. When it
   finishes, the release appears under Releases, with notes taken from the
   changelog section, and the version appears on crates.io.

## When something goes wrong

- **The workflow fails before publishing.** Nothing has been released. Fix the
  problem on `main` through a pull request, then repeat step 4. A run started
  from the Actions tab creates no tag until the builds pass, so there is
  nothing to clean up. If you pushed the tag yourself, delete it first:

  ```bash
  git push --delete origin vX.Y.Z
  git tag -d vX.Y.Z
  ```

- **Only the crates.io job fails.** The GitHub release is already out. Fix the
  cause (an expired token, for example), then re-run just that failed job from
  the workflow run page. If the version already reached crates.io, the job
  skips the upload.

- **A published release is broken.** Don't move or reuse the tag, since people
  may already have downloaded it. Mark the GitHub release as a pre-release or
  edit its notes to warn people, and
  [yank](https://doc.rust-lang.org/cargo/commands/cargo-yank.html) the
  crates.io version (`cargo yank --version X.Y.Z parabloom`). A crates.io
  version can't be deleted or re-uploaded. Then ship a patch release.

## What the workflow checks before building

- The tag (or the version entered in the Actions tab) has the form
  `vMAJOR.MINOR.PATCH` and matches the version in `Cargo.toml`.
- A run from the Actions tab was started on `main`, and its tag doesn't exist
  yet.
- `CHANGELOG.md` has a non-empty section for that version.
- The tagged commit is on `main`.
- The CI suite passes: formatting, clippy, Rust tests and Python tool tests.
- Each binary reports the expected version from `parabloom --version`.
- The crate packages and builds from its package (`cargo publish --dry-run`).

## Repository settings

- **crates.io token.** The crates.io job reads `CARGO_REGISTRY_TOKEN` from the
  `Crates Publishing` deployment environment (Settings → Environments). If that
  environment limits deployments to certain branches or tags, it must allow
  `main` (for runs from the Actions tab) and `v*` tags (for pushed tags). Scope the token on crates.io to
  publishing `parabloom` only, and replace it before it expires.
- **Who can release.** Anyone with write access can start the workflow from
  the Actions tab, and anyone who can push a `v*` tag can publish a release.
  A tag ruleset (Settings → Rules → Rulesets) can restrict who creates or
  deletes `v*` tags. If you add one, let GitHub Actions bypass it so runs from
  the Actions tab can still create their tag.
