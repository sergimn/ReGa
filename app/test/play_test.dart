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

  final running = withStatus(stockholmMachine, MachineStatus.running);
  const tunnelNote = 'Streaming through WireGuard';

  Future<FakeBackend> open(
    WidgetTester tester, {
    List<MachineSummary>? machines,
  }) async {
    final backend = FakeBackend(machines: machines ?? [running]);
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
    return backend;
  }

  Future<void> play(WidgetTester tester) async {
    await tester.tap(find.byKey(const ValueKey('play-gaming-1')));
    await tester.pumpAndSettle();
  }

  VoidCallback? onPressed(WidgetTester tester, String key) =>
      tester.widget<ButtonStyleButton>(find.byKey(ValueKey(key))).onPressed;

  testWidgets('only running machines have Play', (tester) async {
    await open(
      tester,
      machines: [
        stockholmMachine,
        withStatus(
          MachineSummary(
            id: 'den-1',
            name: 'Den',
            regionId: 'eu-north-1',
            regionName: 'Europe (Stockholm)',
            offer: cpuOffer,
            spot: false,
            status: MachineStatus.running,
            busy: false,
            starting: false,
            stoppedItself: false,
            libraryKept: false,
          ),
          MachineStatus.running,
        ),
      ],
    );
    expect(find.byKey(const ValueKey('play-gaming-1')), findsNothing);
    expect(find.byKey(const ValueKey('play-den-1')), findsOneWidget);
    expect(find.byKey(const ValueKey('disconnect-den-1')), findsNothing);
    expect(find.textContaining(tunnelNote), findsNothing);
  });

  testWidgets('Play shows each step, then the tunnel and Disconnect', (
    tester,
  ) async {
    final backend = FakeBackend(machines: [running]);
    final updates = StreamController<PlayUpdate>();
    backend.playUpdates = updates;
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('play-gaming-1')));
    await tester.pump();
    expect(backend.calls, contains('play gaming-1'));
    expect(find.text('Connecting…'), findsOneWidget);
    expect(find.byKey(const ValueKey('stop-gaming-1')), findsNothing);
    // Nothing else starts meanwhile.
    expect(onPressed(tester, 'launch'), isNull);

    for (final (step, label) in [
      (PlayStep.pairing, 'Pairing…'),
      (PlayStep.startingMoonlight, 'Starting Moonlight…'),
    ]) {
      updates.add(PlayUpdate(step: step, paired: false));
      await tester.pump();
      expect(find.text(label), findsOneWidget);
    }
    updates.add(const PlayUpdate(step: PlayStep.done, paired: false));
    await updates.close();
    await tester.pumpAndSettle();

    expect(find.text('Starting Moonlight…'), findsNothing);
    expect(find.text('Running'), findsOneWidget);
    // Moonlight streams on its own: no PIN to ask for.
    expect(find.byKey(const ValueKey('pair-dialog')), findsNothing);
    expect(find.textContaining(tunnelNote), findsOneWidget);
    expect(find.byKey(const ValueKey('disconnect-gaming-1')), findsOneWidget);
    expect(onPressed(tester, 'launch'), isNotNull);
  });

  testWidgets('Play without Moonlight asks for its PIN until it is right', (
    tester,
  ) async {
    final backend = await open(tester);
    backend.moonlightHost = '127.0.0.1';
    await play(tester);

    expect(find.byKey(const ValueKey('pair-dialog')), findsOneWidget);
    expect(find.text('Play Gaming in Moonlight'), findsOneWidget);
    expect(find.textContaining('add the host 127.0.0.1'), findsOneWidget);
    // The steps that need Moonlight are skipped.
    expect(find.textContaining(tunnelNote), findsOneWidget);

    await tester.enterText(find.byKey(const ValueKey('pin')), '0000');
    await tester.tap(find.byKey(const ValueKey('pair-submit')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('pin gaming-1 0000'));
    expect(
      find.text("Sunshine didn't accept that PIN."),
      findsOneWidget,
      reason: 'a wrong PIN says so and keeps the field',
    );
    expect(find.byKey(const ValueKey('pin')), findsOneWidget);

    await tester.enterText(find.byKey(const ValueKey('pin')), '1234');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(backend.calls, contains('pin gaming-1 1234'));
    expect(find.byKey(const ValueKey('pair-error')), findsNothing);
    expect(find.byKey(const ValueKey('pin')), findsNothing);
    expect(
      find.textContaining('Paired. In Moonlight, open 127.0.0.1'),
      findsOne,
    );

    await tester.tap(find.byKey(const ValueKey('pair-close')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('pair-dialog')), findsNothing);
    // The tunnel stays up for Moonlight.
    expect(find.byKey(const ValueKey('disconnect-gaming-1')), findsOneWidget);
  });

  testWidgets('a device Sunshine knows can still pair again', (tester) async {
    final backend = await open(tester);
    backend
      ..moonlightHost = '127.0.0.1'
      ..paired = true;
    await play(tester);

    expect(find.byKey(const ValueKey('pin')), findsNothing);
    expect(find.textContaining('Paired.'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('pair-again')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('pin')), findsOneWidget);
    expect(find.byKey(const ValueKey('pair-again')), findsNothing);

    await tester.tap(find.byKey(const ValueKey('pair-close')));
    await tester.pumpAndSettle();
    expect(backend.calls.where((c) => c.startsWith('pin ')), isEmpty);
  });

  testWidgets('a machine without WireGuard streams with no tunnel', (
    tester,
  ) async {
    final backend = await open(tester);
    backend
      ..wireguard = false
      ..moonlightHost = '203.0.113.7';
    await play(tester);

    expect(find.textContaining('add the host 203.0.113.7'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('pair-close')));
    await tester.pumpAndSettle();
    expect(find.textContaining(tunnelNote), findsNothing);
    expect(find.byKey(const ValueKey('disconnect-gaming-1')), findsNothing);
    // Nothing to lose by quitting.
    expect(await tester.binding.handleRequestAppExit(), AppExitResponse.exit);
  });

  testWidgets('a failed Play says why and leaves no tunnel', (tester) async {
    final backend = await open(tester);
    backend.playError = const RegaError(
      message: 'The machine didn\'t answer over WireGuard within 20 seconds',
      gamesNotSaved: false,
    );
    await play(tester);

    expect(
      find.text('The machine didn\'t answer over WireGuard within 20 seconds'),
      findsOneWidget,
    );
    expect(find.byKey(const ValueKey('pair-dialog')), findsNothing);
    expect(find.textContaining(tunnelNote), findsNothing);
    expect(find.text('Running'), findsOneWidget);
    expect(onPressed(tester, 'play-gaming-1'), isNotNull);
  });

  testWidgets('Disconnect closes the tunnel', (tester) async {
    final backend = await open(tester);
    await play(tester);
    expect(find.textContaining(tunnelNote), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('disconnect-gaming-1')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('close tunnel'));
    expect(backend.stream, isNull);
    expect(find.textContaining(tunnelNote), findsNothing);
    expect(find.byKey(const ValueKey('disconnect-gaming-1')), findsNothing);
    expect(find.byKey(const ValueKey('play-gaming-1')), findsOneWidget);
  });

  testWidgets('stopping the machine ends the tunnel to it', (tester) async {
    final backend = await open(tester);
    await play(tester);

    await tester.tap(find.byKey(const ValueKey('stop-gaming-1')));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('stop gaming-1'));
    expect(find.text('Stopped'), findsOneWidget);
    expect(find.textContaining(tunnelNote), findsNothing);
  });

  testWidgets('a machine that stops itself has its tunnel closed', (
    tester,
  ) async {
    final backend = await open(tester);
    await play(tester);
    expect(backend.calls, isNot(contains('close tunnel')));

    // Idle: the machine shuts down on its own.
    backend.machines[0] = withStatus(
      stockholmMachine,
      MachineStatus.stopped,
      stoppedItself: true,
    );
    await tester.pump(const Duration(minutes: 1));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('close tunnel'));
    expect(backend.stream, isNull);
    expect(find.textContaining(tunnelNote), findsNothing);
  });

  testWidgets('quitting with the tunnel up asks first', (tester) async {
    await open(tester);
    expect(await tester.binding.handleRequestAppExit(), AppExitResponse.exit);
    await play(tester);

    final keep = tester.binding.handleRequestAppExit();
    await tester.pumpAndSettle();
    expect(find.text('Quit and end the stream?'), findsOneWidget);
    expect(find.textContaining('The stream from Gaming'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(await keep, AppExitResponse.cancel);

    final quit = tester.binding.handleRequestAppExit();
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('confirm')));
    await tester.pumpAndSettle();
    expect(await quit, AppExitResponse.exit);
  });
}
