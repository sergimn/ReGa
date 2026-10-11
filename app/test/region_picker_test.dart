import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/backend.dart';
import 'package:rega/widgets/region_picker.dart';

import 'fake_backend.dart';

void main() {
  const choices = RegionChoices(
    recommended: stockholm,
    lowestLatency: frankfurt,
    cheapest: virginia,
    all: [frankfurt, stockholm, virginia],
  );

  testWidgets('shows three choices and expands to all', (tester) async {
    RegionOption? picked;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: RegionPicker(
              choices: choices,
              selectedRegionId: 'eu-north-1',
              onSelected: (r) => picked = r,
            ),
          ),
        ),
      ),
    );

    expect(find.text('Recommended'), findsOneWidget);
    expect(find.text('Lowest latency'), findsOneWidget);
    expect(find.text('Cheapest'), findsOneWidget);
    expect(find.textContaining('38 ms · \$0.56/h'), findsOneWidget);
    expect(find.textContaining('\$4.00 once to copy games'), findsNWidgets(2));
    expect(find.text('Europe (Frankfurt)'), findsNothing);

    await tester.tap(find.text('Show all regions'));
    await tester.pump();
    expect(find.text('Europe (Frankfurt)'), findsOneWidget);
    expect(find.text('US East (N. Virginia)'), findsOneWidget);

    await tester.tap(find.text('Cheapest'));
    expect(picked?.regionId, 'us-east-1');
  });

  testWidgets('a region that wins twice is shown once', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: RegionPicker(
            choices: const RegionChoices(
              recommended: stockholm,
              lowestLatency: frankfurt,
              cheapest: stockholm,
              all: [frankfurt, stockholm],
            ),
            selectedRegionId: 'eu-north-1',
            onSelected: (_) {},
          ),
        ),
      ),
    );
    expect(find.text('Recommended · Cheapest'), findsOneWidget);
    expect(find.text('Lowest latency'), findsOneWidget);
  });

  testWidgets('a pick from the full list stays visible when collapsed', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: RegionPicker(
              choices: const RegionChoices(
                recommended: stockholm,
                lowestLatency: frankfurt,
                cheapest: stockholm,
                all: [frankfurt, stockholm, unmeasured],
              ),
              selectedRegionId: 'sa-east-1',
              onSelected: (_) {},
            ),
          ),
        ),
      ),
    );
    expect(find.text('Your pick'), findsOneWidget);
    expect(find.textContaining('Measuring…'), findsOneWidget);
  });

  testWidgets('explains when no region has GPU quota', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: RegionPicker(
            choices: const RegionChoices(all: []),
            selectedRegionId: null,
            onSelected: (_) {},
          ),
        ),
      ),
    );
    expect(find.textContaining('No region offers'), findsOneWidget);
  });
}
