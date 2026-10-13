import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/app.dart';
import 'package:rega/licences.dart';
import 'package:rega/widgets/about.dart';

import 'fake_backend.dart';

/// The app's assets, with the version.json a build of it has.
class _BuiltAssets extends CachingAssetBundle {
  @override
  Future<ByteData> load(String key) async {
    if (key == 'version.json') {
      final json = '{"app_name":"rega","version":"1.2.3","build_number":"4"}';
      return ByteData.sublistView(utf8.encode(json));
    }
    return rootBundle.load(key);
  }
}

void main() {
  setUp(() {
    final binding = TestWidgetsFlutterBinding.ensureInitialized();
    binding.platformDispatcher.views.first.physicalSize = const Size(
      1400,
      1800,
    );
    binding.platformDispatcher.views.first.devicePixelRatio = 1;
  });

  test('the version comes from version.json, if the build has one', () async {
    expect(await appVersion(_BuiltAssets()), '1.2.3');
    expect(await appVersion(rootBundle), isNull);
  });

  testWidgets('About shows the version and leads to every licence', (
    tester,
  ) async {
    // What the licence list reads, with a Rust crate's notice in place of
    // the files the Flatpak ships.
    LicenseRegistry.reset();
    addTearDown(LicenseRegistry.reset);
    final entries = [
      // Reads the disk, so outside the test's fake clock.
      ...(await tester.runAsync(() => regaLicences('/nonexistent').toList()))!,
      ...rustCrateLicences('''
Crates
${'-' * 78}

serde 1.0.228  MIT OR Apache-2.0


Licence texts
${'-' * 78}

${'=' * 78}
serde 1.0.228: LICENSE-MIT
${'-' * 78}

Copyright (c) 2014 The serde developers
'''),
      const LicenseEntryWithLineBreaks(['flutter'], 'Flutter\'s own'),
    ];
    LicenseRegistry.addLicense(() => Stream.fromIterable(entries));

    await tester.pumpWidget(
      DefaultAssetBundle(
        bundle: _BuiltAssets(),
        child: RegaApp(backend: FakeBackend()),
      ),
    );
    await tester.pumpAndSettle();

    final about = find.descendant(
      of: find.byType(NavigationRail),
      matching: find.byKey(const ValueKey('about')),
    );
    expect(about, findsOneWidget);
    expect(find.byTooltip('About ReGa'), findsOneWidget);
    await tester.tap(about);
    await tester.pumpAndSettle();

    expect(find.byType(AboutDialog), findsOneWidget);
    expect(find.text('ReGa'), findsWidgets);
    expect(find.text('1.2.3'), findsOneWidget);
    expect(find.text(regaLegalese), findsOneWidget);

    await tester.tap(find.text('View licenses'));
    await tester.pumpAndSettle();
    expect(find.byType(LicensePage), findsOneWidget);
    for (final package in ['ReGa', 'serde', 'flutter', 'Rust crates']) {
      expect(
        find.widgetWithText(ListTile, package),
        findsOneWidget,
        reason: package,
      );
    }

    await tester.tap(find.widgetWithText(ListTile, 'ReGa'));
    await tester.pumpAndSettle();
    expect(
      find.textContaining('Permission is hereby granted, free of charge'),
      findsOneWidget,
    );

    await tester.tap(find.widgetWithText(ListTile, 'serde'));
    await tester.pumpAndSettle();
    expect(
      find.text(
        'serde 1.0.228, a Rust crate in ReGa\'s core, is under '
        'MIT OR Apache-2.0.',
      ),
      findsOneWidget,
    );
    expect(
      find.text('Copyright (c) 2014 The serde developers'),
      findsOneWidget,
    );
  });

  testWidgets('without a version.json About leaves the version out', (
    tester,
  ) async {
    await tester.pumpWidget(RegaApp(backend: FakeBackend()));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('about')));
    await tester.pumpAndSettle();

    expect(find.byType(AboutDialog), findsOneWidget);
    expect(find.text(regaLegalese), findsOneWidget);
    await tester.tap(find.text('Close'));
    await tester.pumpAndSettle();
    expect(find.byType(AboutDialog), findsNothing);
  });
}
