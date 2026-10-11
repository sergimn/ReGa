# Spot GPU machine. Spot can't be stopped or resized, so it boots straight to
# the GPU type and is destroyed when the session ends.
region            = "eu-central-1"
machine_id        = "spot1"
instance_type     = "g4dn.xlarge"
spot              = true
public_key        = "ssh-rsa AAAA... rega"
wireguard_enabled = false
client_cidrs      = ["203.0.113.7/32"]
