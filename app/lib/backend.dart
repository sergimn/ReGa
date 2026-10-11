import 'src/rust/api/rega.dart' as rust;
import 'src/rust/api/rega.dart'
    show
        MachineOffer,
        MachineSummary,
        OrphanSummary,
        ProviderInfo,
        RegionChoices;

export 'src/rust/api/rega.dart'
    show
        MachineOffer,
        MachineStatus,
        MachineSummary,
        OrphanKind,
        OrphanSummary,
        ProviderInfo,
        RegaError,
        RegionChoices,
        RegionOption;

/// Everything the UI needs from the core. Widgets depend on this interface
/// so tests can swap in a fake without loading the Rust library.
abstract class RegaBackend {
  /// Signs in to every provider; one without credentials shows demo data
  /// and says why. Calling it again reconnects.
  Future<List<ProviderInfo>> connectProviders();

  /// Measures latency to every region; `true` when results changed.
  Future<bool> probeLatency(String providerId);

  Future<RegionChoices> regionChoices(
    String providerId, {
    String? typeName,
    required bool spot,
  });

  Future<List<MachineOffer>> machineOffers(
    String providerId, {
    required String regionId,
    required bool spot,
  });

  Future<List<MachineSummary>> listMachines(String providerId);

  /// Creates a machine and sets it up. With `cpuFirst`, an on-demand GPU
  /// machine is set up on a cheap CPU type, then switched to the GPU.
  Future<MachineSummary> launchMachine(
    String providerId, {
    required String name,
    required String regionId,
    required String typeName,
    required bool spot,
    required bool wireguard,
    required bool cpuFirst,
  });

  Future<MachineSummary> startMachine(String providerId, String machineId);

  /// Saves the game library and stops the machine.
  Future<MachineSummary> stopMachine(String providerId, String machineId);

  /// Finishes the stop of a machine that stopped itself after sitting idle,
  /// dropping its game library disk if it saved its games first. Leaves a
  /// machine that someone has started again alone.
  Future<MachineSummary> finishStop(String providerId, String machineId);

  /// A running machine restarts as the new type and is set up again.
  Future<MachineSummary> resizeMachine(
    String providerId,
    String machineId,
    String typeName,
  );

  /// Saves the games of a running machine, then deletes it. Throws a
  /// [RegaError] with `gamesNotSaved` instead when that would lose games,
  /// unless `evenIfUnsaved`.
  Future<void> destroyMachine(
    String providerId,
    String machineId, {
    bool evenIfUnsaved = false,
  });

  Future<List<OrphanSummary>> listOrphans(String providerId);

  Future<void> deleteOrphans(String providerId, List<OrphanSummary> orphans);
}

/// Calls into the Rust core through flutter_rust_bridge.
class RustBackend implements RegaBackend {
  @override
  Future<List<ProviderInfo>> connectProviders() => rust.connectProviders();

  @override
  Future<bool> probeLatency(String providerId) =>
      rust.probeLatency(providerId: providerId);

  @override
  Future<RegionChoices> regionChoices(
    String providerId, {
    String? typeName,
    required bool spot,
  }) => rust.regionChoices(
    providerId: providerId,
    typeName: typeName,
    spot: spot,
  );

  @override
  Future<List<MachineOffer>> machineOffers(
    String providerId, {
    required String regionId,
    required bool spot,
  }) => rust.machineOffers(
    providerId: providerId,
    regionId: regionId,
    spot: spot,
  );

  @override
  Future<List<MachineSummary>> listMachines(String providerId) =>
      rust.listMachines(providerId: providerId);

  @override
  Future<MachineSummary> launchMachine(
    String providerId, {
    required String name,
    required String regionId,
    required String typeName,
    required bool spot,
    required bool wireguard,
    required bool cpuFirst,
  }) => rust.launchMachine(
    providerId: providerId,
    name: name,
    regionId: regionId,
    typeName: typeName,
    spot: spot,
    wireguard: wireguard,
    cpuFirst: cpuFirst,
  );

  @override
  Future<MachineSummary> startMachine(String providerId, String machineId) =>
      rust.startMachine(providerId: providerId, machineId: machineId);

  @override
  Future<MachineSummary> stopMachine(String providerId, String machineId) =>
      rust.stopMachine(providerId: providerId, machineId: machineId);

  @override
  Future<MachineSummary> finishStop(String providerId, String machineId) =>
      rust.finishStop(providerId: providerId, machineId: machineId);

  @override
  Future<MachineSummary> resizeMachine(
    String providerId,
    String machineId,
    String typeName,
  ) => rust.resizeMachine(
    providerId: providerId,
    machineId: machineId,
    typeName: typeName,
  );

  @override
  Future<void> destroyMachine(
    String providerId,
    String machineId, {
    bool evenIfUnsaved = false,
  }) => rust.destroyMachine(
    providerId: providerId,
    machineId: machineId,
    evenIfUnsaved: evenIfUnsaved,
  );

  @override
  Future<List<OrphanSummary>> listOrphans(String providerId) =>
      rust.listOrphans(providerId: providerId);

  @override
  Future<void> deleteOrphans(String providerId, List<OrphanSummary> orphans) =>
      rust.deleteOrphans(providerId: providerId, orphans: orphans);
}

extension MachineOfferX on MachineOffer {
  bool get isGpu => gpuModel != null;
}
