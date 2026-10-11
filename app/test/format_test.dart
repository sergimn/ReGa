import 'package:flutter_test/flutter_test.dart';
import 'package:rega/backend.dart';
import 'package:rega/widgets/format.dart';

void main() {
  test('prices show two decimals per hour', () {
    expect(formatPrice(0.556), '\$0.56/h');
    expect(formatPrice(0.045), '\$0.045/h');
    expect(formatPrice(null), 'Not offered');
  });

  test('sizes drop the decimal when whole', () {
    expect(formatGb(16), '16 GB');
    expect(formatGb(16.0), '16 GB');
    expect(formatGb(0.5), '0.5 GB');
  });

  test('latency shows a placeholder until measured', () {
    expect(formatRtt(38), '38 ms');
    expect(formatRtt(null), 'Measuring…');
    expect(formatRtt(null, measuring: false), 'Latency unknown');
  });

  test('machine states read as words', () {
    expect(formatStatus(MachineStatus.pending), 'Starting');
    expect(formatStatus(MachineStatus.stopped), 'Stopped');
    expect(formatStatus(MachineStatus.terminated), 'Destroyed');
  });
}
