import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:rega/app.dart';
import 'package:rega/backend.dart';
import 'package:rega/src/rust/frb_generated.dart';

/// Runs the real app against the Rust core (demo provider).
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await RustLib.init());

  testWidgets('loads AWS regions and machine types from Rust', (tester) async {
    await tester.pumpWidget(RegaApp(backend: RustBackend()));
    await tester.pumpAndSettle();

    expect(find.textContaining('Recommended'), findsOneWidget);
    expect(find.byKey(const ValueKey('offer-g4dn.xlarge')), findsOneWidget);
    await tester.scrollUntilVisible(find.text('No machines yet'), 200);
    expect(find.text('No machines yet'), findsOneWidget);
  });
}
