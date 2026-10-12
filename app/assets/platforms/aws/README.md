# AWS logo

The side menu shows this logo for the AWS platform. AWS and the AWS logo are
trademarks of Amazon.com, Inc. or its affiliates. ReGa is not affiliated
with, sponsored or endorsed by Amazon Web Services, and the logo is not
covered by ReGa's MIT licence.

## Source

AWS's official "AWS Cloud logo" group icon, from the AWS Architecture Icons
package of 31 July 2026 (<https://aws.amazon.com/architecture/icons/>):

- Package: <https://d1.awsstatic.com/onedam/marketing-channels/website/public/shared/architecture-icon-release/Icon-package_07312026.5846e92413caa21490223536cc97f1269e44fa92.zip>,
  sha256 `d2d166c453526471749d520e0db022c459abef759d2946cf2dd1d1c992dc6526`
- `aws.png`, for the light theme: `Architecture-Group-Icons_07312026/AWS-Cloud-logo_32.svg`,
  sha256 `63d73df1aac59dab323df732f82e196e359fb7b6370c0ca8e51e7b897cd350be`
- `aws-dark.png`, for the dark theme: `Architecture-Group-Icons_07312026/AWS-Cloud-logo_32_Dark.svg`,
  sha256 `1c5db2de7a100b1a5e51fcd4bdfd2c5ed76600b3c8b64677d614fa3c402b124c`

Each PNG is its SVG drawn at 32, 64 (`2.0x/`) and 96 px (`3.0x/`), for the
32 px icon at each screen density, with nothing else changed: AWS's
guidelines don't allow altering the logo's proportions or colours. To draw
them again, from a folder holding the two SVGs, into `$OUT` with its `2.0x/`
and `3.0x/` folders (each SVG is 40 px, so it's drawn at 0.8, 1.6 and 2.4
times), use Chrome's headless shell, for instance from
`npx @puppeteer/browsers install chrome-headless-shell@stable`. Plain
`chromium --headless` won't do: its window is at least 500 px wide, so it
only draws the top 2 px of the square.

```sh
for pair in AWS-Cloud-logo_32:aws AWS-Cloud-logo_32_Dark:aws-dark; do
  for scale in 0.8: 1.6:2.0x/ 2.4:3.0x/; do
    chrome-headless-shell --hide-scrollbars --window-size=40,40 \
      --force-device-scale-factor="${scale%%:*}" \
      --screenshot="$OUT/${scale#*:}${pair#*:}.png" "file://$PWD/${pair%%:*}.svg"
  done
done
```

`test/platform_icon_test.dart` checks each PNG's size, its square's colour,
and that the logo is drawn on it.

## Terms

AWS licenses these icons for architecture diagrams and similar material
(whitepapers, presentations, posters). Its
[trademark guidelines](https://aws.amazon.com/trademark-guidelines/) license
AWS customers' apps only the "Powered by AWS" logo (section 3(a)), allow
fair use in plain text without logos (section 13), and need AWS's written
permission for anything else (trademarks@amazon.com). ReGa uses this logo
anyway, as the project's owner decided on 12 October 2026, only to show
which cloud a platform is, unaltered, and on its own background.

If AWS asks for it to be removed, take out the `aws` entry in
`PlatformIcon.logos` (`lib/widgets/platform_icon.dart`), so AWS shows a
plain cloud, then everything that ships or describes the logo: the two
`assets/platforms/aws/` lines in `pubspec.yaml`, this folder, the AWS logo
tests in `test/platform_icon_test.dart`, `aws-logo.md` in
`flatpak/io.github.sergimn.rega.yml` and `flatpak/smoke-test.sh`, and the
logo's notes in the top-level README and `flatpak/README.md`.
