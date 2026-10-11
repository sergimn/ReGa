import 'package:flutter/material.dart';

import '../backend.dart';
import 'format.dart';

/// One existing machine, with the actions its state allows.
class MachineTile extends StatelessWidget {
  const MachineTile({
    super.key,
    required this.machine,
    this.busy,
    required this.onStart,
    required this.onStop,
    required this.onSwitch,
    required this.onDestroy,
  });

  final MachineSummary machine;

  /// What is running on the machine right now, such as `Stopping…`; while
  /// set, no other action is offered.
  final String? busy;
  final VoidCallback onStart;
  final VoidCallback onStop;
  final VoidCallback onSwitch;
  final VoidCallback onDestroy;

  @override
  Widget build(BuildContext context) {
    final m = machine;
    final id = m.id;
    final actions = <Widget>[
      if (m.status == MachineStatus.running && !m.spot)
        TextButton(
          key: ValueKey('stop-$id'),
          onPressed: onStop,
          child: const Text('Stop'),
        ),
      if (m.status == MachineStatus.stopped)
        TextButton(
          key: ValueKey('start-$id'),
          onPressed: onStart,
          child: const Text('Start'),
        ),
      if (m.status == MachineStatus.stopped && m.switchType != null)
        TextButton(
          key: ValueKey('switch-$id'),
          onPressed: onSwitch,
          child: Text(m.offer.isGpu ? 'Switch to CPU' : 'Switch to GPU'),
        ),
      if (m.status != MachineStatus.pending &&
          m.status != MachineStatus.stopping)
        TextButton(
          key: ValueKey('destroy-$id'),
          onPressed: onDestroy,
          child: Text(
            m.status == MachineStatus.terminated ? 'Clean up' : 'Destroy',
          ),
        ),
    ];
    return ListTile(
      leading: const Icon(Icons.computer),
      title: Text(m.name),
      subtitle: Text(
        '${m.offer.vcpus} vCPU · ${formatGb(m.offer.ramGib)} RAM · '
        '${m.offer.gpuModel ?? 'No GPU'} · ${m.regionName}'
        '${m.spot ? ' · Spot' : ''}',
      ),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(busy ?? formatStatus(m.status)),
          const SizedBox(width: 8),
          if (busy != null)
            const SizedBox(
              width: 20,
              height: 20,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          else
            ...actions,
        ],
      ),
    );
  }
}
