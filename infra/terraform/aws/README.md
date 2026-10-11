# ReGa machine on AWS

One Terraform root module = one ReGa machine: a Windows Server 2022 instance,
its security group, key pair, a separate D: game library volume and an idle
backstop alarm. The app runs one state per machine.

## State

State is kept in `rega-tfstate-<account-id>` (eu-north-1), which the app
creates on first run, with S3-native locking (`use_lockfile`):

```sh
terraform init \
  -backend-config="bucket=rega-tfstate-<account-id>" \
  -backend-config="key=machines/<machine-id>.tfstate" \
  -backend-config="region=eu-north-1"
```

## Lifecycle the app drives

| What the user does | Variables |
| --- | --- |
| New machine, CPU phase | `instance_type = "t3.medium"` (or another t3) |
| Switch to GPU | `instance_type = "g4dn.xlarge"` (stops, resizes, starts) |
| Switch back to CPU | `instance_type = "t3.medium"` |
| Stop (after syncing D: to S3) | `instance_state = "stopped"`, `library_volume_enabled = false` |
| Start | `instance_state = "running"`, `library_volume_enabled = true` |
| Spot | `spot = true` with `instance_type = "g4dn.xlarge"`; stopping means `terraform destroy` |
| WireGuard off | `wireguard_enabled = false`, `client_cidrs = ["<ip>/32"]` |
| Ansible run | `admin_cidrs = ["<ip>/32"]` opens WinRM HTTPS (5986) |

C: keeps the size it was created with (resizing a volume needs
`ec2:ModifyVolume`, which the policy denies). A new `library_volume_size`
applies when D: is recreated at the next start.

Change `instance_type` only together with a start. The provider always
starts the instance after a resize, so resizing a stopped machine boots it
briefly before `instance_state` stops it again in the same apply.

Pass `availability_zone` back from the output once the machine exists, so it
never moves zone. `public_ip` changes on every start.

The Windows Administrator password comes out as `password_data`, encrypted
with `public_key` (RSA). The boot script enables WinRM over HTTPS and
registers a scheduled task that brings a new, empty library volume online as
D: within a minute of it being attached, including when Terraform attaches it
after Windows has booted.

## Spot machines

Spot uses one-time requests, so a spot machine can't be stopped or resized.
The idle alarm terminates it instead of stopping it.

On destroy, the AWS provider cancels the spot request before terminating the
instance, and that needs `ec2:CancelSpotInstanceRequests`, which the
`rega-claude` policy doesn't grant yet. Until it does, end a spot session by
calling `TerminateInstances` first, waiting until the instance is `terminated`
(not just `shutting-down`), and then running `terraform destroy`: the
refresh drops the terminated instance and the destroy cleans up the rest.

Constraints from the `rega-claude` IAM policy: types t3.micro/medium/large and
g4dn.xlarge only, gp3 disks up to 256 GB, default VPC, spot only through
`aws_instance` market options, instance profile `ReGaInstanceRole` only, and
no `aws_s3_bucket_public_access_block`. Variable validation mirrors these so
a bad request fails at plan time, not halfway through an apply.

## Idle backstop alarm

`aws_cloudwatch_metric_alarm.idle_stop` stops the instance when CPU stays
below `idle_cpu_percent` for `idle_alarm_minutes` (120 by default). EC2
actions only run on single-metric alarms, so it can't combine CPU and network. It terminates spot machines instead.
A stop by the alarm leaves D: attached until the app's next apply.
CloudWatch needs the service-linked role `AWSServiceRoleForCloudWatchEvents`
for EC2 stop actions, and `rega-claude` can't create it. Create it once per
account as an admin:

```sh
aws iam create-service-linked-role --aws-service-name events.amazonaws.com
```

Until then, set `idle_alarm_enabled = false`.

## Tests

CI (`.github/workflows/terraform.yml`) runs `fmt`, `validate`, `tflint` and
`terraform test`. The tests in `tests/` plan against a mocked AWS provider and
need no credentials.

Real launches are tested by hand with the `rega-claude` key and a local
backend override (`backend_override.tf` with `backend "local" {}`), preferring
`t3.micro`, and destroyed afterwards.
