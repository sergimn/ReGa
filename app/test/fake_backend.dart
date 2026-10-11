import 'package:rega/backend.dart';

const gpuOffer = MachineOffer(
  typeName: 'g4dn.xlarge',
  vcpus: 4,
  ramGib: 16,
  gpuModel: 'NVIDIA T4',
  vramGib: 16,
  diskGib: 128,
  pricePerHour: 0.56,
);

const cpuOffer = MachineOffer(
  typeName: 't3.medium',
  vcpus: 2,
  ramGib: 4,
  diskGib: 128,
  pricePerHour: 0.045,
);

const stockholm = RegionOption(
  regionId: 'eu-north-1',
  displayName: 'Europe (Stockholm)',
  rttMs: 38,
  pricePerHour: 0.56,
  oneOffCost: 0,
);

const frankfurt = RegionOption(
  regionId: 'eu-central-1',
  displayName: 'Europe (Frankfurt)',
  rttMs: 31,
  pricePerHour: 0.66,
  oneOffCost: 4,
);

const virginia = RegionOption(
  regionId: 'us-east-1',
  displayName: 'US East (N. Virginia)',
  rttMs: 110,
  pricePerHour: 0.53,
  oneOffCost: 4,
);

const unmeasured = RegionOption(
  regionId: 'sa-east-1',
  displayName: 'South America (São Paulo)',
  pricePerHour: 0.9,
  oneOffCost: 4,
);

/// In-memory backend for widget tests; records what the UI asked for.
class FakeBackend implements RegaBackend {
  FakeBackend({this.machines = const [], this.onDemandDelay = Duration.zero});

  final List<MachineSummary> machines;

  /// Makes on-demand offers arrive late, to test overlapping reloads.
  final Duration onDemandDelay;
  final List<String> calls = [];
  bool? lastSpot;

  /// What the core currently calls Recommended; tests change it mid-way.
  RegionOption recommended = stockholm;

  /// What `probeLatency` reports; `true` makes the page reload.
  bool latencyChanges = false;

  /// When set, `regionChoices` throws it.
  Object? regionsError;

  /// With Spot on, Virginia isn't offered (say, no spot quota there).
  bool hideVirginiaWithSpot = false;

  /// How long the latency probe takes, and how long region lists take once
  /// it has measured something.
  Duration probeDelay = Duration.zero;
  Duration measuredRegionsDelay = Duration.zero;
  bool _measured = false;

  int get regionCalls => calls.where((c) => c.startsWith('regions')).length;

  @override
  List<ProviderInfo> listProviders() => const [
    ProviderInfo(id: 'aws', name: 'AWS', demo: true),
  ];

  @override
  Future<bool> probeLatency(String providerId) async {
    calls.add('probe $providerId');
    await Future<void>.delayed(probeDelay);
    _measured = latencyChanges;
    return latencyChanges;
  }

  @override
  Future<RegionChoices> regionChoices(
    String providerId, {
    String? typeName,
    required bool spot,
  }) async {
    calls.add('regions $typeName');
    if (regionsError case final e?) throw e;
    // Like the real core: CPU types have no spot price anywhere.
    if (spot && typeName == cpuOffer.typeName) {
      return const RegionChoices(all: []);
    }
    if (_measured) await Future<void>.delayed(measuredRegionsDelay);
    // Before the probe lands (when one is expected), nothing has a latency.
    final strip = latencyChanges && !_measured;
    RegionOption r(RegionOption o) => strip
        ? RegionOption(
            regionId: o.regionId,
            displayName: o.displayName,
            pricePerHour: o.pricePerHour,
            oneOffCost: o.oneOffCost,
          )
        : o;
    final hide = spot && hideVirginiaWithSpot;
    return RegionChoices(
      recommended: r(recommended),
      lowestLatency: r(frankfurt),
      cheapest: hide ? r(stockholm) : r(virginia),
      all: [r(frankfurt), r(stockholm), if (!hide) r(virginia), unmeasured],
    );
  }

  @override
  Future<List<MachineOffer>> machineOffers(
    String providerId, {
    required String regionId,
    required bool spot,
  }) async {
    calls.add('offers $regionId');
    lastSpot = spot;
    if (!spot) await Future<void>.delayed(onDemandDelay);
    return spot ? const [gpuOffer] : const [gpuOffer, cpuOffer];
  }

  @override
  Future<List<MachineSummary>> listMachines(String providerId) async =>
      machines;
}
