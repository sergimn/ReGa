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

  String lastLaunch(FakeBackend backend) =>
      backend.calls.lastWhere((c) => c.startsWith('launch '));

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
      contains(
        'launch Den eu-north-1 g4dn.xlarge spot=false wg=false cpu=true',
      ),
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
  });

  testWidgets('switching a running machine asks first and sets it up again', (
    tester,
  ) async {
    final backend = FakeBackend(
      machines: [withStatus(stockholmMachine, MachineStatus.running)],
    );
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);
    expect(find.byTooltip('t3.medium at \$0.045/h'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('switch-gaming-1')));
    await tester.pumpAndSettle();
    expect(find.text('Switch Gaming to t3.medium?'), findsOneWidget);
    expect(find.textContaining('t3.medium at \$0.045/h'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(backend.calls, isNot(contains('resize t3.medium gaming-1')));

    await tester.tap(find.byKey(const ValueKey('switch-gaming-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pump();
    expect(find.text('Switching and setting up…'), findsOneWidget);
    gate.complete();
    await tester.pumpAndSettle();
    expect(backend.calls, contains('resize t3.medium gaming-1'));
    expect(find.text('Running'), findsOneWidget);
  });

  testWidgets('stopping says it saves the games first', (tester) async {
    final backend = FakeBackend(
      machines: [withStatus(stockholmMachine, MachineStatus.running)],
    );
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);
    await tester.tap(find.byKey(const ValueKey('stop-gaming-1')));
    await tester.pump();
    expect(find.text('Saving games and stopping…'), findsOneWidget);
    gate.complete();
    await tester.pumpAndSettle();
    expect(find.text('Stopped'), findsOneWidget);
  });

  testWidgets('GPU machines set up on a CPU type first, unless turned off', (
    tester,
  ) async {
    final backend = FakeBackend();
    await open(tester, backend);
    final toggle = find.byKey(const ValueKey('cpu-first-toggle'));
    expect(toggle, findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pumpAndSettle();
    expect(
      lastLaunch(backend),
      endsWith('g4dn.xlarge spot=false wg=true cpu=true'),
    );

    await tester.tap(toggle);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('destroy-new-0')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pumpAndSettle();
    expect(lastLaunch(backend), endsWith('cpu=false'));
  });

  testWidgets('no CPU phase for spot or CPU machines', (tester) async {
    final backend = FakeBackend();
    await open(tester, backend);
    final toggle = find.byKey(const ValueKey('cpu-first-toggle'));

    await tester.tap(find.byKey(const ValueKey('offer-t3.medium')));
    await tester.pumpAndSettle();
    expect(toggle, findsNothing);
    await tester.tap(find.byKey(const ValueKey('offer-g4dn.xlarge')));
    await tester.pumpAndSettle();
    expect(toggle, findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('spot-toggle')));
    await tester.pumpAndSettle();
    expect(toggle, findsNothing);
    await tester.tap(find.byKey(const ValueKey('launch')));
    await tester.pumpAndSettle();
    expect(lastLaunch(backend), endsWith('spot=true wg=true cpu=false'));
  });

  testWidgets('a machine that stopped itself has its stop finished once', (
    tester,
  ) async {
    final backend = FakeBackend(
      machines: [
        withStatus(
          stockholmMachine,
          MachineStatus.stopped,
          stoppedItself: true,
        ),
      ],
    )..actionError = const RegaError(message: 'Terraform failed');
    await open(tester, backend);
    expect(backend.calls.where((c) => c == 'stop gaming-1'), hasLength(1));
    expect(find.text('Terraform failed'), findsOneWidget);

    // A failing finish isn't retried on every reload.
    await tester.tap(find.byKey(const ValueKey('offer-t3.medium')));
    await tester.pumpAndSettle();
    expect(backend.calls.where((c) => c == 'stop gaming-1'), hasLength(1));
  });

  testWidgets('a running machine is watched for stopping itself', (
    tester,
  ) async {
    final backend = FakeBackend(
      machines: [withStatus(stockholmMachine, MachineStatus.running)],
    );
    await open(tester, backend);
    expect(backend.calls, isNot(contains('stop gaming-1')));

    // Idle: the machine saves its games and shuts down on its own.
    backend.machines[0] = withStatus(
      stockholmMachine,
      MachineStatus.stopped,
      stoppedItself: true,
    );
    await tester.pump(const Duration(minutes: 1));
    await tester.pumpAndSettle();
    expect(backend.calls.where((c) => c == 'stop gaming-1'), hasLength(1));
    expect(find.text('Stopped'), findsOneWidget);

    // Started again, it can stop itself again later.
    await tester.tap(find.byKey(const ValueKey('start-gaming-1')));
    await tester.pumpAndSettle();
    backend.machines[0] = withStatus(
      stockholmMachine,
      MachineStatus.stopped,
      stoppedItself: true,
    );
    await tester.pump(const Duration(minutes: 1));
    await tester.pumpAndSettle();
    expect(backend.calls.where((c) => c == 'stop gaming-1'), hasLength(2));
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
          starting: false,
          stoppedItself: false,
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

  testWidgets('a machine busy elsewhere waits, then comes back', (
    tester,
  ) async {
    MachineSummary gaming({required bool busy}) => MachineSummary(
      id: 'gaming-1',
      name: 'Gaming',
      regionId: 'eu-north-1',
      regionName: 'Europe (Stockholm)',
      offer: gpuOffer,
      spot: false,
      status: MachineStatus.running,
      busy: busy,
      starting: busy,
      stoppedItself: false,
    );
    final backend = FakeBackend(machines: [gaming(busy: true)]);
    // The spinner never settles.
    await tester.pumpWidget(RegaApp(backend: backend));
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    expect(find.text('Working…'), findsOneWidget);
    expect(find.byKey(const ValueKey('stop-gaming-1')), findsNothing);
    expect(find.byKey(const ValueKey('destroy-gaming-1')), findsNothing);
    expect(
      tester.widget<ButtonStyleButton>(find.byKey(const ValueKey('launch'))),
      isA<ButtonStyleButton>().having((b) => b.onPressed, 'onPressed', null),
    );

    // The other device finishes; the next check picks that up.
    backend.machines[0] = gaming(busy: false);
    await tester.pump(const Duration(seconds: 15));
    await tester.pumpAndSettle();
    expect(find.text('Working…'), findsNothing);
    expect(find.byKey(const ValueKey('stop-gaming-1')), findsOneWidget);
  });

  testWidgets('nothing else starts while a machine starts', (tester) async {
    final backend = FakeBackend(
      machines: [
        stockholmMachine,
        MachineSummary(
          id: 'den-1',
          name: 'Den',
          regionId: 'eu-north-1',
          regionName: 'Europe (Stockholm)',
          offer: cpuOffer,
          spot: false,
          status: MachineStatus.stopped,
          busy: false,
          starting: false,
          switchType: 'g4dn.xlarge',
          stoppedItself: false,
        ),
      ],
    );
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);
    VoidCallback? onPressed(String key) =>
        tester.widget<ButtonStyleButton>(find.byKey(ValueKey(key))).onPressed;
    expect(onPressed('start-den-1'), isNotNull);

    await tester.tap(find.byKey(const ValueKey('start-gaming-1')));
    await tester.pump();
    expect(onPressed('start-den-1'), isNull);
    expect(onPressed('switch-den-1'), isNull);
    expect(onPressed('launch'), isNull);
    // Stopping or destroying another machine is still fine.
    expect(onPressed('destroy-den-1'), isNotNull);

    gate.complete();
    await tester.pumpAndSettle();
    expect(onPressed('start-den-1'), isNotNull);
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

  testWidgets('a machine stopping elsewhere blocks no other start', (
    tester,
  ) async {
    final backend = FakeBackend(
      machines: [
        stockholmMachine,
        MachineSummary(
          id: 'den-1',
          name: 'Den',
          regionId: 'eu-north-1',
          regionName: 'Europe (Stockholm)',
          offer: cpuOffer,
          spot: false,
          status: MachineStatus.stopped,
          busy: true,
          starting: false,
          stoppedItself: false,
        ),
      ],
    );
    await tester.pumpWidget(RegaApp(backend: backend));
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    expect(find.text('Working…'), findsOneWidget);
    expect(
      tester
          .widget<ButtonStyleButton>(
            find.byKey(const ValueKey('start-gaming-1')),
          )
          .onPressed,
      isNotNull,
    );
  });

  testWidgets('machines that can\'t be listed leave the rest of the page', (
    tester,
  ) async {
    final backend = FakeBackend()
      ..listMachinesError =
          'bucket rega-tfstate-1 doesn\'t block public access';
    await open(tester, backend);
    expect(find.byKey(const ValueKey('offer-g4dn.xlarge')), findsOneWidget);
    expect(find.byKey(const ValueKey('machines-error')), findsOneWidget);
    expect(find.text('No machines yet'), findsNothing);
    expect(
      tester
          .widget<ButtonStyleButton>(find.byKey(const ValueKey('launch')))
          .onPressed,
      isNull,
    );

    backend.listMachinesError = null;
    await tester.tap(
      find.descendant(
        of: find.byKey(const ValueKey('machines-error')),
        matching: find.text('Retry'),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('machines-error')), findsNothing);
    expect(find.text('No machines yet'), findsOneWidget);
  });
}
