import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/theme.dart';

void main() {
  test('palette has no orange', () {
    for (final c in RegaColors.all) {
      final hsl = HSLColor.fromColor(c);
      final isOrange = hsl.saturation > 0.3 && hsl.hue > 15 && hsl.hue < 45;
      expect(isOrange, isFalse, reason: '$c reads as orange');
    }
  });

  test('both themes use the brand colours', () {
    final light = regaTheme(Brightness.light).colorScheme;
    final dark = regaTheme(Brightness.dark).colorScheme;
    expect(light.primary, RegaColors.deepTeal);
    expect(dark.primary, RegaColors.aqua);
    expect(dark.surface, RegaColors.night);
    expect(light.brightness, Brightness.light);
    expect(dark.brightness, Brightness.dark);
  });
}
