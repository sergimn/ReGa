# D: holds the Steam library. It is synced to S3 and deleted on stop, then
# recreated and restored on start, so stored games cost S3 rates, not EBS.
resource "aws_ebs_volume" "library" {
  count = var.library_volume_enabled ? 1 : 0

  availability_zone = local.availability_zone
  type              = "gp3"
  size              = var.library_volume_size
  encrypted         = true

  tags = {
    Name     = "${local.name}-library"
    ReGaDisk = "library"
  }

  lifecycle {
    # Resizing in place needs ec2:ModifyVolume, which the IAM policy denies.
    # D: is recreated on every start, so a new size applies then.
    ignore_changes = [size]
  }
}

resource "aws_volume_attachment" "library" {
  count = var.library_volume_enabled ? 1 : 0

  device_name = "xvdf"
  volume_id   = aws_ebs_volume.library[0].id
  instance_id = aws_instance.machine.id

  # Windows must not lose the disk while running. Spot instances can't be
  # stopped; they only detach on destroy, when D: is about to be deleted, so
  # a forced detach is safe and doesn't hang on open files.
  stop_instance_before_detaching = !var.spot
  force_detach                   = var.spot
}
