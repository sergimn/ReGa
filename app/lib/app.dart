import 'package:flutter/material.dart';

import 'backend.dart';
import 'provider_page.dart';
import 'theme.dart';

class RegaApp extends StatelessWidget {
  const RegaApp({super.key, required this.backend});

  final RegaBackend backend;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ReGa',
      theme: regaTheme(Brightness.light),
      darkTheme: regaTheme(Brightness.dark),
      home: HomeShell(backend: backend),
    );
  }
}

/// Side menu of cloud platforms, with the selected one on the right.
class HomeShell extends StatefulWidget {
  const HomeShell({super.key, required this.backend});

  final RegaBackend backend;

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  late final List<ProviderInfo> _providers = widget.backend.listProviders();
  int _selected = 0;

  @override
  Widget build(BuildContext context) {
    if (_providers.isEmpty) {
      return const Scaffold(
        body: Center(child: Text('No platforms available')),
      );
    }
    final provider = _providers[_selected];
    return Scaffold(
      body: Row(
        children: [
          NavigationRail(
            selectedIndex: _selected,
            labelType: NavigationRailLabelType.all,
            leading: Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Column(
                children: [
                  Image.asset(
                    'assets/icon/rega-128.png',
                    width: 40,
                    height: 40,
                    semanticLabel: 'ReGa',
                  ),
                  const SizedBox(height: 16),
                  const Text('Platforms'),
                ],
              ),
            ),
            destinations: [
              for (final p in _providers)
                NavigationRailDestination(
                  icon: const Icon(Icons.cloud_outlined),
                  selectedIcon: const Icon(Icons.cloud),
                  label: Text(p.name),
                ),
            ],
            onDestinationSelected: (i) => setState(() => _selected = i),
          ),
          const VerticalDivider(width: 1),
          Expanded(
            child: ProviderPage(
              key: ValueKey(provider.id),
              backend: widget.backend,
              provider: provider,
            ),
          ),
        ],
      ),
    );
  }
}
