import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/licences.dart';

/// A rust-crates.txt with made-up crates, written by rust_notices.py's own
/// render(), so a change to its layout that the app can't read fails here.
String _renderCrates() {
  const script = r'''
import sys
from pathlib import Path
sys.path.insert(0, "flatpak")
from rust_notices import Crate, Entry, Text, render

MIT = "MIT License\n\nCopyright (c) 2024 Someone\n\nPermission is hereby granted...\n"
APACHE = "                                 Apache License\n                           Version 2.0, January 2004\n\nTERMS AND CONDITIONS\n"
BSD = "Copyright (c) 2017 Someone\n\nRedistribution and use in source and binary forms...\n"

def crate(name, version, license):
    return Crate(name, version, license, None, Path("/registry") / f"{name}-{version}")

entries = [
    Entry(crate("base64", "0.22.1", "MIT OR Apache-2.0"), "MIT OR Apache-2.0",
          (Text("LICENSE-APACHE", APACHE),)),
    Entry(crate("base64", "0.23.1", "MIT OR Apache-2.0"), "MIT OR Apache-2.0",
          (Text("LICENSE-APACHE", APACHE),)),
    Entry(crate("boringtun", "0.7.1", "BSD-3-Clause"), "BSD-3-Clause",
          (Text("app/flatpak/rust-licenses/boringtun/LICENSE, from its repository at 051c9d4", BSD),)),
    Entry(crate("serde", "1.0.228", "MIT OR Apache-2.0"), "MIT OR Apache-2.0",
          (Text("LICENSE-APACHE", APACHE), Text("LICENSE-MIT", MIT))),
]
sys.stdout.write(render("0" * 64, "rustc 1.98.1 (made up)", entries))
''';
  final result = Process.runSync('python3', ['-c', script]);
  if (result.exitCode != 0) {
    fail('rust_notices.py render failed: ${result.stderr}');
  }
  return result.stdout as String;
}

/// The standard library's notices in the Rust toolchain on PATH, if any.
File? _toolchainStdNotices() {
  try {
    final sysroot = Process.runSync('rustc', ['--print', 'sysroot']);
    final file = File(
      '${(sysroot.stdout as String).trim()}/share/doc/rust/COPYRIGHT-library.html',
    );
    return sysroot.exitCode == 0 && file.existsSync() ? file : null;
  } on ProcessException {
    return null;
  }
}

/// The packages and text of each entry.
List<(List<String>, String)> _read(Iterable<LicenseEntry> entries) => [
  for (final entry in entries)
    (entry.packages.toList(), entry.paragraphs.map((p) => p.text).join('\n')),
];

const _stdHtml = '''<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <title>Copyright notices for The Rust Standard Library</title>
</head>
<body>
<!-- a comment
     over lines -->
<h1>Copyright notices for The Rust Standard Library</h1>

<h2 id="short-version">Short version for non-lawyers</h2>

The Rust Standard Library is dual-licensed under Apache 2.0 and MIT terms.

<h2 id="out-of-tree-dependencies">Out-of-tree dependencies</h2>

    <h3>cc-1.2.0</h3>
    <p><b>Authors:</b> Alex Crichton &#60;alex@alexcrichton.com&#62; &amp; others</p>
    <details><summary>LICENSE-MIT</summary>
    <pre>
Copyright (c) 2014 Alex Crichton

Permission is hereby granted, &#34;free of charge&#34;,
to any person</pre></details>
</body>
</html>
''';

void main() {
  test('ReGa\'s licence is the repository\'s LICENSE', () {
    expect(regaLicence, File('../LICENSE').readAsStringSync());
  });

  test('the licence folder is the Flatpak\'s, or next to a build', () {
    expect(
      licenceFolder('/app/lib/rega/rega', {
        'FLATPAK_ID': 'io.github.sergimn.rega',
      }),
      '/app/share/licenses/io.github.sergimn.rega',
    );
    expect(
      licenceFolder('/src/app/build/linux/x64/release/bundle/rega', {}),
      '/src/app/build/linux/x64/release/licenses',
    );
  });

  test('each Rust crate gets its licence and the texts it ships', () {
    final entries = _read(rustCrateLicences(_renderCrates()));
    final apache = entries.where((e) => e.$2.contains('Apache License'));

    expect(entries.where((e) => e.$1.contains('serde')).map((e) => e.$2), [
      'serde 1.0.228, a Rust crate in ReGa\'s core, is under MIT OR Apache-2.0.',
      startsWith('Apache License'),
      startsWith('MIT License'),
    ]);
    // A text several crates ship is listed once, under each of them.
    expect(apache.single.$1, ['base64', 'serde']);
    expect(
      entries.where((e) => e.$1.contains('base64')).map((e) => e.$2),
      contains(
        'base64 0.23.1, a Rust crate in ReGa\'s core, is under '
        'MIT OR Apache-2.0.',
      ),
    );
    expect(
      entries.where((e) => e.$1.contains('boringtun')).last.$2,
      startsWith('Copyright (c) 2017 Someone\nRedistribution'),
    );
    // The centred title stays centred, and nothing of the file's own
    // header or rules leaks into a text.
    final title = rustCrateLicences(_renderCrates())
        .firstWhere(
          (e) => e.packages.contains('serde') && e.packages.length > 1,
        )
        .paragraphs
        .first;
    expect(title.indent, LicenseParagraph.centeredIndent);
    for (final (_, text) in entries) {
      expect(text, isNot(contains('=====')));
      expect(text, isNot(contains('-----')));
      expect(text, isNot(contains('rust_notices.py')));
    }
  });

  test('a crates file it can\'t read is listed whole', () {
    const odd = 'Some other layout\n\nserde 1.0.228: MIT\n';
    final (packages, text) = _read(rustCrateLicences(odd)).single;
    expect(packages, ['Rust crates']);
    expect(text, 'Some other layout\nserde 1.0.228: MIT');
    // A crate in the list without a text: shown whole, so it isn't lost.
    final rendered = _renderCrates();
    final cut = rendered.substring(0, rendered.indexOf('\n\n\nLicence texts'));
    final whole = rustCrateLicences('$cut\n\n\nLicence texts\n${'-' * 78}\n');
    expect(whole.single.packages, ['Rust crates']);
  });

  test('HTML notices become plain sections', () {
    final sections = htmlSections(_stdHtml);
    expect(sections, [
      'Copyright notices for The Rust Standard Library',
      'Short version for non-lawyers\n\n'
          'The Rust Standard Library is dual-licensed under Apache 2.0 and '
          'MIT terms.',
      'Out-of-tree dependencies',
      'cc-1.2.0\n\n'
          'Authors: Alex Crichton <alex@alexcrichton.com> & others\n\n'
          'LICENSE-MIT\n\n'
          'Copyright (c) 2014 Alex Crichton\n\n'
          'Permission is hereby granted, "free of charge",\n'
          'to any person',
    ]);
  });

  // From the Rust toolchain, when there is one with its docs.
  final stdNotices = _toolchainStdNotices();
  test('the toolchain\'s standard library notices read as text', () {
    final sections = htmlSections(stdNotices!.readAsStringSync());
    expect(sections.first, 'Copyright notices for The Rust Standard Library');
    expect(sections.length, greaterThan(5));
    for (final section in sections) {
      expect(section, isNot(matches(RegExp(r'</?[a-z][a-z0-9]*[ >]'))));
      expect(section, isNot(contains('&#')));
    }
  }, skip: stdNotices == null ? 'no Rust toolchain' : false);

  group('the licence list', () {
    late Directory folder;
    setUp(() => folder = Directory.systemTemp.createTempSync('licences'));
    tearDown(() => folder.deleteSync(recursive: true));

    void write(String name, String text) => (File(
      '${folder.path}/$name',
    )..createSync(recursive: true)).writeAsStringSync(text);

    test('has ReGa\'s, then all that ships in the folder', () async {
      write('LICENSE', regaLicence);
      write(rustCratesFile, _renderCrates());
      write(rustStdFile, _stdHtml);
      write('terraform/LICENSE.txt', 'Business Source License 1.1\n');
      write('aws-logo.md', '# AWS logo\n\nAWS is a trademark.\n');
      write('notes/other.txt', 'Something else\n');

      final entries = _read(await regaLicences(folder.path).toList());
      final packages = {for (final (p, _) in entries) ...p};

      expect(entries.first.$1, ['ReGa']);
      expect(entries.first.$2, startsWith('MIT License'));
      expect(entries.where((e) => e.$1.contains('ReGa')), hasLength(1));
      expect(packages, {
        'ReGa',
        'AWS logo',
        'base64',
        'boringtun',
        'notes/other.txt',
        rustStdPackage,
        'serde',
        'Terraform',
      });
      expect(entries.where((e) => e.$1.contains(rustStdPackage)), hasLength(4));
      expect(
        entries.firstWhere((e) => e.$1.contains('Terraform')).$2,
        'Business Source License 1.1',
      );
    });

    test('says when a build has no Rust notices', () async {
      folder.deleteSync();
      final entries = _read(await regaLicences(folder.path).toList());
      expect(entries.map((e) => e.$1), [
        ['ReGa'],
        ['Rust crates'],
      ]);
      expect(entries.last.$2, contains('app/flatpak/README.md'));
      folder.createSync();
    });
  });
}
