import 'package:flutter/material.dart';

import 'backend.dart';
import 'widgets/format.dart';
import 'widgets/machine_card.dart';
import 'widgets/region_picker.dart';

/// One provider: purchase toggles, region picker, machine types and the
/// machines that already exist.
class ProviderPage extends StatefulWidget {
  const ProviderPage({
    super.key,
    required this.backend,
    required this.provider,
  });

  final RegaBackend backend;
  final ProviderInfo provider;

  @override
  State<ProviderPage> createState() => _ProviderPageState();
}

class _ProviderPageState extends State<ProviderPage> {
  bool _spot = false;
  bool _wireguard = true;
  String? _typeName;
  String? _regionId;

  /// False while the region is our pick (Recommended), so it follows
  /// Recommended as prices and latencies change; true once the user taps one.
  bool _userPickedRegion = false;
  bool _measuring = true;

  RegionChoices? _choices;
  List<MachineOffer> _offers = const [];
  List<MachineSummary> _machines = const [];
  Object? _error;
  bool _loading = true;

  /// Bumped by every reload so a slower, older reload can't overwrite the
  /// results of a newer one.
  int _generation = 0;

  RegaBackend get _backend => widget.backend;
  String get _id => widget.provider.id;

  @override
  void initState() {
    super.initState();
    _reload();
    _probeLatency();
  }

  Future<void> _probeLatency() async {
    var changed = false;
    try {
      changed = await _backend.probeLatency(_id);
    } catch (_) {
      // Latency is a hint; regions still show with "Latency unknown".
    }
    if (!mounted) return;
    setState(() => _measuring = false);
    if (changed) _reload();
  }

  String _errorText(Object e) => e is RegaError ? e.message : '$e';

  /// Reloads regions, offers and machines. With `regions: false` (the user
  /// only picked another region) the region list is kept as is.
  Future<void> _reload({bool regions = true}) async {
    final generation = ++_generation;
    final spot = _spot;
    final typeName = _typeName;
    final wantedRegionId = _userPickedRegion ? _regionId : null;
    final currentChoices = _choices;
    bool stale() => !mounted || generation != _generation;

    setState(() => _loading = true);
    try {
      final choices = regions || currentChoices == null
          ? await _backend.regionChoices(_id, typeName: typeName, spot: spot)
          : currentChoices;
      final all = choices.all.map((r) => r.regionId);
      final regionId = all.contains(wantedRegionId)
          ? wantedRegionId
          : choices.recommended?.regionId;
      final offers = regionId == null
          ? const <MachineOffer>[]
          : await _backend.machineOffers(_id, regionId: regionId, spot: spot);
      final machines = await _backend.listMachines(_id);
      if (stale()) return;
      setState(() {
        _choices = choices;
        _regionId = regionId;
        _offers = offers;
        _machines = machines;
        if (!offers.any((o) => o.typeName == _typeName)) {
          final gpu = offers.where((o) => o.isGpu);
          _typeName = gpu.isNotEmpty ? gpu.first.typeName : null;
        }
        _error = null;
        _loading = false;
      });
    } catch (e) {
      if (stale()) return;
      setState(() {
        _error = e;
        _loading = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text(widget.provider.name, style: theme.textTheme.headlineMedium),
        if (widget.provider.demo)
          const Padding(
            padding: EdgeInsets.only(top: 8),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Chip(
                avatar: Icon(Icons.science_outlined, size: 18),
                label: Text('Demo data: prices and latencies are samples'),
              ),
            ),
          ),
        const SizedBox(height: 16),
        SwitchListTile(
          key: const ValueKey('spot-toggle'),
          title: const Text('Spot'),
          subtitle: const Text(
            'Cheaper, but the provider can reclaim the machine. GPU types only.',
          ),
          value: _spot,
          onChanged: (v) {
            setState(() {
              _spot = v;
              // Spot only sells GPU types; drop a CPU pick so the region
              // picker prices the GPU type instead of finding nothing.
              final keep = _offers.any(
                (o) => o.typeName == _typeName && o.isGpu,
              );
              if (v && !keep) _typeName = null;
            });
            _reload();
          },
        ),
        SwitchListTile(
          key: const ValueKey('wireguard-toggle'),
          title: const Text('WireGuard tunnel'),
          subtitle: Text(
            _wireguard
                ? 'Streams through an encrypted tunnel and survives IP changes.'
                : 'Opens the streaming ports to your current IP only.',
          ),
          value: _wireguard,
          onChanged: (v) => setState(() => _wireguard = v),
        ),
        if (_error != null)
          ListTile(
            leading: Icon(Icons.error_outline, color: theme.colorScheme.error),
            title: Text(_errorText(_error!)),
            trailing: TextButton(
              onPressed: _reload,
              child: const Text('Retry'),
            ),
          ),
        if (_loading && _choices == null)
          const Padding(
            padding: EdgeInsets.all(32),
            child: Center(child: CircularProgressIndicator()),
          ),
        if (_choices != null) ...[
          _Section('Region'),
          RegionPicker(
            choices: _choices!,
            selectedRegionId: _regionId,
            measuring: _measuring,
            onSelected: (r) {
              setState(() {
                _regionId = r.regionId;
                _userPickedRegion = true;
              });
              _reload(regions: false);
            },
          ),
          _Section('Machine'),
          Wrap(
            spacing: 12,
            runSpacing: 12,
            children: [
              for (final offer in _offers)
                SizedBox(
                  width: 260,
                  child: MachineCard(
                    key: ValueKey('offer-${offer.typeName}'),
                    offer: offer,
                    selected: offer.typeName == _typeName,
                    onTap: () {
                      setState(() => _typeName = offer.typeName);
                      _reload();
                    },
                  ),
                ),
            ],
          ),
          _Section('Your machines'),
          if (_machines.isEmpty)
            const ListTile(
              leading: Icon(Icons.computer),
              title: Text('No machines yet'),
            )
          else
            for (final m in _machines)
              ListTile(
                leading: const Icon(Icons.computer),
                title: Text(m.name),
                subtitle: Text(
                  '${m.offer.vcpus} vCPU · ${formatGb(m.offer.ramGib)} RAM · '
                  '${m.offer.gpuModel ?? 'No GPU'} · ${m.regionId}',
                ),
                trailing: Text(m.status.name),
              ),
        ],
      ],
    );
  }
}

class _Section extends StatelessWidget {
  const _Section(this.title);

  final String title;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: 24, bottom: 8),
      child: Text(title, style: Theme.of(context).textTheme.titleMedium),
    );
  }
}
