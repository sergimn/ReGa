import 'dart:async';

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

  Future<void> open(WidgetTester tester, FakeBackend backend) async {
    await tester.pumpWidget(RegaApp(backend: backend));
    await tester.pumpAndSettle();
  }

  Finder key(String k) => find.byKey(ValueKey(k));

  TextField field(WidgetTester tester, String id) =>
      tester.widget<TextField>(key('credential-$id'));

  bool enabled(WidgetTester tester, String k) =>
      tester.widget<ButtonStyleButton>(key(k)).onPressed != null;

  Future<void> fill(WidgetTester tester, String keyId, String secret) async {
    await tester.enterText(key('credential-access_key_id'), keyId);
    await tester.enterText(key('credential-secret_access_key'), secret);
    await tester.pump();
  }

  testWidgets('a first start asks for credentials', (tester) async {
    final backend = FakeBackend();
    await open(tester, backend);

    expect(find.textContaining('Demo data'), findsOneWidget);
    expect(
      find.text('Enter your AWS credentials to use your account.'),
      findsOneWidget,
    );
    expect(key('enter-credentials'), findsOneWidget);
    expect(key('sign-out'), findsNothing);
    expect(key('reconnect'), findsNothing);

    await tester.tap(key('enter-credentials'));
    await tester.pumpAndSettle();
    expect(find.text('AWS credentials'), findsOneWidget);
    expect(find.textContaining('nothing else'), findsOneWidget);
    expect(find.text('Access key ID'), findsOneWidget);
    expect(find.text('Secret access key'), findsOneWidget);
    expect(field(tester, 'access_key_id').obscureText, isFalse);
    expect(field(tester, 'secret_access_key').obscureText, isTrue);
    // Nothing to sign in with until both are there.
    expect(enabled(tester, 'credentials-save'), isFalse);
    await tester.enterText(key('credential-access_key_id'), 'typed-key-id');
    await tester.pump();
    expect(enabled(tester, 'credentials-save'), isFalse);
    await tester.enterText(key('credential-secret_access_key'), '   ');
    await tester.pump();
    expect(enabled(tester, 'credentials-save'), isFalse);
  });

  testWidgets('the secret can be shown while typing', (tester) async {
    final backend = FakeBackend();
    await open(tester, backend);
    await tester.tap(key('enter-credentials'));
    await tester.pumpAndSettle();

    await tester.tap(key('show-secret_access_key'));
    await tester.pump();
    expect(field(tester, 'secret_access_key').obscureText, isFalse);
    await tester.tap(key('show-secret_access_key'));
    await tester.pump();
    expect(field(tester, 'secret_access_key').obscureText, isTrue);
  });

  testWidgets('a refused key stays in the dialog to fix', (tester) async {
    final backend = FakeBackend();
    await open(tester, backend);
    await tester.tap(key('enter-credentials'));
    await tester.pumpAndSettle();

    await fill(tester, 'typed-key-id', 'wrong');
    await tester.tap(key('credentials-save'));
    await tester.pumpAndSettle();

    expect(backend.calls, contains('sign in typed-key-id'));
    expect(find.textContaining('InvalidClientTokenId'), findsOneWidget);
    expect(find.text('AWS credentials'), findsOneWidget);
    expect(backend.provider.demo, isTrue);

    // Fixing it signs in.
    await tester.enterText(
      key('credential-secret_access_key'),
      backend.acceptedSecret,
    );
    await tester.pump();
    await tester.tap(key('credentials-save'));
    await tester.pumpAndSettle();
    expect(find.text('AWS credentials'), findsNothing);
    expect(
      find.text('Signed in as IAM user rega, account 123456789012'),
      findsOneWidget,
    );
  });

  testWidgets('signing in swaps the demo data for the account', (tester) async {
    final backend = FakeBackend();
    final gate = Completer<void>();
    backend.signInGate = gate.future;
    await open(tester, backend);
    final reloads = backend.regionCalls;
    await tester.tap(key('enter-credentials'));
    await tester.pumpAndSettle();

    await fill(tester, ' typed-key-id ', backend.acceptedSecret);
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pump();
    // Signing in: the dialog waits and can't be cancelled halfway.
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    expect(enabled(tester, 'credentials-cancel'), isFalse);
    expect(field(tester, 'secret_access_key').enabled, isFalse);

    gate.complete();
    await tester.pumpAndSettle();
    expect(find.text('AWS credentials'), findsNothing);
    expect(find.textContaining('Demo data'), findsNothing);
    expect(
      find.text('Signed in as IAM user rega, account 123456789012'),
      findsOneWidget,
    );
    expect(key('change-credentials'), findsOneWidget);
    expect(key('sign-out'), findsOneWidget);
    expect(key('enter-credentials'), findsNothing);
    // The page starts over on the real account.
    expect(backend.regionCalls, greaterThan(reloads));
    expect(backend.calls.where((c) => c == 'connect'), hasLength(1));
  });

  testWidgets('changing credentials starts from the saved key ID', (
    tester,
  ) async {
    final backend = FakeBackend()..provider = signedIn(keyId: 'saved-key-id');
    await open(tester, backend);

    await tester.tap(key('change-credentials'));
    await tester.pumpAndSettle();
    expect(field(tester, 'access_key_id').controller!.text, 'saved-key-id');
    expect(field(tester, 'secret_access_key').controller!.text, isEmpty);

    await tester.tap(key('credentials-cancel'));
    await tester.pumpAndSettle();
    expect(find.text('AWS credentials'), findsNothing);
    expect(backend.calls.where((c) => c.startsWith('sign in')), isEmpty);
  });

  testWidgets('signing out asks first, then shows demo data', (tester) async {
    final backend = FakeBackend()..provider = signedIn();
    await open(tester, backend);

    await tester.tap(key('sign-out'));
    await tester.pumpAndSettle();
    expect(find.text('Sign out of AWS?'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(backend.calls, isNot(contains('sign out')));

    await tester.tap(key('sign-out'));
    await tester.pumpAndSettle();
    await tester.tap(key('confirm'));
    await tester.pumpAndSettle();
    expect(backend.calls, contains('sign out'));
    expect(find.textContaining('Demo data'), findsOneWidget);
    expect(find.textContaining('Signed out.'), findsOneWidget);
    expect(key('enter-credentials'), findsOneWidget);
    expect(key('sign-out'), findsNothing);
  });

  testWidgets('a failed sign-out says why', (tester) async {
    final backend = FakeBackend()
      ..provider = signedIn()
      ..signOutError = const RegaError(
        message: "Couldn't forget the credentials: the keyring is locked",
        gamesNotSaved: false,
      );
    await open(tester, backend);

    await tester.tap(key('sign-out'));
    await tester.pumpAndSettle();
    await tester.tap(key('confirm'));
    await tester.pumpAndSettle();
    expect(find.textContaining('the keyring is locked'), findsOneWidget);
    expect(
      find.text('Signed in as IAM user rega, account 123456789012'),
      findsOneWidget,
    );
  });

  testWidgets('credentials that could not be saved say so', (tester) async {
    final backend = FakeBackend()
      ..provider = signedIn(notice: 'Signed in, but not saved.');
    await open(tester, backend);
    expect(key('account-warning'), findsOneWidget);
    expect(find.text('Signed in, but not saved.'), findsOneWidget);
  });

  testWidgets('the account stays put while a machine is changing', (
    tester,
  ) async {
    final backend = FakeBackend()..provider = signedIn();
    final gate = Completer<void>();
    backend.actionGate = gate.future;
    await open(tester, backend);

    await tester.tap(key('launch'));
    await tester.pump();
    expect(enabled(tester, 'change-credentials'), isFalse);
    expect(enabled(tester, 'sign-out'), isFalse);

    gate.complete();
    await tester.pumpAndSettle();
    expect(enabled(tester, 'change-credentials'), isTrue);
    expect(enabled(tester, 'sign-out'), isTrue);
  });
}
