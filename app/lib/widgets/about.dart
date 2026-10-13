import 'dart:convert';

import 'package:flutter/material.dart';

/// Under ReGa's name in About and on the licence page.
const regaLegalese = 'Copyright (c) 2026 Sergi Miralles\nMIT licence';

/// ReGa's version, from the version.json `flutter build` puts in the app's
/// assets (pubspec.yaml's, without the build number); null without one, as
/// in widget tests.
Future<String?> appVersion(AssetBundle bundle) async {
  try {
    final info = jsonDecode(
      await bundle.loadString('version.json', cache: false),
    );
    return (info as Map<String, dynamic>)['version'] as String?;
  } catch (_) {
    return null;
  }
}

/// ReGa's About box: name, version and copyright, with a button to the
/// licence page, which lists ReGa's licence and those of everything it is
/// built with (see licences.dart).
Future<void> showRegaAbout(BuildContext context) async {
  final version = await appVersion(DefaultAssetBundle.of(context));
  if (!context.mounted) return;
  showAboutDialog(
    context: context,
    applicationName: 'ReGa',
    applicationVersion: version,
    applicationIcon: Image.asset(
      'assets/icon/rega-128.png',
      width: 64,
      height: 64,
      excludeFromSemantics: true,
    ),
    applicationLegalese: regaLegalese,
    children: const [
      SizedBox(height: 24),
      Text('Play games on a cloud machine you own.'),
    ],
  );
}
