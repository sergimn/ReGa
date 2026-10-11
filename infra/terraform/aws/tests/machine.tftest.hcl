# Plan-only tests with a mocked AWS provider. They need no credentials and
# run in CI. Real launches are tested by hand (see README).

mock_provider "aws" {
  override_data {
    target = data.aws_ec2_instance_type_offerings.gpu
    values = { locations = ["eu-central-1c", "eu-central-1a", "eu-central-1b"] }
  }

  override_data {
    target = data.aws_ami.windows
    values = { id = "ami-0123456789abcdef0" }
  }

  override_data {
    target = data.aws_vpc.default
    values = { id = "vpc-0123456789abcdef0" }
  }

  override_data {
    target = data.aws_subnet.default
    values = { id = "subnet-0123456789abcdef0" }
  }
}

variables {
  region     = "eu-central-1"
  machine_id = "test"
  public_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ test"
}

run "defaults_are_an_on_demand_cpu_machine" {
  command = plan

  assert {
    condition     = aws_instance.machine.instance_type == "t3.medium"
    error_message = "Default type should be the CPU phase type."
  }

  assert {
    condition     = length(aws_instance.machine.instance_market_options) == 0
    error_message = "On-demand machines must not set market options."
  }

  assert {
    condition     = aws_instance.machine.iam_instance_profile == "ReGaInstanceRole"
    error_message = "The IAM policy only allows passing ReGaInstanceRole."
  }

  assert {
    condition     = aws_instance.machine.root_block_device[0].volume_type == "gp3" && aws_instance.machine.root_block_device[0].volume_size == 60
    error_message = "C: should be a 60 GB gp3 volume."
  }

  assert {
    condition     = aws_instance.machine.metadata_options[0].http_tokens == "required"
    error_message = "IMDSv2 should be required."
  }

  assert {
    condition     = local.availability_zone == "eu-central-1a"
    error_message = "The zone should be the first, sorted, that offers the GPU type."
  }

  assert {
    condition     = aws_ebs_volume.library[0].type == "gp3" && aws_ebs_volume.library[0].availability_zone == "eu-central-1a"
    error_message = "D: should be gp3 in the machine's zone."
  }

  assert {
    condition     = aws_ec2_instance_state.machine.state == "running"
    error_message = "Machines start running."
  }

  assert {
    condition     = aws_volume_attachment.library[0].stop_instance_before_detaching && !aws_volume_attachment.library[0].force_detach
    error_message = "On-demand machines must stop before D: is detached, without forcing."
  }

  assert {
    condition     = keys(aws_vpc_security_group_ingress_rule.machine) == ["wireguard-51820"]
    error_message = "With WireGuard on and no admin IPs, only the WireGuard port is open."
  }

  assert {
    condition     = aws_vpc_security_group_ingress_rule.machine["wireguard-51820"].cidr_ipv4 == "0.0.0.0/0" && aws_vpc_security_group_ingress_rule.machine["wireguard-51820"].ip_protocol == "udp"
    error_message = "WireGuard should be open over UDP to any IP."
  }

  assert {
    condition     = length(aws_cloudwatch_metric_alarm.idle_stop) == 1 && aws_cloudwatch_metric_alarm.idle_stop[0].evaluation_periods == 24
    error_message = "The idle backstop should cover 120 minutes of 5-minute periods."
  }

  assert {
    condition     = aws_cloudwatch_metric_alarm.idle_stop[0].alarm_actions == toset(["arn:aws:automate:eu-central-1:ec2:stop"])
    error_message = "The idle backstop should stop the instance."
  }

  assert {
    condition     = aws_cloudwatch_metric_alarm.idle_stop[0].metric_name == "CPUUtilization" && length(aws_cloudwatch_metric_alarm.idle_stop[0].metric_query) == 0
    error_message = "EC2 actions need a single-metric alarm, not metric math."
  }
}

run "tags_carry_project_and_machine" {
  command = plan

  assert {
    condition     = local.tags["Project"] == "ReGa" && local.tags["ReGaMachine"] == "test"
    error_message = "Default tags must carry Project=ReGa and the machine id."
  }

  assert {
    condition     = aws_instance.machine.root_block_device[0].tags["Project"] == "ReGa"
    error_message = "The root volume needs its own Project tag."
  }
}

run "wireguard_off_opens_sunshine_to_client_ips_only" {
  command = plan

  variables {
    wireguard_enabled = false
    client_cidrs      = ["203.0.113.7/32"]
    admin_cidrs       = ["203.0.113.7/32"]
  }

  assert {
    condition     = !contains(keys(aws_vpc_security_group_ingress_rule.machine), "wireguard-51820")
    error_message = "WireGuard port should be closed when WireGuard is off."
  }

  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.machine) == 7
    error_message = "Expected six Sunshine rules plus WinRM."
  }

  assert {
    condition     = alltrue([for r in aws_vpc_security_group_ingress_rule.machine : r.cidr_ipv4 == "203.0.113.7/32"])
    error_message = "Every rule should be limited to the client IP."
  }
}

run "stopped_without_library" {
  command = plan

  variables {
    instance_state         = "stopped"
    library_volume_enabled = false
  }

  assert {
    condition     = aws_ec2_instance_state.machine.state == "stopped"
    error_message = "Machine should be stopped."
  }

  assert {
    condition     = length(aws_ebs_volume.library) == 0 && length(aws_volume_attachment.library) == 0
    error_message = "D: should not exist while stopped."
  }
}

run "spot_gpu_uses_market_options" {
  command = plan

  variables {
    instance_type = "g4dn.xlarge"
    spot          = true
  }

  assert {
    condition     = aws_instance.machine.instance_market_options[0].market_type == "spot"
    error_message = "Spot should go through RunInstances market options."
  }

  assert {
    condition     = aws_instance.machine.instance_market_options[0].spot_options[0].spot_instance_type == "one-time"
    error_message = "Spot requests must be one-time so destroy leaves nothing behind."
  }

  assert {
    condition     = aws_volume_attachment.library[0].stop_instance_before_detaching == false && aws_volume_attachment.library[0].force_detach == true
    error_message = "Spot instances can't be stopped, so D: must be force-detached on destroy instead."
  }

  assert {
    condition     = aws_cloudwatch_metric_alarm.idle_stop[0].alarm_actions == toset(["arn:aws:automate:eu-central-1:ec2:terminate"])
    error_message = "The idle backstop must terminate spot machines, which can't be stopped."
  }
}

run "spot_rejects_cpu_types" {
  command = plan

  variables {
    instance_type = "t3.medium"
    spot          = true
  }

  expect_failures = [var.instance_type]
}

run "spot_cannot_be_stopped" {
  command = plan

  variables {
    instance_type  = "g4dn.xlarge"
    spot           = true
    instance_state = "stopped"
  }

  expect_failures = [var.instance_state]
}

run "rejects_types_outside_the_policy" {
  command = plan

  variables {
    instance_type = "m5.large"
  }

  expect_failures = [var.instance_type]
}

run "rejects_disks_over_256_gb" {
  command = plan

  variables {
    root_volume_size    = 300
    library_volume_size = 257
  }

  expect_failures = [var.root_volume_size, var.library_volume_size]
}

run "rejects_non_rsa_keys" {
  command = plan

  variables {
    public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 test"
  }

  expect_failures = [var.public_key]
}

run "rejects_idle_windows_over_a_day" {
  command = plan

  variables {
    idle_alarm_minutes = 1445
  }

  expect_failures = [var.idle_alarm_minutes]
}
