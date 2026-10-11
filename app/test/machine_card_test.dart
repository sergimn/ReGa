import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rega/widgets/machine_card.dart';

import 'fake_backend.dart';

void main() {
  Future<void> pump(WidgetTester tester, Widget child) =>
      tester.pumpWidget(MaterialApp(home: Scaffold(body: child)));

  testWidgets('leads with specs and price, type name small', (tester) async {
    var taps = 0;
    await pump(
      tester,
      MachineCard(offer: gpuOffer, selected: false, onTap: () => taps++),
    );

    expect(find.text('4 vCPU'), findsOneWidget);
    expect(find.text('16 GB RAM'), findsOneWidget);
    expect(find.text('NVIDIA T4 · 16 GB VRAM'), findsOneWidget);
    expect(find.text('128 GB disk'), findsOneWidget);
    expect(find.text('\$0.56/h'), findsOneWidget);

    final typeName = tester.widget<Text>(find.text('g4dn.xlarge'));
    final price = tester.widget<Text>(find.text('\$0.56/h'));
    expect(typeName.style!.fontSize, lessThan(price.style!.fontSize!));

    await tester.tap(find.byType(MachineCard));
    expect(taps, 1);
  });

  testWidgets('CPU types say they have no GPU', (tester) async {
    await pump(
      tester,
      MachineCard(offer: cpuOffer, selected: true, onTap: () {}),
    );
    expect(find.text('No GPU'), findsOneWidget);
  });
}
