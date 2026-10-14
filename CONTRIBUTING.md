# Contributing

## pre-commit

The repo's formatting and lint checks run through [pre-commit](https://pre-commit.com).
Install it once, then it runs on every commit:

```sh
uv tool install pre-commit   # or: pipx install pre-commit
pre-commit install
pre-commit run --all-files   # everything, not just your changes
```

`.pre-commit-config.yaml` has the hooks: whitespace and file checks, the
Renovate config schema, actionlint and zizmor for the workflows, and four
that use your own toolchains: `cargo fmt`, `dart format`, `terraform fmt`
and `ansible-lint` (run from `infra/ansible`, with its collections
installed). Skip one you don't have with `SKIP=dart-format git commit ...`.

CI runs the same hooks on every PR (`.github/workflows/pre-commit.yml`),
except the four toolchain ones, which the App, Terraform and Ansible
workflows already check.

## Pinned versions

Everything the build downloads is pinned, to a hash wherever the tool can
check one:

| What | Pinned by |
| --- | --- |
| GitHub Actions | commit SHA, with the tag in a comment |
| The Flatpak builder image | image digest |
| pre-commit hooks | commit SHA, with the tag after `# frozen:` |
| Python packages (Ansible controller, ansible-lint, pre-commit in CI) | a `.lock` file with a SHA-256 for every package, generated from the `.txt` file of the same name |
| Rust crates, Dart packages, Terraform providers | `Cargo.lock`, `pubspec.lock`, `.terraform.lock.hcl` (checksums), which CI uses read-only |
| Sunshine, the virtual display driver, the AWS CLI, WireGuard | version and SHA-256 in the Ansible role defaults |
| Terraform in the Flatpak | version and SHA-256 in the manifest |
| Ansible in the Flatpak | `app/flatpak/ansible.yml`, generated from the Ansible lock and `requirements.yml` with a SHA-256 for every file |
| Flutter and Rust in the Flatpak build (the SDK, its engine and tool packages, the toolchain) | `app/flatpak/flutter.yml` and `rust.yml`, generated from CI's versions with a SHA-256 for every file (Flutter's git repository, a commit) |
| Rust, Flutter, Terraform, TFLint, Python and runner images in CI | exact version |

Tools that only accept a version (the toolchains, Pester, PSScriptAnalyzer,
the Ansible collections, the TFLint AWS ruleset) are pinned to an exact one.
Not pinned on purpose: the Steam installer and the NVIDIA driver, which only
exist as "latest" and are signature-checked on the machine instead, the
Ubuntu packages CI installs with apt, and the Flatpak's runtime, SDK and
LLVM SDK extension, which come from Flathub at their current build of the
branch the manifest names.

The `.txt` files hold the versions you choose; install from the `.lock`
files, which pip checks against their hashes. To change a Python package,
edit the `.txt` file and regenerate its lock (and any lock built on it) with
the command in the lock's header, from the same folder:

```sh
cd infra/ansible
uv pip compile --universal --generate-hashes --python-version=3.12 --exclude-newer=P30D requirements.txt --output-file=requirements.lock
uv pip compile --universal --generate-hashes --python-version=3.12 --exclude-newer=P30D requirements-lint.txt --output-file=requirements-lint.lock
```

`--exclude-newer=P30D` keeps every package in the lock at least 30 days old
(see Renovate below), so a version you pin in the `.txt` file has to be that
old too.

## Renovate

[Renovate](https://docs.renovatebot.com) (the Mend-hosted GitHub app) opens
PRs to keep all of the above current, with the config in
`.github/renovate.json5`. The "Dependency Dashboard" issue lists what it
found and what is waiting.

- Renovate only takes releases that are at least 30 days old, security
  fixes included, so a hijacked or malicious release has time to be caught
  and pulled first. When the newest release is younger, it proposes the
  newest one that is old enough. The Python locks hold the packages they
  pull in to the same rule, and CI's pre-commit job installs the hooks' own
  Python dependencies with `PIP_UPLOADED_PRIOR_TO=P30D`.
- Updates whose source gives no release date (the Flatpak builder image on
  ghcr.io, the runner images) are never proposed on their own. They wait on
  the Dependency Dashboard until someone ticks them, and never merge on
  their own. The builder image digest is the one exception to the 30 days:
  the registry only lists a tag's current digest, and the one pinned was 19
  days old when it was set.
- Lock file refreshes are off, since they would take the newest version of
  everything at once with no age check. Cargo and pub can't filter by date:
  when a crate or Dart package update needs a newer version of something it
  depends on, that one comes at its newest compatible version.
- Updates are grouped by area (GitHub Actions, pre-commit hooks, Rust
  crates, Dart packages, Terraform providers, Ansible).
- If Renovate opens a config migration PR, put the comments back in
  `.github/renovate.json5` before merging: it rewrites the file without
  them, and the `// renovate:` comment there is what keeps Renovate's
  Flutter in step with CI's.
- Digest, patch and minor updates merge on their own once every check on the
  PR has passed. Major updates wait for a review, and so do breaking 0.x
  updates of crates and Dart packages.
- For GitHub Actions and pre-commit hooks, the release date is the date on
  the tag's commit, which whoever pushes the tag sets, so a hijacked repo
  could back-date a release past the 30 days. They still merge on their
  own once CI passes.
- Sunshine and the virtual display driver: Renovate updates the version and
  the SHA-256 together, from the GitHub release.
- Terraform: Renovate updates CI and the Flatpak together, but can't hash
  the Flatpak's download, so that PR needs its `sha256` set by hand (the PR
  says where to find it). flutter_rust_bridge updates need the bindings
  regenerated, and Flutter updates need `app/pubspec.lock` regenerated with
  the new Flutter (`flutter pub get`), since CI uses it read-only. None of
  these merges on its own.
- Flutter and Rust: the Flatpak builds the app with CI's versions, from
  `app/flatpak/flutter.yml` and `rust.yml`, so their PRs need
  `python3 app/flatpak/sources.py update` run and its result committed (the
  PR says so, and CI fails until then). These don't merge on their own.
  Crate and Dart package updates need nothing: the Flatpak build takes its
  sources from the locks.
- Ansible: the Flatpak bundles the versions in `infra/ansible/requirements.lock`
  and `requirements.yml`, so a PR that changes them needs
  `python3 app/flatpak/pins.py update` run and its result committed (the PR
  says so, and CI fails until then). These don't merge on their own either.
- Rust crates: the Flatpak ships each crate's licence text, gathered at
  build time, so crate updates need nothing by hand. The App workflow fails
  when a crate ships no licence text, comes under a licence that needs more
  than its notice, or changes a licence someone read by hand; the message
  says what to do (more in `app/flatpak/README.md`).
- Not tracked: the AWS CLI (it releases several times a week, and its MSI
  has no published hash to update from) and WireGuard for Windows. Update
  their version and SHA-256 in the Ansible role defaults by hand.

## Releasing

A release is a `v` tag on main; the Release workflow
(`.github/workflows/release.yml`) does the rest. In a PR first:

1. Set the new version (MAJOR.MINOR.PATCH) in `app/pubspec.yaml` (keep a
   `+build` number), `app/core/Cargo.toml` and `app/rust/Cargo.toml`, then
   run `cargo update --workspace` in `app/`, which moves only ReGa's own
   crates in `Cargo.lock`.
2. Add a `<release version=... date=...>` at the top of the `<releases>` in
   `app/flatpak/io.github.sergimn.rega.metainfo.xml`, with a sentence or two
   on what changed; software centres show it.
3. Write the release notes in `docs/releases/<version>.md`: install steps,
   what's new, what doesn't work yet. They become the GitHub release's text.

`python3 app/flatpak/release.py check` (CI runs it) fails until all of these
agree. To try the uncached build a release does, run the App workflow by
hand (Actions, App, Run workflow) with "release" ticked. Once the PR has
merged, tag its merge commit and push the tag:

```sh
git tag v0.0.1 origin/main
git push origin v0.0.1
```

The workflow checks the tag is on main and matches the version, runs the
whole App workflow on it without caches, and publishes the Flatpak it built
and tested as `rega-x86_64.flatpak`, with a `SHA256SUMS` file. If a job
fails, fix it on main, delete the tag (`git push --delete origin v0.0.1`)
and tag again.
