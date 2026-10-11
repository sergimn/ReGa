import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:rega/app.dart';
import 'package:rega/backend.dart';
import 'package:rega/src/rust/frb_generated.dart';

/// Runs the real app against the Rust core. Without AWS credentials (as in
/// CI) the core falls back to the demo provider; run it with `REGA_DEMO=1`
/// when you have credentials.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // The page's own list; the name field has a Scrollable of its own.
  final page = find
      .descendant(
        of: find.byKey(const ValueKey('provider-page')),
        matching: find.byType(Scrollable),
      )
      .first;
  setUpAll(() async => await RustLib.init());

  testWidgets('loads AWS regions and machine types from Rust', (tester) async {
    await tester.pumpWidget(RegaApp(backend: RustBackend()));
    await tester.pumpAndSettle();

    expect(find.textContaining('Recommended'), findsOneWidget);
    expect(find.byKey(const ValueKey('offer-g4dn.xlarge')), findsOneWidget);
    await tester.scrollUntilVisible(
      find.text('No machines yet'),
      200,
      scrollable: page,
    );
    expect(find.text('No machines yet'), findsOneWidget);
  });

  testWidgets('launches, stops and destroys a demo machine', (tester) async {
    await tester.pumpWidget(RegaApp(backend: RustBackend()));
    await tester.pumpAndSettle();
    expect(find.textContaining('Demo data'), findsOneWidget);

    await tester.enterText(find.byKey(const ValueKey('machine-name')), 'E2E');
    await tester.scrollUntilVisible(
      find.byKey(const ValueKey('launch')),
      200,
      scrollable: page,
    );
    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pumpAndSettle();
    await tester.scrollUntilVisible(
      find.text('Running'),
      200,
      scrollable: page,
    );
    expect(find.text('E2E'), findsWidgets);

    final stop = find.byWidgetPredicate(
      (w) =>
          w.key is ValueKey<String> &&
          (w.key! as ValueKey<String>).value.startsWith('stop-'),
    );
    await tester.tap(stop);
    await tester.pumpAndSettle();
    expect(find.text('Stopped'), findsOneWidget);

    final destroy = find.byWidgetPredicate(
      (w) =>
          w.key is ValueKey<String> &&
          (w.key! as ValueKey<String>).value.startsWith('destroy-'),
    );
    await tester.tap(destroy);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pumpAndSettle();
    expect(find.text('No machines yet'), findsOneWidget);
  });
}
