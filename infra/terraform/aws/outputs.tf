output "instance_id" {
  value = aws_instance.machine.id
}

output "instance_type" {
  value = aws_instance.machine.instance_type
}

output "instance_state" {
  value = aws_ec2_instance_state.machine.state
}

output "public_ip" {
  description = "Changes on every start. Empty while stopped."
  value       = data.aws_instance.current.public_ip
}

output "availability_zone" {
  description = "Pass this back as availability_zone so the machine never moves."
  value       = local.availability_zone
}

output "password_data" {
  description = "Windows Administrator password, base64 and encrypted with public_key. Decrypt it with the matching private key."
  value       = aws_instance.machine.password_data
}

output "security_group_id" {
  value = aws_security_group.machine.id
}

output "library_volume_id" {
  value = one(aws_ebs_volume.library[*].id)
}
