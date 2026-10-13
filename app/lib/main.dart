import 'package:flutter/material.dart';

import 'app.dart';
import 'backend.dart';
import 'licences.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  // Before anything starts Flutter's binding, which adds Flutter's own: the
  // licence page keeps the first package it's given at the top.
  registerLicences();
  await RustLib.init();
  runApp(RegaApp(backend: RustBackend()));
}
