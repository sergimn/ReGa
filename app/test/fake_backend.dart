import 'dart:async';

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

const stockholmMachine = MachineSummary(
  id: 'gaming-1',
  name: 'Gaming',
  regionId: 'eu-north-1',
  regionName: 'Europe (Stockholm)',
  offer: gpuOffer,
  spot: false,
  status: MachineStatus.stopped,
  busy: false,
  starting: false,
  switchType: 't3.medium',
  switchPricePerHour: 0.045,
  stoppedItself: false,
  libraryKept: false,
);

const leftoverVolume = OrphanSummary(
  kind: OrphanKind.volume,
  id: 'vol-1',
  regionId: 'eu-north-1',
  regionName: 'Europe (Stockholm)',
  name: 'rega-old-library',
);

/// In-memory backend for widget tests; records what the UI asked for.
class FakeBackend implements RegaBackend {
  FakeBackend({
    List<MachineSummary> machines = const [],
    this.onDemandDelay = Duration.zero,
  }) : machines = [...machines];

  final List<MachineSummary> machines;
  List<OrphanSummary> orphans = [];

  ProviderInfo provider = const ProviderInfo(
    id: 'aws',
    name: 'AWS',
    demo: true,
    notice: 'No AWS credentials found.',
  );

  /// When set, machine actions and launches throw it.
  Object? actionError;

  /// `listMachines` throws this when set.
  Object? listMachinesError;

  /// Machines whose games couldn't be saved: destroying one asks first.
  final Set<String> unsaved = {};

  /// Machine actions wait for this before finishing, when set.
  Future<void>? actionGate;

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

  /// Region names reported as couldn't-check.
  List<String> unreachable = const [];

  /// With Spot on, Virginia isn't offered (say, no spot quota there).
  bool hideVirginiaWithSpot = false;

  /// How long the latency probe takes, and how long region lists take once
  /// it has measured something.
  Duration probeDelay = Duration.zero;
  Duration measuredRegionsDelay = Duration.zero;
  bool _measured = false;

  int get regionCalls => calls.where((c) => c.startsWith('regions')).length;

  /// When set, Play's updates come from it, so a test can step through.
  /// Otherwise Play goes through every step and ends done.
  StreamController<PlayUpdate>? playUpdates;

  /// Where Play ends when ReGa can't run Moonlight (manual pairing): the
  /// host to add in Moonlight. Null means Moonlight streams.
  String? moonlightHost;

  /// Whether Sunshine already has this device paired.
  bool paired = false;

  /// Play fails with this, when set.
  Object? playError;

  /// The PIN Sunshine takes.
  String acceptedPin = '1234';

  /// Machines Play connects to have WireGuard on.
  bool wireguard = true;

  /// What Play is connected to.
  StreamStatus? stream;

  void _dropStream(String machineId) {
    if (stream?.machineId == machineId) stream = null;
  }

  @override
  Future<List<ProviderInfo>> connectProviders() async {
    calls.add('connect');
    return [provider];
  }

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
      return const RegionChoices(all: [], unreachable: []);
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
      unreachable: unreachable,
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
  Future<List<MachineSummary>> listMachines(String providerId) async {
    if (listMachinesError case final e?) throw e;
    return List.of(machines);
  }

  Future<MachineSummary> _change(
    String what,
    String machineId,
    MachineSummary Function(MachineSummary) f,
  ) async {
    calls.add('$what $machineId');
    // Like the core: a change that takes the machine down closes Play's
    // connection to it first.
    if (what != 'start' && what != 'finish') _dropStream(machineId);
    await actionGate;
    if (actionError case final e?) throw e;
    final i = machines.indexWhere((m) => m.id == machineId);
    machines[i] = f(machines[i]);
    return machines[i];
  }

  @override
  Future<MachineSummary> launchMachine(
    String providerId, {
    required String name,
    required String regionId,
    required String typeName,
    required bool spot,
    required bool wireguard,
    required bool cpuFirst,
  }) async {
    calls.add(
      'launch $name $regionId $typeName spot=$spot wg=$wireguard '
      'cpu=$cpuFirst',
    );
    await actionGate;
    if (actionError case final e?) throw e;
    final m = MachineSummary(
      id: 'new-${machines.length}',
      name: name,
      regionId: regionId,
      regionName: regionId,
      offer: typeName == gpuOffer.typeName ? gpuOffer : cpuOffer,
      spot: spot,
      status: MachineStatus.running,
      busy: false,
      starting: false,
      switchType: spot ? null : 't3.medium',
      switchPricePerHour: spot ? null : 0.045,
      stoppedItself: false,
      libraryKept: false,
    );
    machines.add(m);
    return m;
  }

  @override
  Future<MachineSummary> startMachine(String providerId, String machineId) =>
      _change('start', machineId, (m) => withStatus(m, MachineStatus.running));

  @override
  Future<MachineSummary> stopMachine(String providerId, String machineId) =>
      _change('stop', machineId, (m) => withStatus(m, MachineStatus.stopped));

  @override
  Future<MachineSummary> finishStop(String providerId, String machineId) =>
      _change('finish', machineId, (m) => withStatus(m, m.status));

  @override
  Future<MachineSummary> resizeMachine(
    String providerId,
    String machineId,
    String typeName,
  ) => _change(
    'resize $typeName',
    machineId,
    (m) => withStatus(
      m,
      m.status,
      offer: typeName == cpuOffer.typeName ? cpuOffer : gpuOffer,
    ),
  );

  @override
  Future<void> destroyMachine(
    String providerId,
    String machineId, {
    bool evenIfUnsaved = false,
  }) async {
    calls.add('destroy $machineId${evenIfUnsaved ? ' anyway' : ''}');
    _dropStream(machineId);
    await actionGate;
    if (actionError case final e?) throw e;
    if (!evenIfUnsaved && unsaved.contains(machineId)) {
      throw const RegaError(
        message: "Gaming's games couldn't be saved to the game library.",
        gamesNotSaved: true,
      );
    }
    machines.removeWhere((m) => m.id == machineId);
  }

  @override
  Future<List<OrphanSummary>> listOrphans(String providerId) async {
    calls.add('orphans');
    return List.of(orphans);
  }

  @override
  Future<void> deleteOrphans(
    String providerId,
    List<OrphanSummary> orphans,
  ) async {
    calls.add('delete ${orphans.map((o) => o.id).join(',')}');
    this.orphans = [];
  }

  @override
  Stream<PlayUpdate> play(String providerId, String machineId) {
    calls.add('play $machineId');
    void connect() => stream = StreamStatus(
      providerId: providerId,
      machineId: machineId,
      host: moonlightHost ?? '127.0.0.1',
      wireguard: wireguard,
    );
    if (playUpdates case final updates?) {
      return updates.stream.map((u) {
        if (u.step == PlayStep.done) connect();
        return u;
      });
    }
    return _play(connect);
  }

  Stream<PlayUpdate> _play(void Function() connect) async* {
    yield const PlayUpdate(step: PlayStep.connecting, paired: false);
    if (playError case final e?) throw e;
    connect();
    if (moonlightHost == null) {
      yield const PlayUpdate(step: PlayStep.pairing, paired: false);
      yield const PlayUpdate(step: PlayStep.startingMoonlight, paired: false);
    }
    yield PlayUpdate(
      step: PlayStep.done,
      moonlightHost: moonlightHost,
      paired: paired,
    );
  }

  @override
  Future<void> pairWithPin(
    String providerId,
    String machineId,
    String pin,
  ) async {
    calls.add('pin $machineId $pin');
    if (pin != acceptedPin) {
      throw const RegaError(
        message: "Sunshine didn't accept that PIN.",
        gamesNotSaved: false,
      );
    }
    paired = true;
  }

  @override
  StreamStatus? streamStatus() => stream;

  @override
  Future<void> closeTunnel() async {
    calls.add('close tunnel');
    stream = null;
  }
}

/// `m` in another state: after any change by this app it no longer counts
/// as having stopped itself.
MachineSummary withStatus(
  MachineSummary m,
  MachineStatus status, {
  MachineOffer? offer,
  bool stoppedItself = false,
  bool libraryKept = false,
}) => MachineSummary(
  id: m.id,
  name: m.name,
  regionId: m.regionId,
  regionName: m.regionName,
  offer: offer ?? m.offer,
  spot: m.spot,
  status: status,
  busy: m.busy,
  starting: m.starting,
  switchType: m.switchType,
  switchPricePerHour: m.switchPricePerHour,
  stoppedItself: stoppedItself,
  libraryKept: libraryKept,
);
