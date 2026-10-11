# Flatpak

The manifest packages the Flutter release bundle, so build that first.

```sh
cd app
flutter build linux --release
flatpak-builder --user --install --force-clean build/flatpak flatpak/io.github.sergimn.rega.yml
flatpak run io.github.sergimn.rega
```

CI builds the same bundle and uploads `rega.flatpak` as an artifact.

Not bundled yet: Terraform and Ansible. They will be added as modules here
once the app runs them.
