import 'package:flutter/material.dart';

import 'backend.dart';
import 'provider_page.dart';
import 'theme.dart';
import 'widgets/about.dart';
import 'widgets/platform_icon.dart';

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
  late Future<List<ProviderInfo>> _providers = widget.backend
      .connectProviders();
  int _selected = 0;

  void _reconnect() {
    final providers = widget.backend.connectProviders();
    setState(() {
      _providers = providers;
    });
  }

  /// A provider the user signed in to or out of.
  void _changed(ProviderInfo info) {
    setState(() {
      _providers = _providers.then(
        (providers) => [for (final p in providers) p.id == info.id ? info : p],
      );
    });
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<List<ProviderInfo>>(
      future: _providers,
      builder: (context, snapshot) {
        if (snapshot.hasError) {
          return Scaffold(
            body: Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text('${snapshot.error}'),
                  TextButton(onPressed: _reconnect, child: const Text('Retry')),
                ],
              ),
            ),
          );
        }
        final providers = snapshot.data;
        if (providers == null) {
          return const Scaffold(
            body: Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  CircularProgressIndicator(),
                  SizedBox(height: 16),
                  Text('Signing in…'),
                ],
              ),
            ),
          );
        }
        return _shell(providers);
      },
    );
  }

  Widget _shell(List<ProviderInfo> providers) {
    if (providers.isEmpty) {
      return const Scaffold(
        body: Center(child: Text('No platforms available')),
      );
    }
    final selected = _selected.clamp(0, providers.length - 1);
    final provider = providers[selected];
    return Scaffold(
      body: Row(
        children: [
          NavigationRail(
            selectedIndex: selected,
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
              for (final p in providers)
                NavigationRailDestination(
                  icon: PlatformIcon(providerId: p.id),
                  selectedIcon: PlatformIcon(providerId: p.id, selected: true),
                  label: Text(p.name),
                ),
            ],
            onDestinationSelected: (i) => setState(() => _selected = i),
            trailingAtBottom: true,
            trailing: Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: IconButton(
                key: const ValueKey('about'),
                icon: const Icon(Icons.info_outline),
                tooltip: 'About ReGa',
                onPressed: () => showRegaAbout(context),
              ),
            ),
          ),
          const VerticalDivider(width: 1),
          Expanded(
            child: ProviderPage(
              // Signing in, out or to another account changes what the
              // page shows from the ground up.
              key: ValueKey(
                '${provider.id}-${provider.demo}-${provider.signedInAs}',
              ),
              backend: widget.backend,
              provider: provider,
              onReconnect: _reconnect,
              onProviderChanged: _changed,
            ),
          ),
        ],
      ),
    );
  }
}
