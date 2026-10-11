# ReGa machine setup (Ansible)

Turns the Windows Server 2022 machine from `infra/terraform/aws` into a
streaming box: Sunshine for Moonlight, a WireGuard tunnel, Steam, the S3 game
library agent with the idle stop, and, once the machine is a g4dn, the NVIDIA
gaming driver and a virtual display.

The same playbook runs in both phases. On a t3 (CPU phase) it installs
everything and leaves the GPU part to a boot task. Run it again after the
resize to g4dn and it finishes the GPU setup right away, rebooting as needed.
If nobody runs it, the boot task does the same on its own.

## Running it

The app writes an inventory and runs the playbook from its own folder. By
hand:

```sh
cd infra/ansible
python3 -m venv .venv && .venv/bin/pip install -r requirements.txt
.venv/bin/ansible-galaxy collection install -r requirements.yml
cp inventory.example.yml inventory.yml   # fill in the IP and password
.venv/bin/ansible-playbook -i inventory.yml site.yml
```

From Terraform it needs:

| Inventory variable | From |
| --- | --- |
| `ansible_host` | `public_ip` (changes on every start) |
| `ansible_password` | `password_data`, decrypted with the key pair's private key |
| `rega_wireguard_port` | the same `wireguard_port` Terraform opened |

WinRM HTTPS (5986) must be open to the controller first:
`admin_cidrs = ["<ip>/32"]`. The certificate is the self-signed one from the
boot script, so the connection doesn't validate it (`group_vars/rega.yml`).

Optional variables:

| Variable | Default | |
| --- | --- | --- |
| `rega_wireguard_client_public_key` | empty | The app's own WireGuard public key. Empty means the playbook makes a client key pair and writes `wireguard-client.conf`. |
| `rega_idle_stop_enabled` | `true` | Shut the machine down after `rega_idle_minutes` (15) with nobody playing. |
| `rega_library_bucket` / `rega_library_bucket_region` | `rega-games-<account>` / the machine's region | Where the Steam library lives in S3. |
| `rega_sunshine_password`, `rega_desktop_password` | generated once | Pass them to use your own. |
| `rega_reboot_allowed` | `true` | CI sets it to `false`. |

## What comes back

Everything the app needs lands in
`$XDG_DATA_HOME/rega/machines/<machine-id>/` (or `~/.local/share/...`), mode
0700, never in the repo:

- `connection.json`: Sunshine user and web UI port, WireGuard server public
  key, addresses, port and MTU.
- `sunshine-password`, `desktop-password`.
- `wireguard-client.conf`, only when no client key was passed. It holds the
  client's private key.

The inventory holds the Administrator password, so `inventory.yml` is
git-ignored.

## On the machine

| | |
| --- | --- |
| `base` | ReGa folders under `C:\ProgramData\ReGa` (SYSTEM and Administrators only, Users read), a `rega` desktop account that logs on at boot, high performance power plan with no display sleep, audio services, AWS CLI. |
| `steam` | Steam. Logging in to Steam is done by hand once, over Moonlight. |
| `sunshine` | Sunshine as a service, its web UI login, and the few `sunshine.conf` keys ReGa owns. |
| `wireguard` | WireGuard for Windows with one tunnel, `rega`: server `10.213.37.1/24`, client `10.213.37.2/32`, MTU 1400. Keys are made on the machine and the server key never leaves it. |
| `library_agent` | `rega-agent.ps1` and its scheduled tasks (below). |
| `gpu` | `rega-gpu-setup.ps1` and the `ReGa-GpuSetup` boot task. |

The tunnel always runs. Terraform's `wireguard_enabled` decides whether the
security group lets Moonlight in through it (UDP 51820) or straight to
Sunshine from the client's IP.

### Game library and idle stop

| Task | When | Does |
| --- | --- | --- |
| `ReGa-LibraryRestore` | boot | Waits for the empty D: from Terraform (label `ReGaLibrary`), copies `s3://rega-games-<account>/steam-library/` to `D:\SteamLibrary` and writes `D:\.rega-restored.json`. |
| `ReGa-Agent` | every minute | Tracks activity, uploads the library when Steam's installed games change and nobody is playing, and stops the machine after the idle time. |
| `ReGa-SteamLaunch` | `rega` logs on | Waits for the restore, adds `D:\SteamLibrary` to Steam's library folders and starts Steam. |

Activity means a Moonlight client connected (from Sunshine's log), a Steam
download in progress, a restore running, or a WinRM session (Ansible or the
app). Uploads use `aws s3 sync --delete`, and only after a restore finished
on this disk, so an empty or half-restored disk never wipes the bucket.

The idle stop does a final sync, records it at
`s3://rega-games-<account>/rega/machines/<machine-id>/final-sync.json`, and
shuts Windows down, which EC2 turns into a stop. It only does so on EC2. The
app should then apply `instance_state = "stopped"` and
`library_volume_enabled = false` as for any stop; the record says whether the
disk was safely uploaded.

The app can drive the agent over WinRM:

```powershell
powershell -NoProfile -File C:\ProgramData\ReGa\agent\rega-agent.ps1 -Action Status     # JSON: clients, downloads, restore and idle state
powershell -NoProfile -File C:\ProgramData\ReGa\agent\rega-agent.ps1 -Action FinalSync  # before a stop the app starts; prints {"synced": true|false}
powershell -NoProfile -File C:\ProgramData\ReGa\agent\rega-agent.ps1 -Action ResetIdle
```

Logs are in `C:\ProgramData\ReGa\logs`.

### GPU

On every boot `ReGa-GpuSetup` checks for an NVIDIA GPU. On the first g4dn
boot it:

1. Downloads the gaming driver from `s3://nvidia-gaming/windows/latest/`,
   installs it silently and sets the gaming license (registry value and
   `GridSwCert.txt`), then reboots.
2. Installs the [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)
   so Sunshine has a monitor to capture without a dummy plug, and disables the
   EC2 basic display adapter so Windows renders on the NVIDIA GPU.

On a t3 it does nothing. Back on a t3 after being a g4dn, the driver stays
installed and simply has no device.

The virtual display driver comes from a pinned release, checked against its
SHA-256. Its catalog must carry a valid signature from the pinned SignPath
certificate (thumbprint in `roles/gpu/defaults/main.yml`) before that
certificate is added to the machine's Trusted Publishers. It is never added
as a root. The certificate expires on 2027-09-07; a newer driver release
means updating the URL, hash and thumbprint together.

## Needs from AWS

`ReGaInstanceRole` reads the NVIDIA bucket, which needs this statement in the
`ReGaGameLibrary` policy (already in
`/mnt/project-files/aws-setup/rega-instance-role-policy.json`):

```json
{
  "Sid": "NvidiaGamingDriver",
  "Effect": "Allow",
  "Action": ["s3:ListBucket", "s3:GetObject"],
  "Resource": ["arn:aws:s3:::nvidia-gaming", "arn:aws:s3:::nvidia-gaming/*"]
}
```

Without it the GPU setup logs an error naming this policy and retries at the
next boot.

## Tests

`.github/workflows/ansible.yml` runs without AWS credentials:

- ansible-lint (production profile), a syntax check, PSScriptAnalyzer, and
  Pester unit tests of the agent (`tests/pester`).
- The playbook against a GitHub Windows Server 2022 runner, from Ansible in
  WSL over WinRM HTTPS set up by Terraform's own boot script. It runs twice
  and the second run must change nothing. Then it checks the services, the
  tunnel, the Sunshine API login, an agent tick, and installs the virtual
  display.

Not covered without a real g4dn: the NVIDIA driver install and license, the
virtual display next to the NVIDIA GPU, Sunshine capture and encoding, the S3
restore and sync with the instance role, and the idle stop turning into an
EC2 stop.
