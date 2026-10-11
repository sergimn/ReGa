import 'package:flutter/material.dart';

/// ReGa colour palette. Cool tones only: teal, indigo and violet on navy.
abstract final class RegaColors {
  // Brand
  static const aqua = Color(0xFF5EEAD4);
  static const teal = Color(0xFF14B8A6);
  static const deepTeal = Color(0xFF0F766E);
  static const indigo = Color(0xFF818CF8);
  static const deepIndigo = Color(0xFF4F46E5);
  static const violet = Color(0xFFA78BFA);
  static const deepViolet = Color(0xFF7C3AED);

  // Neutrals
  static const night = Color(0xFF0B1220);
  static const navy = Color(0xFF1C2A4A);
  static const slate = Color(0xFF334155);
  static const cloud = Color(0xFFE2E8F0);
  static const mist = Color(0xFFF4F7FB);

  // Status
  static const success = Color(0xFF22C55E);
  // Each error red keeps at least 4.5:1 contrast on its theme's surface.
  static const error = Color(0xFFCE2C31);
  static const errorOnDark = Color(0xFFF87171);

  static const all = [
    aqua,
    teal,
    deepTeal,
    indigo,
    deepIndigo,
    violet,
    deepViolet,
    night,
    navy,
    slate,
    cloud,
    mist,
    success,
    error,
    errorOnDark,
  ];
}

ThemeData regaTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  final scheme =
      ColorScheme.fromSeed(
        seedColor: RegaColors.teal,
        brightness: brightness,
      ).copyWith(
        primary: dark ? RegaColors.aqua : RegaColors.deepTeal,
        onPrimary: dark ? RegaColors.night : Colors.white,
        secondary: dark ? RegaColors.indigo : RegaColors.deepIndigo,
        onSecondary: dark ? RegaColors.night : Colors.white,
        tertiary: dark ? RegaColors.violet : RegaColors.deepViolet,
        onTertiary: dark ? RegaColors.night : Colors.white,
        error: dark ? RegaColors.errorOnDark : RegaColors.error,
        onError: dark ? RegaColors.night : Colors.white,
        surface: dark ? RegaColors.night : RegaColors.mist,
        onSurface: dark ? RegaColors.cloud : RegaColors.night,
        surfaceContainerLow: dark ? RegaColors.navy : Colors.white,
        outlineVariant: dark ? RegaColors.slate : RegaColors.cloud,
      );
  return ThemeData(colorScheme: scheme);
}
