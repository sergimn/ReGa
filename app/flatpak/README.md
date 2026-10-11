# Flatpak

The manifest packages the Flutter release bundle, so build that first.

```sh
cd app
flutter build linux --release
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
  build/flatpak flatpak/io.github.sergimn.rega.yml
flatpak run io.github.sergimn.rega
```

CI builds the same bundle and uploads `rega.flatpak` as an artifact.

Terraform is bundled (the `terraform` module in the manifest); the app runs
it from `/app/bin`. The AWS Terraform provider is downloaded on first use
into the app's cache. The app reads AWS credentials from `~/.aws`, which
the manifest shares read-only.

Not bundled yet: Ansible. It will be added as a module here once the app
runs it.
