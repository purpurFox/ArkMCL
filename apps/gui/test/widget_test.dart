import 'package:arkmcl_gui/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('首页显示应用名称与后端未连接状态', (tester) async {
    await tester.pumpWidget(const ArkmclApp());

    expect(find.text('ArkMCL'), findsOneWidget);
    expect(find.text('后端：未连接'), findsOneWidget);
  });
}
