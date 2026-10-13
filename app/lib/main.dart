import 'package:flutter/material.dart';

import 'app.dart';
import 'backend.dart';
import 'licences.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  await RustLib.init();
  registerLicences();
  runApp(RegaApp(backend: RustBackend()));
}
