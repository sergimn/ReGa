import 'package:flutter/material.dart';

/// A cloud platform's icon in the side menu: its logo, or a cloud for a
/// platform without one.
class PlatformIcon extends StatelessWidget {
  const PlatformIcon({
    super.key,
    required this.providerId,
    this.selected = false,
  });

  final String providerId;

  /// Fills the cloud of a platform without a logo; a logo stays as it is,
  /// with the menu's indicator behind it.
  final bool selected;

  /// Logos by provider id, for the light and dark themes. Each one's source
  /// and terms are in the README next to it.
  static const logos = {
    'aws': (
      light: 'assets/platforms/aws/aws.png',
      dark: 'assets/platforms/aws/aws-dark.png',
    ),
  };

  /// The size of a logo, which comes with its own background.
  static const logoSize = 32.0;

  @override
  Widget build(BuildContext context) {
    final logo = logos[providerId];
    if (logo == null) {
      return Icon(selected ? Icons.cloud : Icons.cloud_outlined);
    }
    final dark = Theme.of(context).brightness == Brightness.dark;
    return Image.asset(
      dark ? logo.dark : logo.light,
      key: ValueKey('platform-logo-$providerId'),
      width: logoSize,
      height: logoSize,
      filterQuality: FilterQuality.medium,
      // The menu's label already names the platform.
      excludeFromSemantics: true,
    );
  }
}
