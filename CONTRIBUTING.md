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
| Rust crates, Dart packages, Terraform providers | `Cargo.lock`, `pubspec.lock`, `.terraform.lock.hcl` (checksums) |
| Sunshine, the virtual display driver, the AWS CLI, WireGuard | version and SHA-256 in the Ansible role defaults |
| Terraform in the Flatpak | version and SHA-256 in the manifest |
| Rust, Flutter, Terraform, TFLint, Python and runner images in CI | exact version |

Tools that only accept a version (the toolchains, Pester, PSScriptAnalyzer,
the Ansible collections, the TFLint AWS ruleset) are pinned to an exact one.
Not pinned on purpose: the Steam installer and the NVIDIA driver, which only
exist as "latest" and are signature-checked on the machine instead, and the
Ubuntu packages CI installs with apt.

The `.txt` files hold the versions you choose; install from the `.lock`
files, which pip checks against their hashes. To change a Python package,
edit the `.txt` file and regenerate its lock (and any lock built on it) with
the command in the lock's header, from the same folder:

```sh
cd infra/ansible
uv pip compile --universal --generate-hashes --python-version=3.12 requirements.txt --output-file=requirements.lock
uv pip compile --universal --generate-hashes --python-version=3.12 requirements-lint.txt --output-file=requirements-lint.lock
```

## Renovate

[Renovate](https://docs.renovatebot.com) (the Mend-hosted GitHub app) opens
PRs to keep all of the above current, with the config in
`.github/renovate.json5`. The "Dependency Dashboard" issue lists what it
found and what is waiting.

- Updates are grouped by area (GitHub Actions, pre-commit hooks, Rust
  crates, Dart packages, Terraform providers, Ansible), and a new release
  waits 3 days before Renovate proposes it.
- Lock files are refreshed weekly; the Flatpak builder image digest weekly.
- Digest, patch and minor updates merge on their own once every check on the
  PR has passed. Major updates wait for a review.
- Sunshine and the virtual display driver: Renovate updates the version and
  the SHA-256 together, from the GitHub release.
- Terraform: Renovate updates CI and the Flatpak together, but can't hash
  the Flatpak's download, so that PR needs its `sha256` set by hand (the PR
  says where to find it). flutter_rust_bridge updates need the bindings
  regenerated. Neither merges on its own.
- Not tracked: the AWS CLI (it releases several times a week, and its MSI
  has no published hash to update from) and WireGuard for Windows. Update
  their version and SHA-256 in the Ansible role defaults by hand.
