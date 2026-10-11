import 'package:flutter/material.dart';

import '../backend.dart';
import 'format.dart';

/// A machine type, led by what it offers. The provider's type name is small.
class MachineCard extends StatelessWidget {
  const MachineCard({
    super.key,
    required this.offer,
    required this.selected,
    required this.onTap,
  });

  final MachineOffer offer;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final gpu = offer.isGpu
        ? '${offer.gpuModel} · ${formatGb(offer.vramGib!)} VRAM'
        : 'No GPU';
    return Card(
      clipBehavior: Clip.antiAlias,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(
          color: selected
              ? theme.colorScheme.primary
              : theme.colorScheme.outlineVariant,
          width: selected ? 2 : 1,
        ),
      ),
      child: InkWell(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              _Spec(icon: Icons.memory, text: '${offer.vcpus} vCPU'),
              _Spec(icon: Icons.storage, text: '${formatGb(offer.ramGib)} RAM'),
              _Spec(
                icon: offer.isGpu ? Icons.videogame_asset : Icons.block,
                text: gpu,
              ),
              _Spec(icon: Icons.save, text: '${formatGb(offer.diskGib)} disk'),
              const SizedBox(height: 12),
              Text(
                formatPrice(offer.pricePerHour),
                style: theme.textTheme.titleLarge,
              ),
              Text(
                offer.typeName,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Spec extends StatelessWidget {
  const _Spec({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 18),
          const SizedBox(width: 8),
          Flexible(child: Text(text)),
        ],
      ),
    );
  }
}
