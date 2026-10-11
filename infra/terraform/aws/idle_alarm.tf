# Backstop for the in-VM idle auto-stop: if CPU stays near zero for
# idle_alarm_minutes, CloudWatch stops the instance. It works even if the VM
# agent or the app is gone. EC2 actions only run on alarms over a single
# per-instance metric, not metric math, so this watches CPU alone; a stream or
# a download should keep it above the threshold. Spot instances can't be
# stopped, so for them the alarm terminates instead (the game library is safe
# in S3).
resource "aws_cloudwatch_metric_alarm" "idle_stop" {
  count = var.idle_alarm_enabled ? 1 : 0

  alarm_name          = "${local.name}-idle-stop"
  alarm_description   = "${var.spot ? "Terminates" : "Stops"} ReGa machine ${var.machine_id} after ${var.idle_alarm_minutes} idle minutes."
  namespace           = "AWS/EC2"
  metric_name         = "CPUUtilization"
  dimensions          = { InstanceId = aws_instance.machine.id }
  statistic           = "Maximum"
  period              = 300
  comparison_operator = "LessThanThreshold"
  threshold           = var.idle_cpu_percent
  evaluation_periods  = var.idle_alarm_minutes / 5
  datapoints_to_alarm = var.idle_alarm_minutes / 5
  treat_missing_data  = "notBreaching"
  alarm_actions       = ["arn:aws:automate:${var.region}:ec2:${var.spot ? "terminate" : "stop"}"]
}
