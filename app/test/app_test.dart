import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/app.dart';
import 'package:rega/backend.dart';

import 'fake_backend.dart';

void main() {
  setUp(() {
    final binding = TestWidgetsFlutterBinding.ensureInitialized();
    binding.platformDispatcher.views.first.physicalSize = const Size(
      1400,
      1800,
    );
    binding.platformDispatcher.views.first.devicePixelRatio = 1;
  });

  testWidgets('side menu lists platforms and the page loads', (tester) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    expect(find.byType(NavigationRail), findsOneWidget);
    expect(find.text('Platforms'), findsOneWidget);
    expect(find.text('AWS'), findsWidgets);
    expect(find.textContaining('Demo data'), findsOneWidget);
    expect(find.text('No machines yet'), findsOneWidget);
    expect(backend.calls, contains('probe aws'));
    // The recommended region is selected and machines are priced for it.
    expect(backend.calls, contains('offers eu-north-1'));
    expect(find.byKey(const ValueKey('offer-t3.medium')), findsOneWidget);
  });

  testWidgets('Spot shows only GPU types', (tester) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();

    expect(backend.lastSpot, isTrue);
    expect(find.byKey(const ValueKey('offer-g4dn.xlarge')), findsOneWidget);
    expect(find.byKey(const ValueKey('offer-t3.medium')), findsNothing);
  });

  testWidgets('picking a machine reprices regions for it', (tester) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('offer-t3.medium')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('regions t3.medium'));
  });

  testWidgets('existing machines are listed', (tester) async {
    final backend = FakeBackend(
      machines: const [
        MachineSummary(
          id: 'i-1',
          name: 'Gaming',
          regionId: 'eu-north-1',
          offer: gpuOffer,
          spot: false,
          status: MachineStatus.stopped,
        ),
      ],
    );
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    expect(find.text('Gaming'), findsOneWidget);
    expect(find.text('stopped'), findsOneWidget);
  });
}
