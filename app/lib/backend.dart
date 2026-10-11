import 'src/rust/api/rega.dart' as rust;
import 'src/rust/api/rega.dart'
    show MachineOffer, MachineSummary, ProviderInfo, RegionChoices;

export 'src/rust/api/rega.dart'
    show
        MachineOffer,
        MachineStatus,
        MachineSummary,
        ProviderInfo,
        RegaError,
        RegionChoices,
        RegionOption;

/// Everything the UI needs from the core. Widgets depend on this interface
/// so tests can swap in a fake without loading the Rust library.
abstract class RegaBackend {
  List<ProviderInfo> listProviders();

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
}

/// Calls into the Rust core through flutter_rust_bridge.
class RustBackend implements RegaBackend {
  @override
  List<ProviderInfo> listProviders() => rust.listProviders();

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
}

extension MachineOfferX on MachineOffer {
  bool get isGpu => gpuModel != null;
}
