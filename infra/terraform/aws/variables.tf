locals {
  # Instance types the IAM policy allows. Keep in sync with
  # aws-setup/rega-claude-policy.json.
  cpu_instance_types = ["t3.micro", "t3.medium", "t3.large"]
  gpu_instance_types = ["g4dn.xlarge"]
}

variable "region" {
  description = "AWS region the machine runs in."
  type        = string
}

variable "machine_id" {
  description = "Short id of this machine, unique per account. Used in resource names and the state key."
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{0,31}$", var.machine_id))
    error_message = "machine_id must be 1-32 lowercase letters, digits or dashes."
  }
}

variable "instance_type" {
  description = "Current instance type. A CPU type for setup and downloads, the GPU type for playing. Changing it on an on-demand machine stops, resizes and restarts it in place."
  type        = string
  default     = "t3.medium"

  validation {
    condition     = contains(concat(local.cpu_instance_types, local.gpu_instance_types), var.instance_type)
    error_message = "instance_type must be one of t3.micro, t3.medium, t3.large or g4dn.xlarge."
  }

  validation {
    condition     = !var.spot || contains(local.gpu_instance_types, var.instance_type)
    error_message = "Spot machines can't be resized, so they must use a GPU type."
  }
}

variable "gpu_instance_type" {
  description = "GPU type this machine will be resized to. The availability zone is chosen so this type is offered there."
  type        = string
  default     = "g4dn.xlarge"

  validation {
    condition     = contains(local.gpu_instance_types, var.gpu_instance_type)
    error_message = "gpu_instance_type must be g4dn.xlarge."
  }
}

variable "spot" {
  description = "Launch as a one-time Spot instance. Spot machines can't be stopped or resized: stopping one means destroying it (the game library is safe in S3)."
  type        = bool
  default     = false
}

variable "spot_max_price" {
  description = "Highest Spot price per hour in USD. null means the on-demand price."
  type        = string
  default     = null
}

variable "instance_state" {
  description = "running or stopped."
  type        = string
  default     = "running"

  validation {
    condition     = contains(["running", "stopped"], var.instance_state)
    error_message = "instance_state must be running or stopped."
  }

  validation {
    condition     = !var.spot || var.instance_state == "running"
    error_message = "Spot machines can't be stopped; destroy them instead."
  }
}

variable "availability_zone" {
  description = "Availability zone to use. null picks the first zone that offers gpu_instance_type. Keep it fixed once the machine exists."
  type        = string
  default     = null
}

variable "ami_id" {
  description = "AMI to launch. null uses the latest Windows Server 2022 English Full Base image. Later AMI releases don't replace an existing machine."
  type        = string
  default     = null
}

variable "public_key" {
  description = "OpenSSH RSA public key. AWS encrypts the Windows Administrator password with it (see the password_data output)."
  type        = string

  validation {
    condition     = startswith(var.public_key, "ssh-rsa ")
    error_message = "Windows instances need an RSA key (ssh-rsa ...)."
  }
}

variable "instance_profile_name" {
  description = "Instance profile that gives the machine access to the game library bucket. The IAM policy only lets the app pass ReGaInstanceRole."
  type        = string
  default     = "ReGaInstanceRole"
}

variable "root_volume_size" {
  description = "Size of C: in GB. Kept between sessions."
  type        = number
  default     = 60

  validation {
    condition     = var.root_volume_size >= 30 && var.root_volume_size <= 256
    error_message = "root_volume_size must be between 30 and 256 GB."
  }
}

variable "library_volume_enabled" {
  description = "Whether the D: game library volume exists. The app sets it to false after syncing games to S3 on stop, and back to true on start."
  type        = bool
  default     = true
}

variable "library_volume_size" {
  description = "Size of the D: game library volume in GB."
  type        = number
  default     = 100

  validation {
    condition     = var.library_volume_size >= 1 && var.library_volume_size <= 256
    error_message = "library_volume_size must be between 1 and 256 GB."
  }
}

variable "wireguard_enabled" {
  description = "Open the WireGuard port to any IP. When false, the Sunshine ports open to client_cidrs instead."
  type        = bool
  default     = true
}

variable "wireguard_port" {
  description = "UDP port of the WireGuard server on the machine."
  type        = number
  default     = 51820
}

variable "client_cidrs" {
  description = "Public IPs (as /32 CIDRs) allowed to reach Sunshine directly when WireGuard is off."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for c in var.client_cidrs : can(cidrhost(c, 0))])
    error_message = "client_cidrs must be CIDR blocks, for example 203.0.113.7/32."
  }
}

variable "admin_cidrs" {
  description = "Public IPs (as /32 CIDRs) allowed to reach WinRM over HTTPS (5986), for Ansible. Empty closes WinRM."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for c in var.admin_cidrs : can(cidrhost(c, 0))])
    error_message = "admin_cidrs must be CIDR blocks, for example 203.0.113.7/32."
  }
}

variable "idle_alarm_enabled" {
  description = "Create the CloudWatch backstop alarm that stops the machine after a long idle stretch. It doesn't depend on the VM or the app."
  type        = bool
  default     = true
}

variable "idle_alarm_minutes" {
  description = "Minutes of near-zero CPU and network before the backstop alarm stops the machine. Multiple of 5."
  type        = number
  default     = 120

  validation {
    condition     = var.idle_alarm_minutes >= 15 && var.idle_alarm_minutes % 5 == 0
    error_message = "idle_alarm_minutes must be a multiple of 5 and at least 15."
  }
}

variable "idle_cpu_percent" {
  description = "Average CPU below this counts as idle."
  type        = number
  default     = 5
}

variable "idle_network_bytes" {
  description = "Network in plus out below this many bytes per 5 minutes counts as idle. The default (50 MB) is far below a stream or a download."
  type        = number
  default     = 50000000
}
