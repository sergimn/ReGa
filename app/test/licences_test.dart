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

/// A python-packages.txt with made-up packages, written by
/// python_notices.py's own render(), as for [_renderCrates].
String _renderPython() {
  const script = r'''
import sys
sys.path.insert(0, "flatpak")
from python_notices import Package, render
from rust_notices import Text

MIT = "MIT License\n\nCopyright (c) 2024 Someone\n\nPermission is hereby granted...\n"
GPL = "                    GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007\n"

packages = [
    Package("ansible-core", "2.21.4", "GPL-3.0-or-later",
            (Text("licenses/COPYING", GPL), Text("licenses/licenses/MIT-license.txt", MIT))),
    Package("PyYAML", "6.0.3", "MIT", (Text("licenses/LICENSE", MIT),)),
    Package("pywinrm", "0.5.0", "MIT License", (Text("LICENSE", MIT),)),
    Package("ansible.windows", "3.8.0", "GPL-3.0-or-later", (Text("COPYING", GPL),), collection=True),
]
sys.stdout.write(render("0" * 64, packages))
''';
  final result = Process.runSync('python3', ['-c', script]);
  if (result.exitCode != 0) {
    fail('python_notices.py render failed: ${result.stderr}');
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
    // A build run from inside another app's sandbox.
    expect(
      licenceFolder('/src/app/build/linux/x64/release/bundle/rega', {
        'FLATPAK_ID': 'org.example.Editor',
      }),
      '/src/app/build/linux/x64/release/licenses',
    );
  });

  test('each Rust crate gets its licence and the texts it ships', () {
    final entries = _read(rustCrateLicences(_renderCrates()));

    expect(entries.where((e) => e.$1.contains('serde')).map((e) => e.$2), [
      'serde 1.0.228, a Rust crate in ReGa\'s core, is under MIT OR Apache-2.0.',
      startsWith('serde 1.0.228: LICENSE-APACHE\nApache License'),
      startsWith('serde 1.0.228: LICENSE-MIT\nMIT License'),
    ]);
    // Each version of a crate that ships a text, once over the text.
    expect(entries.where((e) => e.$1.contains('base64')).map((e) => e.$2), [
      'base64 0.22.1, a Rust crate in ReGa\'s core, is under '
          'MIT OR Apache-2.0.',
      'base64 0.23.1, a Rust crate in ReGa\'s core, is under '
          'MIT OR Apache-2.0.',
      startsWith(
        'base64 0.22.1: LICENSE-APACHE\nbase64 0.23.1: LICENSE-APACHE\n'
        'Apache License',
      ),
    ]);
    expect(entries.every((e) => e.$1.length == 1), isTrue);
    expect(
      entries.where((e) => e.$1.contains('boringtun')).last.$2,
      'boringtun 0.7.1: app/flatpak/rust-licenses/boringtun/LICENSE, from its '
      'repository at 051c9d4\n'
      'Copyright (c) 2017 Someone\n'
      'Redistribution and use in source and binary forms...',
    );
    // The file's line stands alone over the text, whose centred title
    // stays centred; nothing of the file's own header or rules leaks in.
    final apache = rustCrateLicences(_renderCrates())
        .where((e) => e.packages.contains('serde'))
        .elementAt(1)
        .paragraphs;
    expect(apache.first.text, 'serde 1.0.228: LICENSE-APACHE');
    expect(apache.first.indent, 0);
    expect(apache.elementAt(1).indent, LicenseParagraph.centeredIndent);
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

  test('each Python package and collection gets its licence and texts', () {
    final entries = _read(pythonPackageLicences(_renderPython()));

    expect(entries.where((e) => e.$1.contains('PyYAML')).map((e) => e.$2), [
      'PyYAML 6.0.3, a Python package that ReGa runs Ansible with, is under '
          'MIT.',
      startsWith('PyYAML 6.0.3: licenses/LICENSE\nMIT License'),
    ]);
    expect(
      entries.where((e) => e.$1.contains('ansible.windows')).map((e) => e.$2),
      [
        'ansible.windows 3.8.0, an Ansible collection that ReGa\'s playbooks '
            'use, is under GPL-3.0-or-later.',
        startsWith(
          'ansible.windows 3.8.0: COPYING\nGNU GENERAL PUBLIC LICENSE',
        ),
      ],
    );
    expect(
      entries.firstWhere((e) => e.$1.contains('pywinrm')).$2,
      'pywinrm 0.5.0, a Python package that ReGa runs Ansible with, is under '
      'MIT License.',
    );
    expect(
      {for (final (p, _) in entries) ...p},
      {'ansible-core', 'ansible.windows', 'PyYAML', 'pywinrm'},
    );
    for (final (_, text) in entries) {
      expect(text, isNot(contains('=====')));
      expect(text, isNot(contains('python_notices.py')));
    }
    // A file it can't read, listed whole.
    final rendered = _renderPython();
    final cut = rendered.substring(0, rendered.indexOf('\n\n\nLicence texts'));
    final whole = pythonPackageLicences('$cut\n\n\nLicence texts\n');
    expect(whole.single.packages, ['Python packages']);
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
      // Tags the page uses, not text like "<year>" in a licence template.
      expect(
        section,
        isNot(
          matches(
            RegExp(r'</?(a|b|code|details|div|h[1-6]|li|p|pre|summary|ul)[ >]'),
          ),
        ),
      );
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
      write(pythonPackagesFile, _renderPython());
      write('terraform/LICENSE.txt', 'Business Source License 1.1\n');
      write(
        'aws-logo.md',
        '# AWS logo\n\nAWS is a trademark.\n\n## Source\n\nsha256 0123\n',
      );
      write('notes/other.txt', 'Something else\n');
      // The copies flatpak-builder makes of each module's licence files.
      write('rega/LICENSE', regaLicence);
      write('terraform/LICENSE_1', 'Business Source License 1.1\n');
      write('rega/licenses_rust-crates.txt', _renderCrates());

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
        'ansible-core',
        'ansible.windows',
        'PyYAML',
        'pywinrm',
      });
      expect(entries.where((e) => e.$1.contains(rustStdPackage)), hasLength(4));
      expect(
        entries.singleWhere((e) => e.$1.contains('Terraform')).$2,
        'Business Source License 1.1',
      );
      expect(
        entries.singleWhere((e) => e.$1.contains('AWS logo')).$2,
        'AWS is a trademark.',
      );
      expect(entries.where((e) => e.$1.contains('serde')), hasLength(3));

      // Off the UI's isolate, the same.
      final background = _read(
        await regaLicencesInBackground(folder.path).toList(),
      );
      expect(background, hasLength(entries.length));
      for (final (i, (packages, text)) in background.indexed) {
        expect(packages, entries[i].$1);
        expect(text, entries[i].$2);
      }
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
