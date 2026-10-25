import 'package:flutter/material.dart';

void main() {
  runApp(const ArkmclApp());
}

/// 应用根组件。
class ArkmclApp extends StatelessWidget {
  const ArkmclApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ArkMCL',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.teal),
        useMaterial3: true,
      ),
      home: const HomePage(),
    );
  }
}

/// 首页。
///
/// 当前只展示应用信息；与 Rust 服务端或 CLI 的通信尚未实现，
/// 因此这里明确标注后端状态，而不是伪造可用的连接状态。
class HomePage extends StatelessWidget {
  const HomePage({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Scaffold(
      appBar: AppBar(title: const Text('ArkMCL')),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: Card(
            margin: const EdgeInsets.all(24),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('应用骨架已就绪', style: theme.textTheme.headlineSmall),
                  const SizedBox(height: 12),
                  Text(
                    '核心逻辑位于 Rust 的 arkmcl-core，CLI 为 arkmcl。'
                    'GUI 与服务端的通信尚未接入。',
                    style: theme.textTheme.bodyMedium,
                  ),
                  const SizedBox(height: 24),
                  const Chip(
                    avatar: Icon(Icons.link_off, size: 18),
                    label: Text('后端：未连接'),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
