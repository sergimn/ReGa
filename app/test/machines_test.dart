import 'dart:async';
import 'dart:ui' show AppExitResponse;

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
      2400,
    );
    binding.platformDispatcher.views.first.devicePixelRatio = 1;
  });

  Future<void> open(WidgetTester tester, FakeBackend backend) async {
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
  }

  testWidgets('demo data says why and can reconnect', (tester) async {
    final backend = FakeBackend();
    await open(tester, backend);
    expect(find.text('No AWS credentials found.'), findsOneWidget);

    backend.provider = const ProviderInfo(id: 'aws', name: 'AWS', demo: false);
    await tester.tap(find.byKey(const ValueKey('reconnect')));
    await tester.pumpAndSettle();
    expect(backend.calls.where((c) => c == 'connect'), hasLength(2));
    expect(find.textContaining('Demo data'), findsNothing);
  });

  testWidgets('launch uses the picked region, type and toggles', (
    tester,
  ) async {
    final backend = FakeBackend();
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);

    expect(find.text('g4dn.xlarge in Europe (Stockholm), \$0.56/h'), findsOne);
    await tester.enterText(find.byKey(const ValueKey('machine-name')), 'Den');
    await tester.tap(find.byKey(const ValueKey('wireguard-toggle')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pump();

    expect(find.textContaining('Launching'), findsOneWidget);
    gate.complete();
    await tester.pumpAndSettle();
    expect(
      backend.calls,
      contains('launch Den eu-north-1 g4dn.xlarge spot=false wg=false'),
    );
    expect(find.byKey(const ValueKey('machine-new-0')), findsOneWidget);
    expect(find.text('No machines yet'), findsNothing);
  });

  testWidgets('a failed launch shows why and looks for leftovers', (
    tester,
  ) async {
    final backend = FakeBackend()
      ..actionError = const RegaError(message: 'Blocked: account');
    await open(tester, backend);
    final orphanChecks = backend.calls.where((c) => c == 'orphans').length;

    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pumpAndSettle();

    expect(find.text('Blocked: account'), findsOneWidget);
    expect(backend.calls.where((c) => c == 'orphans').length, orphanChecks + 1);
    await tester.tap(find.byTooltip('Dismiss'));
    await tester.pumpAndSettle();
    expect(find.text('Blocked: account'), findsNothing);
  });

  testWidgets('a stopped machine starts and switches type', (tester) async {
    final backend = FakeBackend(machines: const [stockholmMachine]);
    await open(tester, backend);

    expect(find.byKey(const ValueKey('stop-gaming-1')), findsNothing);
    await tester.tap(find.byKey(const ValueKey('switch-gaming-1')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('resize t3.medium gaming-1'));
    expect(find.text('Switch to GPU'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('start-gaming-1')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('start gaming-1'));
    expect(find.text('Running'), findsOneWidget);
    expect(find.byKey(const ValueKey('stop-gaming-1')), findsOneWidget);
    expect(find.byKey(const ValueKey('switch-gaming-1')), findsNothing);
  });

  testWidgets('a machine shows what it is doing', (tester) async {
    final backend = FakeBackend(machines: const [stockholmMachine]);
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);

    await tester.tap(find.byKey(const ValueKey('start-gaming-1')));
    await tester.pump();
    expect(find.text('Starting…'), findsOneWidget);
    expect(find.byKey(const ValueKey('destroy-gaming-1')), findsNothing);

    gate.complete();
    await tester.pumpAndSettle();
    expect(find.text('Starting…'), findsNothing);
  });

  testWidgets('destroy asks first', (tester) async {
    final backend = FakeBackend(machines: const [stockholmMachine]);
    await open(tester, backend);

    await tester.tap(find.byKey(const ValueKey('destroy-gaming-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(backend.calls, isNot(contains('destroy gaming-1')));

    await tester.tap(find.byKey(const ValueKey('destroy-gaming-1')));
    await tester.pumpAndSettle();
    expect(find.text('Destroy Gaming?'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('destroy gaming-1'));
    expect(find.text('No machines yet'), findsOneWidget);
  });

  testWidgets('spot machines have no stop, only destroy', (tester) async {
    final backend = FakeBackend(
      machines: const [
        MachineSummary(
          id: 'spot-1',
          name: 'Spot',
          regionId: 'eu-central-1',
          regionName: 'Europe (Frankfurt)',
          offer: gpuOffer,
          spot: true,
          status: MachineStatus.running,
          busy: false,
        ),
      ],
    );
    await open(tester, backend);
    expect(find.byKey(const ValueKey('stop-spot-1')), findsNothing);
    expect(find.byKey(const ValueKey('destroy-spot-1')), findsOneWidget);
    expect(find.textContaining('· Spot'), findsOneWidget);
  });

  testWidgets('leftovers are listed and deleted after asking', (tester) async {
    final backend = FakeBackend()..orphans = [leftoverVolume];
    await open(tester, backend);
    expect(find.text('Leftover resources'), findsOneWidget);
    expect(find.text('rega-old-library'), findsOneWidget);
    expect(find.text('Disk vol-1 · Europe (Stockholm)'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('delete-orphans')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('delete vol-1'));
    expect(find.text('Leftover resources'), findsNothing);
  });

  testWidgets('no leftovers, no section', (tester) async {
    await open(tester, FakeBackend());
    expect(find.text('Leftover resources'), findsNothing);
  });

  testWidgets('a machine busy elsewhere offers no actions', (tester) async {
    final backend = FakeBackend(
      machines: const [
        MachineSummary(
          id: 'gaming-1',
          name: 'Gaming',
          regionId: 'eu-north-1',
          regionName: 'Europe (Stockholm)',
          offer: gpuOffer,
          spot: false,
          status: MachineStatus.running,
          busy: true,
        ),
      ],
    );
    // The spinner never settles.
    await tester.pumpWidget(RegaApp(backend: backend));
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    expect(find.text('Working…'), findsOneWidget);
    expect(find.byKey(const ValueKey('stop-gaming-1')), findsNothing);
    expect(find.byKey(const ValueKey('destroy-gaming-1')), findsNothing);
  });

  testWidgets('quitting mid-change asks first', (tester) async {
    final backend = FakeBackend(machines: const [stockholmMachine]);
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);
    // Nothing running: quit right away.
    expect(await tester.binding.handleRequestAppExit(), AppExitResponse.exit);

    // A spinner runs from here on, so pump for the dialog's animation
    // instead of waiting for things to settle.
    Future<void> settle() => tester.pump(const Duration(milliseconds: 500));
    await tester.tap(find.byKey(const ValueKey('start-gaming-1')));
    await tester.pump();
    final keep = tester.binding.handleRequestAppExit();
    await settle();
    expect(find.text('Quit while a change is running?'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await settle();
    expect(await keep, AppExitResponse.cancel);

    final quit = tester.binding.handleRequestAppExit();
    await settle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await settle();
    expect(await quit, AppExitResponse.exit);
    gate.complete();
    await tester.pumpAndSettle();
  });
}
