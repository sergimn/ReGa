locals {
  name = "rega-${var.machine_id}"

  tags = {
    Project     = "ReGa"
    ReGaMachine = var.machine_id
    ManagedBy   = "terraform"
  }

  availability_zone = coalesce(var.availability_zone, sort(data.aws_ec2_instance_type_offerings.gpu.locations)[0])
  ami_id            = coalesce(var.ami_id, data.aws_ami.windows.id)
}

# The machine must sit in a zone that offers the GPU type, or the resize from
# the CPU phase would fail.
data "aws_ec2_instance_type_offerings" "gpu" {
  location_type = "availability-zone"

  filter {
    name   = "instance-type"
    values = [var.gpu_instance_type]
  }
}

data "aws_ami" "windows" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["Windows_Server-2022-English-Full-Base-*"]
  }

  filter {
    name   = "architecture"
    values = ["x86_64"]
  }
}

# The IAM policy can't create VPCs, so machines use the region's default VPC.
data "aws_vpc" "default" {
  default = true
}

data "aws_subnet" "default" {
  vpc_id            = data.aws_vpc.default.id
  availability_zone = local.availability_zone
  default_for_az    = true
}
