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

  testWidgets('Spot after picking a CPU type falls back to GPU', (
    tester,
  ) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('offer-t3.medium')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('offer-g4dn.xlarge')), findsOneWidget);
    expect(find.textContaining('Recommended'), findsOneWidget);
  });

  testWidgets('a slow older reload does not overwrite a newer one', (
    tester,
  ) async {
    final backend = FakeBackend(
      onDemandDelay: const Duration(milliseconds: 500),
    );
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pump();
    // The first (on-demand) reload is still waiting; turn Spot on now.
    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();

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

  testWidgets('picking a region reprices machines without reloading regions', (
    tester,
  ) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
    final before = backend.regionCalls;

    await tester.tap(find.text('Lowest latency'));
    await tester.pumpAndSettle();

    expect(backend.calls.last, 'offers eu-central-1');
    expect(backend.regionCalls, before);
  });

  testWidgets('our pick follows Recommended; a user pick sticks', (
    tester,
  ) async {
    final backend = FakeBackend();
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('offers eu-north-1'));

    backend.recommended = virginia;
    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();
    expect(backend.calls.last, 'offers us-east-1');

    await tester.tap(find.text('Lowest latency'));
    await tester.pumpAndSettle();
    backend.recommended = stockholm;
    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();
    expect(backend.calls.last, 'offers eu-central-1');
  });

  testWidgets('new latency results refresh the regions', (tester) async {
    final backend = FakeBackend()..latencyChanges = true;
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
    expect(backend.regionCalls, 2);

    await tester.tap(find.text('Show all regions'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Latency unknown'), findsOneWidget);
    expect(find.textContaining('Measuring'), findsNothing);
  });

  testWidgets('errors show their message and Retry recovers', (tester) async {
    final backend = FakeBackend()
      ..regionsError = const RegaError(message: 'AWS said no');
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
    expect(find.text('AWS said no'), findsOneWidget);

    backend.regionsError = null;
    await tester.tap(find.text('Retry'));
    await tester.pumpAndSettle();
    expect(find.text('AWS said no'), findsNothing);
    expect(find.textContaining('Recommended'), findsOneWidget);
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
