resource "aws_key_pair" "machine" {
  key_name   = local.name
  public_key = var.public_key
}

resource "aws_instance" "machine" {
  ami                    = local.ami_id
  instance_type          = var.instance_type
  subnet_id              = data.aws_subnet.default.id
  vpc_security_group_ids = [aws_security_group.machine.id]
  key_name               = aws_key_pair.machine.key_name
  iam_instance_profile   = var.instance_profile_name
  get_password_data      = true

  user_data = templatefile("${path.module}/templates/user_data.ps1.tftpl", {})

  # Spot uses RunInstances market options, which the IAM policy allows; a
  # standalone spot request isn't. On destroy the provider cancels the spot
  # request before terminating, which needs ec2:CancelSpotInstanceRequests
  # (see README).
  dynamic "instance_market_options" {
    for_each = var.spot ? [1] : []
    content {
      market_type = "spot"
      spot_options {
        spot_instance_type             = "one-time"
        instance_interruption_behavior = "terminate"
        max_price                      = var.spot_max_price
      }
    }
  }

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
  }

  # C: is kept between sessions and goes away with the machine.
  root_block_device {
    volume_type           = "gp3"
    volume_size           = var.root_volume_size
    encrypted             = true
    delete_on_termination = true
    tags = merge(local.tags, {
      Name     = "${local.name}-system"
      ReGaDisk = "system"
    })
  }

  tags = {
    Name = local.name
  }

  lifecycle {
    # A newer Windows AMI must not replace an existing machine. Resizing C:
    # needs ec2:ModifyVolume, and changing user_data needs a non-resize
    # ModifyInstanceAttribute; the IAM policy denies both. Existing machines
    # keep the boot script they were created with.
    ignore_changes = [ami, root_block_device[0].volume_size, user_data]
  }
}

# Attach the library before starting, so the boot script can bring D: online.
resource "aws_ec2_instance_state" "machine" {
  instance_id = aws_instance.machine.id
  state       = var.instance_state

  depends_on = [aws_volume_attachment.library]

  lifecycle {
    # A resize stops and restarts the instance behind
    # this resource's back. Re-apply the wanted state after every instance
    # change, so a stopped machine stays stopped and public_ip is re-read.
    replace_triggered_by = [aws_instance.machine]
  }
}

# aws_instance only knows the address from before a stop or start, so read the
# instance again once it has reached instance_state.
data "aws_instance" "current" {
  instance_id = aws_ec2_instance_state.machine.id

  depends_on = [aws_ec2_instance_state.machine]
}
