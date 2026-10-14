import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge.dart'
    show AnyhowException;

import 'src/rust/api/rega.dart' as rust;
import 'src/rust/api/rega.dart'
    show
        CredentialFieldInfo,
        MachineOffer,
        MachineSummary,
        OrphanSummary,
        ProviderInfo,
        RegaError,
        RegionChoices;
import 'src/rust/api/stream.dart' as stream;
import 'src/rust/api/stream.dart' show PlayUpdate, StreamStatus;

export 'src/rust/api/rega.dart'
    show
        CredentialFieldInfo,
        MachineOffer,
        MachineStatus,
        MachineSummary,
        OrphanKind,
        OrphanSummary,
        ProviderInfo,
        RegaError,
        RegionChoices,
        RegionOption;
export 'src/rust/api/stream.dart' show PlayStep, PlayUpdate, StreamStatus;

/// Everything the UI needs from the core. Widgets depend on this interface
/// so tests can swap in a fake without loading the Rust library.
abstract class RegaBackend {
  /// Signs in to every provider with the credentials saved in ReGa; one
  /// without working credentials shows demo data and says why. Calling it
  /// again reconnects.
  Future<List<ProviderInfo>> connectProviders();

  /// Signs in to the provider with what the user typed, by
  /// [CredentialFieldInfo.id], and saves it in the system keyring once the
  /// cloud accepts it. Throws a [RegaError], changing nothing, when the
  /// cloud refuses it.
  Future<ProviderInfo> saveCredentials(
    String providerId,
    Map<String, String> values,
  );

  /// Forgets the provider's saved credentials; it shows demo data until
  /// new ones are entered. Machines in the account are left alone.
  Future<ProviderInfo> forgetCredentials(String providerId);

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

  /// Connects to a running machine (through its WireGuard tunnel when it
  /// has one), pairs Moonlight if needed and starts the stream, reporting
  /// each step. The last update is [PlayStep.done]; a failure ends the
  /// stream with a [RegaError] instead.
  Stream<PlayUpdate> play(String providerId, String machineId);

  /// Hands the PIN Moonlight shows to the machine Play connected to.
  Future<void> pairWithPin(String providerId, String machineId, String pin);

  /// What Play is connected to, if anything.
  StreamStatus? streamStatus();

  /// Ends Play's connection, and with it a stream through the tunnel.
  Future<void> closeTunnel();
}

/// Calls into the Rust core through flutter_rust_bridge.
class RustBackend implements RegaBackend {
  @override
  Future<List<ProviderInfo>> connectProviders() => rust.connectProviders();

  @override
  Future<ProviderInfo> saveCredentials(
    String providerId,
    Map<String, String> values,
  ) => rust.saveCredentials(providerId: providerId, values: values);

  @override
  Future<ProviderInfo> forgetCredentials(String providerId) =>
      rust.forgetCredentials(providerId: providerId);

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

  @override
  Stream<PlayUpdate> play(String providerId, String machineId) => stream
      .play(providerId: providerId, machineId: machineId)
      .transform(
        StreamTransformer.fromHandlers(
          // The core sends its failures as plain messages.
          handleError: (e, trace, sink) => sink.addError(
            e is AnyhowException
                ? RegaError(message: e.message, gamesNotSaved: false)
                : e,
            trace,
          ),
        ),
      );

  @override
  Future<void> pairWithPin(String providerId, String machineId, String pin) =>
      stream.pairWithPin(
        providerId: providerId,
        machineId: machineId,
        pin: pin,
      );

  @override
  StreamStatus? streamStatus() => stream.streamStatus();

  @override
  Future<void> closeTunnel() => stream.closeTunnel();
}

extension MachineOfferX on MachineOffer {
  bool get isGpu => gpuModel != null;
}
