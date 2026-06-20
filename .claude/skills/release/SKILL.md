---
name: release
description: Use when cutting a new parley release, bumping the version, or publishing a new version to Homebrew (e.g. "zrób release", "wydaj nową wersję", "release to homebrew").
---

# Release parley

Bump the version based on what changed since the last release, **confirm the
proposed version with the user**, commit the bump, then publish to the Homebrew
tap via `packaging/release.sh`.

## Preconditions (check first, abort if unmet)

- On branch `main` with a clean working tree (`git status`). Commit/stash pending work first.
- `gh` is authenticated (`gh auth status`) — `release.sh` needs it to publish.
- `cargo` builds (toolchain on PATH; `build.sh`/`release.sh` add rustup automatically).

## Procedure

1. **Find the baseline** — the last version bump:
   ```bash
   BASE=$(git log --grep='bump version' -i --format='%H' -1)
   git log --oneline "$BASE..HEAD"
   ```
   The current version is in `Cargo.toml` (`version = "X.Y.Z"`).

2. **Classify the commits** since `$BASE` by Conventional-Commit type and pick the bump:

   | Highest-impact change since last release | Bump | 0.3.0 → |
   |------------------------------------------|------|---------|
   | Breaking: `feat!`, `fix!`, or `BREAKING CHANGE` in body | major | 1.0.0 |
   | Any `feat:` | minor | 0.4.0 |
   | Only `fix:` / `chore:` / `docs:` / `refactor:` etc. | patch | 0.3.1 |

   Note: parley is pre-1.0 and treats `feat` as a minor bump (matches the 0.1→0.2→0.3 history). If a change is genuinely breaking, propose major but flag it.

3. **Propose & CONFIRM (required).** Show the user the commit list, the detected
   bump type, and the resulting version. Ask explicitly whether the version is OK.
   **Never skip this** — the user may override (e.g. wants major despite only fixes).
   Use the confirmed number even if it differs from the proposal.

4. **Bump `Cargo.toml`** — set `version = "X.Y.Z"` (the `[package]` version near the top).

5. **Sanity build + commit:**
   ```bash
   ./build.sh
   git commit -am "chore: bump version to X.Y.Z"
   ```

6. **Publish to Homebrew:**
   ```bash
   ./packaging/release.sh
   ```
   This builds the release binary, creates the tarball, publishes a GitHub Release
   in the tap repo (`zbyhoo/homebrew-parley`), and updates+pushes `Formula/parley.rb`.
   It reads the version straight from `Cargo.toml`, so step 4 must be committed first.

7. **Report** the install command and remind the user the source `main` bump commit
   is local — ask before `git push` (per the repo's git workflow).

## Common mistakes

- Running `release.sh` before committing the `Cargo.toml` bump → tarball/tag/version mismatch.
- Skipping the confirm step — the proposed bump is a suggestion, not the decision.
- Releasing from a feature branch or dirty tree → tap gets unmerged/uncommitted code.
- Assuming git tags exist in the source repo: there are none; the only tag (`vX.Y.Z`) lives in the tap repo, created by `release.sh`.
