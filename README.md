# aaiyer/codex shared-auth fork

This is a maintained fork of [OpenAI Codex](https://github.com/openai/codex).
The current upstream tag and exact commit are recorded in
`[workspace.metadata.aaiyer]` in [codex-rs/Cargo.toml](codex-rs/Cargo.toml).
Fork versions use `X.Y.Z+aaiyer.N` and tags use `rust-vX.Y.Z+aaiyer.N`;
the initial release is `0.154.0+aaiyer.3`. These are **aaiyer builds**, not
OpenAI releases.

The maintained patches provide native shared authentication, preserve SemVer
build metadata in metric tag values, and provide public Linux releases.
Upstream installation commands below install upstream Codex; they do not install
this fork.

## Shared authentication

On Linux, set `CODEX_AUTH_HOME` to an existing private directory owned by the
current user (mode 0700). Keep a separate `CODEX_HOME` for each thread. The shared
directory holds `auth.json` and `auth.lock`; credential files must be private
(mode 0600). An unset `CODEX_AUTH_HOME` retains upstream storage behavior.
A supervisor may instead pass an exact numeric `/proc/<pid>/fd/<dirfd>` route;
the fork validates and retains the target directory handle. Other symlink routes
are rejected.

```sh
CODEX_AUTH_HOME=/private/shared-auth CODEX_HOME=/private/thread-home \
  ./bin/codex login --device-auth
CODEX_AUTH_HOME=/private/shared-auth \
  ./bin/codex login --with-auth-json < /private/source/auth.json
```

The import accepts at most 1 MiB of native durable auth JSON and applies existing
account restrictions. Login, import, refresh and logout share a file lock with a
30-second acquisition bound and atomically replace private credential files.
Uncoordinated raw copies bypass the refresh guarantee; use the import command.
A remote token rotation followed by a local crash may still require re-login.

Long-running sessions reload authentication before outbound model requests. A
current stream can finish with its original account. Switching accounts clears
cached WebSocket/incremental routing for the next request; logout or invalid
auth clears cached credentials. Shared mode does not fall back to ambient API
keys or workload identity. Explicit app-server external-token replacement is
rejected on a manager bound to shared file auth; use an explicitly ephemeral or
isolated manager for externally supplied credentials. Native durable auth modes
retain upstream validation. Shared storage does not change thread configuration,
history, or the native terminal interface.

## Fork releases

[GitHub Releases](https://github.com/aaiyer/codex/releases) provides:

- `codex-package-x86_64-unknown-linux-musl.tar.gz` for x86_64 Linux.
- `codex-package-aarch64-unknown-linux-musl.tar.gz` for ARM64 Linux.
- `SHA256SUMS` for checking the downloaded archives.

Keep the complete extracted package together, including `bin/`,
`codex-resources/`, `codex-path/`, and `codex-package.json`; launch `bin/codex`.
Check `bin/codex --version` against the selected release. GitHub's release asset
API also supplies each asset's `sha256:` digest for automated installers.

The musl target names describe the native Codex build. Upstream's bundled zsh
requires host glibc on both architectures; its ARM64 ripgrep also requires host
glibc. The release workflow qualifies these helpers on Ubuntu 24.04.

The [fork release workflow](.github/workflows/aaiyer-release.yml) runs only in
`aaiyer/codex` when a `rust-vX.Y.Z+aaiyer.N` tag is pushed. It uses public
`ubuntu-24.04` and `ubuntu-24.04-arm` runners, Rust 1.95.0, Zig 0.14.0, and
Python 3.12.9. It reuses upstream's musl build setup and canonical package
builder, including checksum-verified V8, ripgrep and zsh resources. Bundled
`bwrap` is finalized and hashed before building the CLI. The complete Rust
workspace suite on each musl release target, package unit tests, and smoke checks
of the extracted archive gate publication on both architectures. The workflow creates a draft release, checks GitHub's
uploaded asset digests, then publishes it. It uses only the repository's
`GITHUB_TOKEN`; it requires no OpenAI signing, internal runners, or release
storage credentials. Before building, it verifies the recorded upstream tag is
an exact stable `rust-vX.Y.Z` release (neither draft nor prerelease), its freshly
fetched peeled commit matches the recorded base and is an ancestor of the fork,
and the fork version has that upstream version prefix. Release notes include
both the upstream tag/commit and the built fork commit.

## Maintaining the fork

Maintain a small, linear patch stack on `aaiyer/shared-auth` and **rebase it onto
newer tagged stable upstream releases**. Do not merge upstream branches into the
patch stack. Alpha, beta, release-candidate, and other suffixed upstream tags are
excluded, even when a GitHub release is not marked prerelease. The commands
below use the explicit fork URL for branch observation and publication; a checkout
may still have `origin` pointing to `openai/codex`.

1. Start with a clean fork checkout and no running builds or writers. Read the
   old upstream tag and commit from `[workspace.metadata.aaiyer]`. Select an
   explicit upstream tag whose numeric `(major, minor, patch)` version is newer;
   do not use upstream `main`, a local tag, or `latest` as the source identity.
2. Set `upstream_tag` to that exact tag. Observe its published release and remote
   Git identities, then fetch and verify the same tag before using its commit:

   ```sh
   set -euo pipefail
   [[ "$upstream_tag" =~ ^rust-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
   gh api "repos/openai/codex/releases/tags/$upstream_tag" | jq -e --arg tag "$upstream_tag" \
     '.tag_name == $tag and .draft == false and .prerelease == false'
   observed_refs=$(git ls-remote --exit-code https://github.com/openai/codex.git \
     "refs/tags/$upstream_tag" "refs/tags/$upstream_tag^{}")
   observed_tag=$(printf '%s\n' "$observed_refs" | awk -v ref="refs/tags/$upstream_tag" '$2 == ref {print $1}')
   upstream_commit=$(printf '%s\n' "$observed_refs" | awk -v ref="refs/tags/$upstream_tag^{}" '$2 == ref {print $1}')
   upstream_commit=${upstream_commit:-$observed_tag}
   git fetch --no-tags https://github.com/openai/codex.git \
     "refs/tags/$upstream_tag:refs/aaiyer/upstream-next"
   test "$(git rev-parse refs/aaiyer/upstream-next)" = "$observed_tag"
   test "$(git rev-parse 'refs/aaiyer/upstream-next^{commit}')" = "$upstream_commit"
   ```

   Stop on any failure or changed identity. If the temporary ref already exists
   from an earlier attempt, inspect it before replacing it. The tag may be
   annotated or lightweight; `upstream_commit` always identifies the commit.
3. Set `old_base` to the recorded upstream commit. Save the current fork HEAD in
   a backup branch and capture the existing remote branch OID before rewriting
   the patch stack. Rebase only the fork commits, then review the replay:

   ```sh
   old_head=$(git rev-parse HEAD)
   expected_remote_head=$(git ls-remote --exit-code https://github.com/aaiyer/codex.git refs/heads/aaiyer/shared-auth | cut -f1)
   git branch "backup/shared-auth-before-$upstream_tag" "$old_head"
   git merge-base --is-ancestor "$old_base" "$old_head"
   git rebase --onto "$upstream_commit" "$old_base" aaiyer/shared-auth
   git range-diff "$old_base..$old_head" "$upstream_commit..HEAD"
   ```

   Resolve conflicts against the shared-auth contract and retain its regression
   tests. Review login, token refresh, account changes, WebSocket ownership, and
   app-server account reads. Retire a patch only when upstream provides its
   behavior. Use `git rebase --abort` if reconciliation cannot be completed.
4. Update `upstream-tag` and `upstream-commit` in the Cargo workspace metadata.
   Set `[workspace.package].version` to the selected upstream version plus
   `+aaiyer.1`, and refresh corresponding workspace versions in `Cargo.lock`.
   Increment `N` for subsequent releases on the same upstream base. Reconcile
   the fork workflow's Rust pin, build prerequisites, and packaged resource pins
   with the new upstream release; preserve the OpenAI-only upstream release
   guard. Commit these updates before checking the final candidate.
5. Run the fork's targeted Rust and package checks. The release workflow gates
   both Linux architectures on login/provider/CLI/telemetry tests, WebSocket auth
   regression coverage, app-server shared account replacement/logout coverage,
   and package smoke checks. A clean rebase or successful compilation alone does
   not establish that the fork patches still work.
6. Push the reviewed rebased branch using the exact previously observed lease,
   then create and push a **new** matching fork release tag:

   ```sh
   git push --force-with-lease="refs/heads/aaiyer/shared-auth:$expected_remote_head" \
     https://github.com/aaiyer/codex.git HEAD:refs/heads/aaiyer/shared-auth
   # Set fork_version to the exact workspace version, e.g. 0.154.0+aaiyer.3.
   git tag -a "rust-v$fork_version" -m "aaiyer Codex $fork_version"
   git push https://github.com/aaiyer/codex.git "rust-v$fork_version"
   ```

   If the lease fails, inspect the new remote work and reconcile it before
   retrying. Never force-update or delete a published release tag or replace its
   assets. Old tags retain their original commits even after the branch rebase.
7. Confirm the workflow published both packages and verified their GitHub asset
   digests before updating a dashboard or deployment pin. For corrected bytes,
   use a new fork revision. If publication stops after creating a draft, inspect
   the failure and unpublished draft before removing that draft and rerunning;
   the workflow does not overwrite an existing release.

---

<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

The standalone installers download from `https://releases.openai.com/codex` by default and fall back to GitHub Releases if a metadata or asset download is unavailable. To force GitHub Releases, set `CODEX_INSTALLER_USE_RELEASES_OPENAI_COM` to `false` (`0` and `no` are also accepted):

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_INSTALLER_USE_RELEASES_OPENAI_COM=false sh
```

```powershell
$env:CODEX_INSTALLER_USE_RELEASES_OPENAI_COM='false'; irm https://chatgpt.com/codex/install.ps1 | iex
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
