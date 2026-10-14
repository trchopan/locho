import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'package:locho_flutter/locho_flutter.dart';

/// Autonomous entry point because Flutter Driver cannot execute release tests.
/// Only fixed pass/fail categories are printed; input credentials are never logged.
void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(
    const MaterialApp(
      home: Scaffold(body: Center(child: Text('Locho release smoke'))),
    ),
  );
  smoke();
}

Future<void> smoke() async {
  LochoClient? client;
  final api = http.Client();
  try {
    client = await LochoClient.create();
    for (var attempt = 0; attempt < 2; attempt++) {
      const direct = String.fromEnvironment('LOCHO_DIRECT_ADDRESS');
      final attachment = await client.attach(
        hostId: const String.fromEnvironment('LOCHO_HOST_ID'),
        capability: const String.fromEnvironment('LOCHO_CAPABILITY'),
        directAddress: direct.isEmpty ? null : direct,
      );
      final response = await api
          .get(attachment.httpBaseUri.resolve('/health'))
          .timeout(const Duration(seconds: 15));
      if (response.statusCode != 200 ||
          !response.body.contains('locho-notes')) {
        throw const LochoException('health');
      }
      await attachment.close();
      if (attachment.currentStatus.state != LochoState.closed) {
        throw const LochoException('cleanup');
      }
    }
    await client.close();
    // Fixed marker consumed by the physical-device harness.
    debugPrint('LOCHO_RELEASE_SMOKE_PASS');
  } catch (_) {
    debugPrint('LOCHO_RELEASE_SMOKE_FAIL');
  } finally {
    api.close();
    await client?.close();
  }
}
