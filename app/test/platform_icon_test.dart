import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/app.dart';
import 'package:rega/theme.dart';
import 'package:rega/widgets/platform_icon.dart';

import 'fake_backend.dart';

Widget _themed(Brightness brightness, Widget child) => MaterialApp(
  theme: regaTheme(brightness),
  home: Scaffold(body: Center(child: child)),
);

/// The colour of the square each AWS logo is drawn on.
const _awsSquares = {
  'assets/platforms/aws/aws.png': Color(0xFF242F3E),
  'assets/platforms/aws/aws-dark.png': Color(0xFFFFFFFF),
};

/// The colour of pixel [p] in raw RGBA bytes.
Color _pixel(ByteData rgba, int p) => Color.fromARGB(
  rgba.getUint8(p * 4 + 3),
  rgba.getUint8(p * 4),
  rgba.getUint8(p * 4 + 1),
  rgba.getUint8(p * 4 + 2),
);

/// How many of the pixels in raw RGBA bytes are far from the first one's
/// colour, the corner's.
double _shareOffCorner(ByteData rgba) {
  final corner = _pixel(rgba, 0);
  final pixels = rgba.lengthInBytes ~/ 4;
  var off = 0;
  for (var p = 0; p < pixels; p++) {
    final c = _pixel(rgba, p);
    final distance =
        (c.r - corner.r).abs() +
        (c.g - corner.g).abs() +
        (c.b - corner.b).abs();
    if (distance > 1.2) off++;
  }
  return off / pixels;
}

void main() {
  for (final (brightness, file) in [
    (Brightness.light, 'aws.png'),
    (Brightness.dark, 'aws-dark.png'),
  ]) {
    testWidgets('AWS shows its logo in the ${brightness.name} theme', (
      tester,
    ) async {
      await tester.pumpWidget(
        _themed(brightness, const PlatformIcon(providerId: 'aws')),
      );

      final image = tester.widget<Image>(
        find.byKey(const ValueKey('platform-logo-aws')),
      );
      expect(
        (image.image as AssetImage).assetName,
        'assets/platforms/aws/$file',
      );
      expect(
        tester.getSize(find.byType(Image)),
        const Size.square(PlatformIcon.logoSize),
      );
      expect(find.byType(Icon), findsNothing);
    });
  }

  testWidgets('a platform without a logo shows a cloud', (tester) async {
    await tester.pumpWidget(
      _themed(Brightness.light, const PlatformIcon(providerId: 'elsewhere')),
    );
    expect(find.byIcon(Icons.cloud_outlined), findsOneWidget);

    await tester.pumpWidget(
      _themed(
        Brightness.light,
        const PlatformIcon(providerId: 'elsewhere', selected: true),
      ),
    );
    expect(find.byIcon(Icons.cloud), findsOneWidget);
  });

  test('every logo is bundled at each density, drawn on its square', () async {
    TestWidgetsFlutterBinding.ensureInitialized();
    for (final logo in PlatformIcon.logos.values) {
      for (final path in [logo.light, logo.dark]) {
        final slash = path.lastIndexOf('/') + 1;
        for (final (folder, scale) in [('', 1), ('2.0x/', 2), ('3.0x/', 3)]) {
          final variant =
              '${path.substring(0, slash)}$folder${path.substring(slash)}';
          final bytes = await rootBundle.load(variant);
          final codec = await ui.instantiateImageCodec(
            bytes.buffer.asUint8List(),
          );
          final image = (await codec.getNextFrame()).image;
          final side = (PlatformIcon.logoSize * scale).round();
          expect((image.width, image.height), (side, side), reason: variant);
          final rgba = (await image.toByteData())!;
          final corners = [0, side - 1, side * (side - 1), side * side - 1];
          for (final p in corners) {
            expect(
              _pixel(rgba, p),
              _awsSquares[path] ?? _pixel(rgba, 0),
              reason: '$variant: square',
            );
          }
          // The logo takes about a tenth of the square; a blank or
          // half-drawn image has far less or far more.
          expect(
            _shareOffCorner(rgba),
            inInclusiveRange(0.05, 0.2),
            reason: '$variant: logo',
          );
          image.dispose();
        }
      }
    }
  });

  testWidgets('the side menu shows the AWS logo', (tester) async {
    final view = tester.view
      ..physicalSize = const Size(1400, 1800)
      ..devicePixelRatio = 1;
    addTearDown(view.reset);
    await tester.pumpWidget(RegaApp(backend: FakeBackend()));
    await tester.pumpAndSettle();

    final rail = find.byType(NavigationRail);
    expect(
      find.descendant(
        of: rail,
        matching: find.byKey(const ValueKey('platform-logo-aws')),
      ),
      findsOneWidget,
    );
    expect(
      find.descendant(of: rail, matching: find.byIcon(Icons.cloud)),
      findsNothing,
    );
    // AWS is always selected here, so check the unselected icon by hand.
    final destination = tester.widget<NavigationRail>(rail).destinations.single;
    for (final (icon, selected) in [
      (destination.icon, false),
      (destination.selectedIcon, true),
    ]) {
      expect(
        icon,
        isA<PlatformIcon>()
            .having((i) => i.providerId, 'providerId', 'aws')
            .having((i) => i.selected, 'selected', selected),
      );
    }
  });
}
