import 'package:flutter/material.dart';

import '../backend.dart';
import 'format.dart';

/// Shows Recommended, Lowest latency and Cheapest, and expands to every
/// launchable region on demand.
class RegionPicker extends StatefulWidget {
  const RegionPicker({
    super.key,
    required this.choices,
    required this.selectedRegionId,
    required this.onSelected,
    this.measuring = true,
  });

  final RegionChoices choices;
  final String? selectedRegionId;
  final ValueChanged<RegionOption> onSelected;

  /// True while latency probes are still running.
  final bool measuring;

  @override
  State<RegionPicker> createState() => _RegionPickerState();
}

class _RegionPickerState extends State<RegionPicker> {
  bool _showAll = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.choices;
    if (c.all.isEmpty) {
      return const ListTile(
        leading: Icon(Icons.info_outline),
        title: Text('No region offers this machine on this account yet.'),
      );
    }
    // One region can win several categories; show it once with every label.
    final labels = <String, List<String>>{};
    final options = <String, RegionOption>{};
    for (final (label, option) in [
      ('Recommended', c.recommended),
      ('Lowest latency', c.lowestLatency),
      ('Cheapest', c.cheapest),
    ]) {
      if (option == null) continue;
      labels.putIfAbsent(option.regionId, () => []).add(label);
      options[option.regionId] = option;
    }
    // Keep a region picked from the full list visible when it's collapsed.
    final picked = c.all
        .where((r) => r.regionId == widget.selectedRegionId)
        .firstOrNull;
    final tiles = <Widget>[
      for (final MapEntry(key: id, value: names) in labels.entries)
        _tile(options[id]!, label: names.join(' · ')),
      if (!_showAll && picked != null && !labels.containsKey(picked.regionId))
        _tile(picked, label: 'Your pick'),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          onPressed: () => setState(() => _showAll = !_showAll),
          icon: Icon(_showAll ? Icons.expand_less : Icons.expand_more),
          label: Text(_showAll ? 'Hide regions' : 'Show all regions'),
        ),
      ),
      if (_showAll)
        for (final option in c.all) _tile(option),
    ];
    return Column(children: tiles);
  }

  Widget _tile(RegionOption option, {String? label}) {
    final selected = option.regionId == widget.selectedRegionId;
    final copy = option.oneOffCost > 0
        ? ' · \$${option.oneOffCost.toStringAsFixed(2)} once to copy games'
        : '';
    return ListTile(
      key: ValueKey('region-${label ?? 'all'}-${option.regionId}'),
      selected: selected,
      leading: Icon(
        selected ? Icons.radio_button_checked : Icons.radio_button_unchecked,
      ),
      title: Text(label ?? option.displayName),
      subtitle: Text(
        '${label == null ? '' : '${option.displayName} · '}'
        '${formatRtt(option.rttMs, measuring: widget.measuring)} · '
        '${formatPrice(option.pricePerHour)}$copy',
      ),
      onTap: () => widget.onSelected(option),
    );
  }
}
