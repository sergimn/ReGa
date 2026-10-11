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

/// In-memory backend for widget tests; records what the UI asked for.
class FakeBackend implements RegaBackend {
  FakeBackend({this.machines = const []});

  final List<MachineSummary> machines;
  final List<String> calls = [];
  bool? lastSpot;

  @override
  List<ProviderInfo> listProviders() => const [
    ProviderInfo(id: 'aws', name: 'AWS', demo: true),
  ];

  @override
  Future<void> probeLatency(String providerId) async =>
      calls.add('probe $providerId');

  @override
  Future<RegionChoices> regionChoices(
    String providerId, {
    String? typeName,
    required bool spot,
  }) async {
    calls.add('regions $typeName');
    return const RegionChoices(
      recommended: stockholm,
      lowestLatency: frankfurt,
      cheapest: virginia,
      all: [frankfurt, stockholm, virginia],
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
    return spot ? const [gpuOffer] : const [gpuOffer, cpuOffer];
  }

  @override
  Future<List<MachineSummary>> listMachines(String providerId) async =>
      machines;
}
