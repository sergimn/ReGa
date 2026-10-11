locals {
  # Sunshine's default ports. Only opened when WireGuard is off; with
  # WireGuard on, Moonlight reaches them through the tunnel.
  sunshine_ports = [
    { protocol = "tcp", from = 47984, to = 47984 },
    { protocol = "tcp", from = 47989, to = 47990 },
    { protocol = "tcp", from = 48010, to = 48010 },
    { protocol = "udp", from = 47998, to = 48000 },
    { protocol = "udp", from = 48002, to = 48002 },
    { protocol = "udp", from = 48010, to = 48010 },
  ]

  ingress_rules = merge(
    var.wireguard_enabled ? {
      # The port is in the key so a new port replaces the rule: updating a
      # rule in place needs ec2:ModifySecurityGroupRules, which the policy
      # doesn't grant.
      "wireguard-${var.wireguard_port}" = { protocol = "udp", from = var.wireguard_port, to = var.wireguard_port, cidr = "0.0.0.0/0" }
    } : {},
    var.wireguard_enabled ? {} : {
      for pair in setproduct(local.sunshine_ports, var.client_cidrs) :
      "sunshine-${pair[0].protocol}-${pair[0].from}-${pair[1]}" => {
        protocol = pair[0].protocol, from = pair[0].from, to = pair[0].to, cidr = pair[1]
      }
    },
    { for cidr in var.admin_cidrs : "winrm-${cidr}" => { protocol = "tcp", from = 5986, to = 5986, cidr = cidr } },
  )
}

resource "aws_security_group" "machine" {
  name        = local.name
  description = "ReGa machine ${var.machine_id}"
  vpc_id      = data.aws_vpc.default.id

  tags = {
    Name = local.name
  }
}

resource "aws_vpc_security_group_ingress_rule" "machine" {
  for_each = local.ingress_rules

  security_group_id = aws_security_group.machine.id
  description       = each.key
  ip_protocol       = each.value.protocol
  from_port         = each.value.from
  to_port           = each.value.to
  cidr_ipv4         = each.value.cidr
}

resource "aws_vpc_security_group_egress_rule" "all" {
  security_group_id = aws_security_group.machine.id
  description       = "all outbound"
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}
