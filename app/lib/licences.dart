import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'package:flutter/foundation.dart';

/// ReGa's own licence, word for word the LICENSE at the top of the
/// repository (a test keeps them the same).
const regaLicence = '''MIT License

Copyright (c) 2026 Sergi Miralles

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
''';

/// What `flatpak/rust_notices.py write` names its files.
const rustCratesFile = 'rust-crates.txt';
const rustStdFile = 'rust-std.html';

/// The AWS logo's README, where it comes from and on what terms.
const _awsLogoFile = 'aws-logo.md';

/// The name the licence list gives a file in the licence folder, where its
/// path alone wouldn't say what it is about.
const _names = {'terraform/LICENSE.txt': 'Terraform', _awsLogoFile: 'AWS logo'};

/// The package the standard library's notices are listed under.
const rustStdPackage = 'Rust standard library';

/// ReGa's Flatpak app id.
const regaAppId = 'io.github.sergimn.rega';

/// Where the licence notices that ship with ReGa are, for the program at
/// [executable]. The Flatpak installs them in `/app/share/licenses/<app id>`;
/// another build finds the ones `flatpak/rust_notices.py write` put next to
/// its bundle, in build/linux/x64/release/licenses for a release build.
String licenceFolder(String executable, Map<String, String> environment) {
  // Only in ReGa's own sandbox: a build started from inside another Flatpak
  // (a terminal in an IDE's, say) has that app's id.
  if (environment['FLATPAK_ID'] == regaAppId) {
    return '/app/share/licenses/$regaAppId';
  }
  return '${File(executable).parent.parent.path}/licenses';
}

/// Adds ReGa's licence, and the notices in its licence folder, to the ones
/// Flutter keeps for its own code and the Dart packages, so the licence
/// page lists them all. They're read when the page opens.
void registerLicences() {
  final folder = licenceFolder(
    Platform.resolvedExecutable,
    Platform.environment,
  );
  LicenseRegistry.addLicense(() => regaLicencesInBackground(folder));
}

/// [regaLicences], read and taken apart on an isolate of their own, as
/// Flutter does with its own: the standard library's notices alone are over
/// a megabyte, and the licence page would stall while they're split up.
Stream<LicenseEntry> regaLicencesInBackground(String folder) =>
    Stream.fromFuture(Isolate.run(() => regaLicences(folder).toList()))
        .expand((entries) => entries);

/// ReGa's licence, then those of everything in [folder]: each Rust crate's
/// texts under its own name, the standard library's notices, and any other
/// file under its name or path. A folder without the Rust notices (a build
/// nobody wrote them for) gets a note saying so in their place.
Stream<LicenseEntry> regaLicences(String folder) async* {
  yield const LicenseEntryWithLineBreaks(['ReGa'], regaLicence);
  final files = <String, File>{};
  final dir = Directory(folder);
  try {
    await for (final entity in dir.list(recursive: true)) {
      if (entity is File) {
        files[entity.path.substring(dir.path.length + 1)] = entity;
      }
    }
  } on FileSystemException {
    // No folder: only the note below.
  }
  if (!files.containsKey(rustCratesFile)) {
    yield const LicenseEntryWithLineBreaks(
      ['Rust crates'],
      'This build of ReGa doesn\'t have the licence notices of the Rust '
      'crates and the Rust standard library it is built with. The '
      'Flatpak has them; for a build of your own, see "Rust crate '
      'licences" in app/flatpak/README.md.',
    );
  }
  // The files this knows first, then the rest by path. flatpak-builder
  // also copies each module's licence files into a folder of the module's
  // name (rega/LICENSE, terraform/LICENSE_1), so a file that is word for
  // word one listed already is left out.
  final known = [rustCratesFile, rustStdFile, ..._names.keys];
  final listed = {regaLicence};
  for (final name in [
    ...known.where(files.containsKey),
    ...files.keys.where((name) => !known.contains(name)).toList()..sort(),
  ]) {
    final String text;
    try {
      text = utf8.decode(
        await files[name]!.readAsBytes(),
        allowMalformed: true,
      );
    } on FileSystemException catch (e) {
      yield LicenseEntryWithLineBreaks([
        _names[name] ?? name,
      ], 'Couldn\'t read $folder/$name: ${e.message}');
      continue;
    }
    if (!listed.add(text)) continue;
    switch (name) {
      case rustCratesFile:
        yield* Stream.fromIterable(rustCrateLicences(text));
      case rustStdFile:
        for (final section in htmlSections(text)) {
          yield LicenseEntryWithLineBreaks([rustStdPackage], section);
        }
      case _awsLogoFile:
        yield LicenseEntryWithLineBreaks([_names[name]!], _markdownIntro(text));
      default:
        yield LicenseEntryWithLineBreaks([_names[name] ?? name], text);
    }
  }
}

/// A Markdown file's text up to its first `##` heading, without its title:
/// for the AWS logo's README, the notice, without how to draw the logo
/// again. The whole text if that leaves nothing.
String _markdownIntro(String text) {
  final lines = const LineSplitter().convert(text);
  final end = lines.indexWhere((line) => line.startsWith('## '));
  final intro = _withoutBlankEnds([
    for (final line in end < 0 ? lines : lines.sublist(0, end))
      if (!line.startsWith('# ')) line,
  ]);
  return intro.isEmpty ? text : intro;
}

/// A licence text that a crate ships, under the files it ships it in.
class _ShippedText extends LicenseEntry {
  const _ShippedText(this.crate, this.files, this.text);

  final String crate;

  /// `serde 1.0.228: LICENSE-MIT`, one for each version of the crate that
  /// ships the text.
  final List<String> files;

  /// The text, shared by every crate that ships it word for word.
  final LicenseEntryWithLineBreaks text;

  @override
  Iterable<String> get packages => [crate];

  @override
  Iterable<LicenseParagraph> get paragraphs => [
    for (final file in files) LicenseParagraph(file, 0),
    ...text.paragraphs,
  ];
}

final _rule = '=' * 78;
final _thinRule = '-' * 78;
// A crate in the list: `serde 1.0.228    MIT OR Apache-2.0`.
final _listed = RegExp(r'^(\S+) (\S+) {2,}(\S.*)$');
// A crate that ships a text: `serde 1.0.228: LICENSE-MIT`.
final _shipper = RegExp(r'^(\S+) (\S+): \S');

/// The entries for the crates in a `rust-crates.txt`: one per crate with
/// its version and licence, then the licence texts it ships, each under
/// the file it came from (vendored code's say whose they are). A file that
/// doesn't read the way rust_notices.py writes it is listed whole, under
/// "Rust crates", so no notice is ever lost.
List<LicenseEntry> rustCrateLicences(String text) {
  final lines = const LineSplitter().convert(text);
  final crates = lines.indexOf('Crates');
  final texts = lines.indexOf('Licence texts');
  final entries = <LicenseEntry>[];
  final listed = <String>{};
  if (crates >= 0 && texts > crates) {
    for (final line in lines.sublist(crates + 1, texts)) {
      final m = _listed.firstMatch(line);
      if (m == null) continue;
      listed.add(m[1]!);
      entries.add(
        LicenseEntryWithLineBreaks([
          m[1]!,
        ], '${m[1]} ${m[2]}, a Rust crate in ReGa\'s core, is under ${m[3]}.'),
      );
    }
  }

  // Each text: a blank line and a rule, the crates that ship it, a thin
  // rule and a blank line, then the text, up to the next one's blank line.
  final starts = <int>[];
  if (texts >= 0) {
    for (var i = texts + 1; i < lines.length; i++) {
      if (lines[i] != _rule || lines[i - 1].isNotEmpty) continue;
      var end = i + 1;
      while (end < lines.length && _shipper.hasMatch(lines[end])) {
        end++;
      }
      if (end > i + 1 && end < lines.length && lines[end] == _thinRule) {
        starts.add(i);
      }
    }
  }
  final shipping = <String>{};
  for (final (n, start) in starts.indexed) {
    final stop = n + 1 < starts.length ? starts[n + 1] - 1 : lines.length;
    var line = start + 1;
    // The lines naming each crate's files, by crate.
    final files = <String, List<String>>{};
    for (; lines[line] != _thinRule; line++) {
      final crate = _shipper.firstMatch(lines[line])![1]!;
      (files[crate] ??= []).add(lines[line]);
    }
    final body = _withoutBlankEnds(lines.sublist(line + 1, stop));
    if (body.isEmpty) continue;
    shipping.addAll(files.keys);
    final shared = LicenseEntryWithLineBreaks(const [], body);
    for (final MapEntry(key: crate, value: from) in files.entries) {
      entries.add(_ShippedText(crate, from, shared));
    }
  }

  if (listed.isEmpty || !shipping.containsAll(listed)) {
    return [
      LicenseEntryWithLineBreaks(const ['Rust crates'], text),
    ];
  }
  return entries;
}

/// [lines] joined, without the blank ones at either end. A first line's
/// indent stays: it centres a licence's title.
String _withoutBlankEnds(List<String> lines) {
  var start = 0;
  var end = lines.length;
  while (start < end && lines[start].trim().isEmpty) {
    start++;
  }
  while (end > start && lines[end - 1].trim().isEmpty) {
    end--;
  }
  return lines.sublist(start, end).map((l) => l.trimRight()).join('\n');
}

// Tags that end a paragraph; anything else (b, code, a) is part of one.
const _blocks = {
  'br',
  'dd',
  'details',
  'div',
  'dl',
  'dt',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'hr',
  'li',
  'ol',
  'p',
  'pre',
  'summary',
  'table',
  'td',
  'th',
  'tr',
  'ul',
};
final _tag = RegExp(
  r'<(/?)([a-zA-Z][a-zA-Z0-9]*)\b[^>]*>|<!--.*?-->|<!.*?>|<',
  dotAll: true,
);
final _entity = RegExp(r'&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);');
const _named = {
  'amp': '&',
  'lt': '<',
  'gt': '>',
  'quot': '"',
  'apos': "'",
  'nbsp': ' ',
};

String _decode(String text) => text.replaceAllMapped(_entity, (m) {
  final name = m[1]!;
  final code = name.startsWith('#x') || name.startsWith('#X')
      ? int.tryParse(name.substring(2), radix: 16)
      : name.startsWith('#')
      ? int.tryParse(name.substring(1))
      : null;
  if (code != null && code > 0 && code <= 0x10FFFF) {
    return String.fromCharCode(code);
  }
  return _named[name] ?? m[0]!;
});

/// The text of an HTML page of notices, like the standard library's, in
/// the plain paragraphs a licence entry shows: one section before the first
/// h2 or h3 heading, then one per heading. Whitespace runs collapse outside
/// `<pre>`, where line breaks stay; the head is left out.
List<String> htmlSections(String html) {
  final sections = <String>[];
  final section = StringBuffer();
  final paragraph = StringBuffer();
  var inHead = false;
  var pre = 0;

  void endParagraph() {
    final text = paragraph.toString();
    paragraph.clear();
    final out = pre > 0
        // A licence text, laid out with its own line breaks.
        ? _withoutBlankEnds(const LineSplitter().convert(text))
        : text.replaceAll(RegExp(r'\s+'), ' ').trim();
    if (out.isNotEmpty) section.write('$out\n\n');
  }

  void endSection() {
    endParagraph();
    final text = section.toString().trim();
    section.clear();
    if (text.isNotEmpty) sections.add(text);
  }

  var at = 0;
  for (final m in _tag.allMatches(html)) {
    if (!inHead) paragraph.write(_decode(html.substring(at, m.start)));
    at = m.end;
    final name = m[2]?.toLowerCase();
    if (name == null) {
      // A comment or doctype, left out; a lone < is text.
      if (m[0] == '<' && !inHead) paragraph.write('<');
      continue;
    }
    final closing = m[1] == '/';
    if (name == 'head') {
      inHead = !closing;
    } else if (!closing && (name == 'h2' || name == 'h3')) {
      endSection();
    } else if (name == 'pre') {
      endParagraph();
      pre += closing ? (pre > 0 ? -1 : 0) : 1;
    } else if (_blocks.contains(name) && pre == 0) {
      endParagraph();
    }
  }
  if (!inHead) paragraph.write(_decode(html.substring(at)));
  endSection();
  return sections;
}
