import 'package:flutter/material.dart';

import '../backend.dart';

/// Asks for a provider's credentials and signs in with them. Pops with the
/// signed-in [ProviderInfo]; a refusal stays in the dialog, so the user can
/// fix a typo.
class CredentialsDialog extends StatefulWidget {
  const CredentialsDialog({
    super.key,
    required this.backend,
    required this.provider,
  });

  final RegaBackend backend;
  final ProviderInfo provider;

  @override
  State<CredentialsDialog> createState() => _CredentialsDialogState();
}

class _CredentialsDialogState extends State<CredentialsDialog> {
  late final Map<String, TextEditingController> _values = {
    for (final f in widget.provider.credentialFields)
      f.id: TextEditingController(text: f.value ?? ''),
  };

  /// Secret fields the user chose to show while typing.
  final Set<String> _shown = {};
  bool _saving = false;
  Object? _error;

  @override
  void dispose() {
    for (final c in _values.values) {
      c.dispose();
    }
    super.dispose();
  }

  bool get _complete => _values.values.every((c) => c.text.trim().isNotEmpty);

  Future<void> _save() async {
    if (_saving || !_complete) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      final info = await widget.backend.saveCredentials(widget.provider.id, {
        for (final e in _values.entries) e.key: e.value.text,
      });
      if (mounted) Navigator.pop(context, info);
    } catch (e) {
      if (mounted) {
        setState(() {
          _saving = false;
          _error = e;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final fields = widget.provider.credentialFields;
    return AlertDialog(
      title: Text('${widget.provider.name} credentials'),
      content: SizedBox(
        width: 480,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'ReGa signs in with what you enter here and nothing else, '
              'even if this computer has other credentials set up. They are '
              'kept in your system keyring.',
              style: theme.textTheme.bodyMedium,
            ),
            for (final (i, f) in fields.indexed) ...[
              const SizedBox(height: 16),
              TextField(
                key: ValueKey('credential-${f.id}'),
                controller: _values[f.id],
                enabled: !_saving,
                autofocus: i == 0,
                obscureText: f.secret && !_shown.contains(f.id),
                enableSuggestions: !f.secret,
                autocorrect: false,
                textInputAction: i == fields.length - 1
                    ? TextInputAction.done
                    : TextInputAction.next,
                onChanged: (_) => setState(() {}),
                onSubmitted: (_) => _save(),
                decoration: InputDecoration(
                  labelText: f.label,
                  border: const OutlineInputBorder(),
                  suffixIcon: f.secret
                      ? IconButton(
                          key: ValueKey('show-${f.id}'),
                          tooltip: _shown.contains(f.id) ? 'Hide' : 'Show',
                          icon: Icon(
                            _shown.contains(f.id)
                                ? Icons.visibility_off_outlined
                                : Icons.visibility_outlined,
                          ),
                          onPressed: () => setState(() {
                            if (!_shown.remove(f.id)) _shown.add(f.id);
                          }),
                        )
                      : null,
                ),
              ),
            ],
            if (_error case final e?) ...[
              const SizedBox(height: 16),
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(Icons.error_outline, color: theme.colorScheme.error),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      '$e',
                      key: const ValueKey('credentials-error'),
                      style: TextStyle(color: theme.colorScheme.error),
                    ),
                  ),
                ],
              ),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          key: const ValueKey('credentials-cancel'),
          onPressed: _saving ? null : () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const ValueKey('credentials-save'),
          onPressed: _saving || !_complete ? null : _save,
          child: _saving
              ? const SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              : const Text('Sign in'),
        ),
      ],
    );
  }
}
