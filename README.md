<p align="center">
  <img src="app/assets/icon/rega.svg" width="128" height="128" alt="ReGa icon">
</p>

<h1 align="center">ReGa</h1>

<p align="center">Play your games on a cloud machine you own, streamed to your Linux desktop.</p>

ReGa is a desktop app plus a set of Terraform and Ansible scripts. It sets up
a Windows gaming machine in your own cloud account (AWS first), streams it to
you with Moonlight and Sunshine, and stops it when you stop playing.

![The ReGa desktop app](docs/screenshot.png)

## Repository

| Path | What |
| --- | --- |
| `app/` | Desktop app: Flutter UI on a Rust core, packaged as a Flatpak ([how to install it](app/flatpak/README.md#install)). See [app/README.md](app/README.md). |
| `infra/` | Terraform (and later Ansible) that creates and configures the cloud machine. |

Checks, pinned versions and dependency updates: see [CONTRIBUTING.md](CONTRIBUTING.md).

## Colour palette

Cool tones only: teal for actions, indigo and violet for accents, on navy.
The values live in [`app/lib/theme.dart`](app/lib/theme.dart).

![ReGa colour palette](app/assets/palette.svg)

| Role | Light theme | Dark theme |
| --- | --- | --- |
| Primary | Deep teal `#0F766E` | Aqua `#5EEAD4` |
| Secondary | Deep indigo `#4F46E5` | Indigo `#818CF8` |
| Tertiary | Deep violet `#7C3AED` | Violet `#A78BFA` |
| Surface | Mist `#F4F7FB` | Night `#0B1220` |
| Cards | White | Navy `#1C2A4A` |
| Error | `#CE2C31` | `#F87171` |

## Licence

ReGa is released under the [MIT License](LICENSE). The vendored Cargokit
build scripts in `app/rust_builder/cargokit/` keep their own copyright and
licence, in [their LICENSE file](app/rust_builder/cargokit/LICENSE).
