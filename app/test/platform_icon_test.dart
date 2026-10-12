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

/// Width and height from a PNG's header.
(int, int) _pngSize(ByteData png) {
  expect(png.getUint32(0), 0x89504E47, reason: 'not a PNG');
  return (png.getUint32(16), png.getUint32(20));
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

  test('every logo is bundled at each density', () async {
    TestWidgetsFlutterBinding.ensureInitialized();
    for (final logo in PlatformIcon.logos.values) {
      for (final path in [logo.light, logo.dark]) {
        final slash = path.lastIndexOf('/') + 1;
        for (final (folder, scale) in [('', 1), ('2.0x/', 2), ('3.0x/', 3)]) {
          final variant =
              '${path.substring(0, slash)}$folder${path.substring(slash)}';
          final size = _pngSize(await rootBundle.load(variant));
          final side = (PlatformIcon.logoSize * scale).round();
          expect(size, (side, side), reason: variant);
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
  });
}
