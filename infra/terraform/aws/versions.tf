terraform {
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # State lives in the per-account bucket rega-tfstate-<account-id>, which the
  # app creates on first run. bucket, key and region are passed at init time:
  #   terraform init \
  #     -backend-config="bucket=rega-tfstate-<account-id>" \
  #     -backend-config="key=machines/<machine-id>.tfstate" \
  #     -backend-config="region=eu-north-1"
  # use_lockfile keeps a lock object next to the state, so two devices can't
  # change the same machine at once.
  backend "s3" {
    use_lockfile = true
    encrypt      = true
  }
}

provider "aws" {
  region = var.region

  # Every resource carries Project=ReGa so the app can list machines and clean
  # up orphans across regions.
  default_tags {
    tags = local.tags
  }
}
