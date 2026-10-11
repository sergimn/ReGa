/// Display helpers shared by the widgets.
library;

String formatPrice(double? perHour) {
  if (perHour == null) return 'Not offered';
  // Cheap CPU types need a third decimal to tell them apart.
  final digits = perHour < 0.1 ? 3 : 2;
  return '\$${perHour.toStringAsFixed(digits)}/h';
}

String formatGb(num gib) => gib == gib.roundToDouble()
    ? '${gib.round()} GB'
    : '${gib.toStringAsFixed(1)} GB';

String formatRtt(int? ms) => ms == null ? 'Measuring…' : '$ms ms';
