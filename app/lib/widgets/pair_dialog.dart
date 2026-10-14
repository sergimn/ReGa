import 'package:flutter/material.dart';

/// Pairing by hand, for when ReGa can't run Moonlight itself (as inside
/// its Flatpak): the user adds the host in Moonlight, which shows a PIN,
/// and types that PIN here. Then they stream from Moonlight's own window.
class PairDialog extends StatefulWidget {
  const PairDialog({
    super.key,
    required this.machineName,
    required this.host,
    required this.throughTunnel,
    required this.paired,
    required this.onPair,
  });

  final String machineName;

  /// What to connect to in Moonlight: 127.0.0.1 through the tunnel, else
  /// the machine's public IP.
  final String host;

  /// Moonlight reaches the machine through ReGa's WireGuard tunnel, which
  /// closes with ReGa.
  final bool throughTunnel;

  /// Sunshine already has this device paired. Only a hint: Moonlight may
  /// have forgotten it, so pairing again stays on offer.
  final bool paired;

  /// Hands the PIN to Sunshine; throws when it isn't taken.
  final Future<void> Function(String pin) onPair;

  @override
  State<PairDialog> createState() => _PairDialogState();
}

class _PairDialogState extends State<PairDialog> {
  final _pin = TextEditingController();
  late bool _paired = widget.paired;
  bool _pairing = false;
  Object? _error;

  /// Asked for the PIN again, though Sunshine knows this device.
  bool _again = false;

  @override
  void dispose() {
    _pin.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    setState(() {
      _pairing = true;
      _error = null;
    });
    try {
      await widget.onPair(_pin.text.trim());
      if (!mounted) return;
      setState(() {
        _paired = true;
        _again = false;
        _pin.clear();
      });
    } catch (e) {
      if (mounted) setState(() => _error = e);
    }
    if (mounted) setState(() => _pairing = false);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final host = widget.host;
    final askPin = !_paired || _again;
    return AlertDialog(
      key: const ValueKey('pair-dialog'),
      title: Text('Play ${widget.machineName} in Moonlight'),
      content: SizedBox(
        width: 420,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (askPin) ...[
              Text(
                'In Moonlight, add the host $host (the + button), then pick '
                'it. Moonlight shows a 4-digit PIN: type it here.',
              ),
              const SizedBox(height: 12),
              TextField(
                key: const ValueKey('pin'),
                controller: _pin,
                autofocus: true,
                enabled: !_pairing,
                keyboardType: TextInputType.number,
                maxLength: 4,
                decoration: const InputDecoration(labelText: 'PIN'),
                onSubmitted: (_) => _submit(),
              ),
            ] else
              Text(
                'Paired. In Moonlight, open $host and start Desktop.'
                '${widget.throughTunnel ? ' The stream goes through ReGa, so keep it open.' : ''}',
              ),
            if (_error case final e?)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(
                  '$e',
                  key: const ValueKey('pair-error'),
                  style: TextStyle(color: theme.colorScheme.error),
                ),
              ),
          ],
        ),
      ),
      actions: [
        if (!askPin)
          TextButton(
            key: const ValueKey('pair-again'),
            onPressed: () => setState(() => _again = true),
            child: const Text('Pair again'),
          ),
        TextButton(
          key: const ValueKey('pair-close'),
          onPressed: () => Navigator.pop(context),
          child: Text(askPin ? 'Cancel' : 'Done'),
        ),
        if (askPin)
          FilledButton(
            key: const ValueKey('pair-submit'),
            onPressed: _pairing ? null : _submit,
            child: _pairing
                ? const SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('Pair'),
          ),
      ],
    );
  }
}
