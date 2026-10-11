# Backstop for the in-VM idle auto-stop: if CPU and network both stay near
# zero for idle_alarm_minutes, CloudWatch stops the instance. It works even if
# the VM agent or the app is gone. Spot instances can't be stopped, so for
# them the alarm terminates instead (the game library is safe in S3).
resource "aws_cloudwatch_metric_alarm" "idle_stop" {
  count = var.idle_alarm_enabled ? 1 : 0

  alarm_name          = "${local.name}-idle-stop"
  alarm_description   = "${var.spot ? "Terminates" : "Stops"} ReGa machine ${var.machine_id} after ${var.idle_alarm_minutes} idle minutes."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = 1
  evaluation_periods  = var.idle_alarm_minutes / 5
  datapoints_to_alarm = var.idle_alarm_minutes / 5
  treat_missing_data  = "notBreaching"
  alarm_actions       = ["arn:aws:automate:${var.region}:ec2:${var.spot ? "terminate" : "stop"}"]

  metric_query {
    id          = "idle"
    expression  = "IF(cpu < ${var.idle_cpu_percent} AND (net_in + net_out) < ${var.idle_network_bytes}, 1, 0)"
    label       = "Idle"
    return_data = true
  }

  metric_query {
    id = "cpu"
    metric {
      namespace   = "AWS/EC2"
      metric_name = "CPUUtilization"
      period      = 300
      stat        = "Average"
      dimensions  = { InstanceId = aws_instance.machine.id }
    }
  }

  metric_query {
    id = "net_in"
    metric {
      namespace   = "AWS/EC2"
      metric_name = "NetworkIn"
      period      = 300
      stat        = "Sum"
      dimensions  = { InstanceId = aws_instance.machine.id }
    }
  }

  metric_query {
    id = "net_out"
    metric {
      namespace   = "AWS/EC2"
      metric_name = "NetworkOut"
      period      = 300
      stat        = "Sum"
      dimensions  = { InstanceId = aws_instance.machine.id }
    }
  }
}
