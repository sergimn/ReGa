/// Display helpers shared by the widgets.
library;

import '../backend.dart';

String formatPrice(double? perHour) {
  if (perHour == null) return 'Not offered';
  // Cheap CPU types need a third decimal to tell them apart.
  final digits = perHour < 0.1 ? 3 : 2;
  return '\$${perHour.toStringAsFixed(digits)}/h';
}

String formatGb(num gib) => gib == gib.roundToDouble()
    ? '${gib.round()} GB'
    : '${gib.toStringAsFixed(1)} GB';

/// `measuring` says whether a probe is still running for missing values.
String formatRtt(int? ms, {bool measuring = true}) => ms != null
    ? '$ms ms'
    : measuring
    ? 'Measuring…'
    : 'Latency unknown';

String formatStatus(MachineStatus status) => switch (status) {
  MachineStatus.pending => 'Starting',
  MachineStatus.running => 'Running',
  MachineStatus.stopping => 'Stopping',
  MachineStatus.stopped => 'Stopped',
  MachineStatus.terminated => 'Destroyed',
};
