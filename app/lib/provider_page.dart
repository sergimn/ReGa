import 'dart:async';
import 'dart:ui' show AppExitResponse;

import 'package:flutter/material.dart';

import 'backend.dart';
import 'widgets/format.dart';
import 'widgets/machine_card.dart';
import 'widgets/machine_tile.dart';
import 'widgets/region_picker.dart';

/// One provider: purchase toggles, region picker, machine types, launch,
/// the machines that already exist and any leftovers to clean up.
class ProviderPage extends StatefulWidget {
  const ProviderPage({
    super.key,
    required this.backend,
    required this.provider,
    this.onReconnect,
  });

  final RegaBackend backend;
  final ProviderInfo provider;

  /// Signs in again, for example after adding credentials.
  final VoidCallback? onReconnect;

  @override
  State<ProviderPage> createState() => _ProviderPageState();
}

class _ProviderPageState extends State<ProviderPage> {
  bool _spot = false;
  bool _wireguard = true;
  String? _typeName;

  /// The region offers are priced for: the user's pick while it's offered,
  /// otherwise Recommended.
  String? _regionId;

  /// The region the user tapped, kept even while it isn't offered (say, no
  /// spot quota) so it comes back when it is. Null means follow Recommended.
  String? _userRegionId;
  bool _measuring = true;
  bool _probeDone = false;

  /// True from the start of a full reload until one lands, so a region tap
  /// in between doesn't keep an outdated region list.
  bool _regionsStale = true;

  RegionChoices? _choices;
  List<MachineOffer> _offers = const [];
  List<MachineSummary> _machines = const [];

  /// Why machines couldn't be listed (say, the state bucket can't be made);
  /// regions and prices still show.
  Object? _machinesError;
  Object? _error;
  bool _loading = true;

  final _name = TextEditingController(text: 'Gaming PC');
  bool _launching = false;

  /// Machines with an action in progress, and what it is.
  final Map<String, String> _busy = {};

  /// This app is changing a machine, or one is starting on another device.
  /// ReGa brings up one machine at a time, so Start, Switch and Launch wait.
  bool get _changing =>
      _launching || _busy.isNotEmpty || _machines.any((m) => m.starting);

  /// Rechecks machines that another device (or an earlier run of the app)
  /// is changing, since nothing else says when that's done.
  Timer? _poll;
  bool _rechecking = false;

  /// The last failed launch or machine action.
  Object? _actionError;
  List<OrphanSummary> _orphans = const [];
  bool _deletingOrphans = false;

  /// Bumped by every reload so a slower, older reload can't overwrite the
  /// results of a newer one.
  int _generation = 0;

  RegaBackend get _backend => widget.backend;
  String get _id => widget.provider.id;

  late final AppLifecycleListener _lifecycle;

  @override
  void initState() {
    super.initState();
    _lifecycle = AppLifecycleListener(onExitRequested: _confirmExit);
    _reload();
    _probeLatency();
    _loadOrphans();
  }

  @override
  void dispose() {
    _poll?.cancel();
    _lifecycle.dispose();
    _name.dispose();
    super.dispose();
  }

  /// Quitting in the middle of a change can stop it halfway, so ask first.
  Future<AppExitResponse> _confirmExit() async {
    if (!_launching && _busy.isEmpty && !_deletingOrphans) {
      return AppExitResponse.exit;
    }
    final quit = await _confirm(
      'Quit while a change is running?',
      'Quitting can stop it halfway. ReGa picks up from there the next time, '
          'and anything left behind shows under Leftover resources.',
      'Quit',
    );
    return quit ? AppExitResponse.exit : AppExitResponse.cancel;
  }

  void _pollWhileBusy() {
    if (!_machines.any((m) => m.busy)) {
      _poll?.cancel();
      _poll = null;
      return;
    }
    _poll ??= Timer.periodic(const Duration(seconds: 15), (_) => _recheck());
  }

  Future<void> _recheck() async {
    // This app's own changes reload when they finish.
    if (_rechecking || _launching || _busy.isNotEmpty) return;
    _rechecking = true;
    try {
      final generation = _generation;
      final machines = await _backend.listMachines(_id);
      if (!mounted || generation != _generation) return;
      setState(() {
        _machines = machines;
        _machinesError = null;
      });
      _pollWhileBusy();
      if (!machines.any((m) => m.busy)) {
        // Done: quotas and leftovers may have changed with it.
        await _reload();
        await _loadOrphans();
      }
    } catch (_) {
      // Try again on the next tick.
    } finally {
      _rechecking = false;
    }
  }

  /// Leftovers are looked for across every region, so only at start and
  /// after actions that can leave some behind, not on every reload.
  Future<void> _loadOrphans() async {
    try {
      final orphans = await _backend.listOrphans(_id);
      if (mounted) setState(() => _orphans = orphans);
    } catch (_) {
      // Leftovers are a hint; the machines list still works.
    }
  }

  Future<void> _launch() async {
    final regionId = _regionId;
    final typeName = _typeName;
    if (regionId == null || typeName == null) return;
    setState(() {
      _launching = true;
      _actionError = null;
    });
    try {
      await _backend.launchMachine(
        _id,
        name: _name.text,
        regionId: regionId,
        typeName: typeName,
        spot: _spot,
        wireguard: _wireguard,
      );
    } catch (e) {
      if (mounted) setState(() => _actionError = e);
    }
    if (!mounted) return;
    // A failed launch still shows up, so it can be cleaned up. The regions
    // reload too: the machine now uses some of their GPU quota.
    await _reload();
    if (!mounted) return;
    setState(() => _launching = false);
    await _loadOrphans();
  }

  /// Runs one action on a machine, showing `label` until it's done.
  Future<void> _act(
    MachineSummary m,
    String label,
    Future<void> Function() action,
  ) async {
    setState(() {
      _busy[m.id] = label;
      _actionError = null;
    });
    try {
      await action();
    } catch (e) {
      if (mounted) setState(() => _actionError = e);
    }
    if (!mounted) return;
    // Keep the machine busy until its new state is in, so its old buttons
    // don't come back in between. Quotas change with it, so regions too.
    await _reload();
    if (mounted) setState(() => _busy.remove(m.id));
  }

  Future<bool> _confirm(String title, String body, String action) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: Text(body),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const ValueKey('confirm'),
            onPressed: () => Navigator.pop(context, true),
            child: Text(action),
          ),
        ],
      ),
    );
    return ok ?? false;
  }

  Future<void> _destroy(MachineSummary m) async {
    final ok = await _confirm(
      'Destroy ${m.name}?',
      'This deletes the machine and its disks. Games already copied to '
          'the game library stay there.',
      'Destroy',
    );
    if (!ok || !mounted) return;
    await _act(m, 'Destroying…', () => _backend.destroyMachine(_id, m.id));
    await _loadOrphans();
  }

  Future<void> _deleteOrphans() async {
    final ok = await _confirm(
      'Delete ${_orphans.length} leftover resources?',
      'They are tagged for ReGa but belong to no machine, for example '
          'after a failed launch. Instances are terminated first.',
      'Delete',
    );
    if (!ok || !mounted) return;
    setState(() {
      _deletingOrphans = true;
      _actionError = null;
    });
    try {
      await _backend.deleteOrphans(_id, _orphans);
    } catch (e) {
      if (mounted) setState(() => _actionError = e);
    }
    if (!mounted) return;
    setState(() => _deletingOrphans = false);
    // A deleted instance frees its GPU quota.
    await _reload();
    await _loadOrphans();
  }

  Future<void> _probeLatency() async {
    var changed = false;
    try {
      changed = await _backend.probeLatency(_id);
    } catch (_) {
      // Latency is a hint; regions still show with "Latency unknown".
    }
    if (!mounted) return;
    _probeDone = true;
    // Keep "Measuring…" until a region list fetched after the probe lands;
    // the reload that lands clears it (this one, or a newer one).
    if (changed) {
      _reload();
    } else {
      setState(() => _measuring = false);
    }
  }

  /// Reloads regions, offers and machines. With `regions: false` (the user
  /// only picked another region) the region list is kept as is.
  Future<void> _reload({bool regions = true}) async {
    final generation = ++_generation;
    final spot = _spot;
    final typeName = _typeName;
    final wantedRegionId = _userRegionId;
    final currentChoices = _choices;
    final fetchRegions = regions || _regionsStale || currentChoices == null;
    bool stale() => !mounted || generation != _generation;

    setState(() {
      _loading = true;
      if (fetchRegions) _regionsStale = true;
    });
    try {
      final choices = fetchRegions
          ? await _backend.regionChoices(_id, typeName: typeName, spot: spot)
          : currentChoices;
      final all = choices.all.map((r) => r.regionId);
      final regionId = all.contains(wantedRegionId)
          ? wantedRegionId
          : choices.recommended?.regionId;
      final offers = regionId == null
          ? const <MachineOffer>[]
          : await _backend.machineOffers(_id, regionId: regionId, spot: spot);
      List<MachineSummary>? machines;
      Object? machinesError;
      try {
        machines = await _backend.listMachines(_id);
      } catch (e) {
        machinesError = e;
      }
      if (stale()) return;
      setState(() {
        _choices = choices;
        _regionsStale = false;
        if (_probeDone && fetchRegions) _measuring = false;
        _regionId = regionId;
        _offers = offers;
        if (machines != null) _machines = machines;
        _machinesError = machinesError;
        if (!offers.any((o) => o.typeName == _typeName)) {
          final gpu = offers.where((o) => o.isGpu);
          _typeName = gpu.isNotEmpty ? gpu.first.typeName : null;
        }
        _error = null;
        _loading = false;
      });
      _pollWhileBusy();
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
      key: const ValueKey('provider-page'),
      padding: const EdgeInsets.all(24),
      children: [
        Text(widget.provider.name, style: theme.textTheme.headlineMedium),
        if (widget.provider.demo) ...[
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
          if (widget.provider.notice case final notice?)
            ListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(notice),
              trailing: widget.onReconnect == null
                  ? null
                  : TextButton(
                      key: const ValueKey('reconnect'),
                      onPressed: widget.onReconnect,
                      child: const Text('Reconnect'),
                    ),
            ),
        ],
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
            title: Text('$_error'),
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
            onSelected: (r, recommendedTile) {
              setState(() {
                _regionId = r.regionId;
                // Tapping the Recommended tile means "follow Recommended";
                // picking a region by name pins it.
                _userRegionId = recommendedTile ? null : r.regionId;
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
          _launchRow(theme),
          _Section('Your machines'),
          if (_actionError != null)
            ListTile(
              key: const ValueKey('action-error'),
              leading: Icon(
                Icons.error_outline,
                color: theme.colorScheme.error,
              ),
              title: Text('$_actionError'),
              trailing: IconButton(
                tooltip: 'Dismiss',
                icon: const Icon(Icons.close),
                onPressed: () => setState(() => _actionError = null),
              ),
            ),
          if (_machinesError != null)
            ListTile(
              key: const ValueKey('machines-error'),
              leading: Icon(
                Icons.error_outline,
                color: theme.colorScheme.error,
              ),
              title: Text('$_machinesError'),
              trailing: TextButton(
                onPressed: _reload,
                child: const Text('Retry'),
              ),
            )
          else if (_machines.isEmpty)
            const ListTile(
              leading: Icon(Icons.computer),
              title: Text('No machines yet'),
            )
          else
            for (final m in _machines)
              MachineTile(
                key: ValueKey('machine-${m.id}'),
                machine: m,
                busy: _busy[m.id],
                onStart: _changing
                    ? null
                    : () => _act(
                        m,
                        'Starting…',
                        () => _backend.startMachine(_id, m.id),
                      ),
                onStop: () =>
                    _act(m, 'Stopping…', () => _backend.stopMachine(_id, m.id)),
                onSwitch: _changing
                    ? null
                    : () => _act(
                        m,
                        'Switching…',
                        () => _backend.resizeMachine(_id, m.id, m.switchType!),
                      ),
                onDestroy: () => _destroy(m),
              ),
          if (_orphans.isNotEmpty) ..._orphanSection(theme),
        ],
      ],
    );
  }

  Widget _launchRow(ThemeData theme) {
    final offer = _offers.where((o) => o.typeName == _typeName).firstOrNull;
    final region = _choices?.all
        .where((r) => r.regionId == _regionId)
        .firstOrNull;
    // Without the machine list, the one-machine check can't run either.
    final ready =
        offer != null && region != null && !_loading && _machinesError == null;
    return Padding(
      padding: const EdgeInsets.only(top: 24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              SizedBox(
                width: 260,
                child: TextField(
                  key: const ValueKey('machine-name'),
                  controller: _name,
                  decoration: const InputDecoration(labelText: 'Name'),
                ),
              ),
              const SizedBox(width: 12),
              FilledButton.icon(
                key: const ValueKey('launch'),
                onPressed: ready && !_changing ? _launch : null,
                icon: const Icon(Icons.rocket_launch_outlined),
                label: const Text('Launch'),
              ),
            ],
          ),
          const SizedBox(height: 8),
          if (_launching)
            const Text(
              'Launching. Windows takes several minutes to boot and set '
              'its password.',
            )
          else if (ready)
            Text(
              '${offer.typeName} in ${region.displayName}, '
              '${formatPrice(offer.pricePerHour)}${_spot ? ' as spot' : ''}',
              style: theme.textTheme.bodySmall,
            ),
        ],
      ),
    );
  }

  List<Widget> _orphanSection(ThemeData theme) => [
    _Section('Leftover resources'),
    Text(
      'Tagged for ReGa but part of no machine, for example after a failed '
      'launch. They may still cost money.',
      style: theme.textTheme.bodySmall,
    ),
    for (final o in _orphans)
      ListTile(
        leading: const Icon(Icons.delete_sweep_outlined),
        title: Text(o.name ?? o.id),
        subtitle: Text('${formatKind(o.kind)} ${o.id} · ${o.regionName}'),
      ),
    Align(
      alignment: Alignment.centerLeft,
      child: _deletingOrphans
          ? const Padding(
              padding: EdgeInsets.all(8),
              child: CircularProgressIndicator(),
            )
          : FilledButton.tonal(
              key: const ValueKey('delete-orphans'),
              onPressed: _deleteOrphans,
              child: const Text('Delete all'),
            ),
    ),
  ];
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
