import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:http/http.dart' as http;
import 'package:locho_flutter/locho_flutter.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const hostId = String.fromEnvironment('LOCHO_HOST_ID');
  const capability = String.fromEnvironment('LOCHO_CAPABILITY');
  const direct = String.fromEnvironment('LOCHO_DIRECT_ADDRESS');
  const ticketText = String.fromEnvironment('LOCHO_TICKET');
  testWidgets('native attach, health, lifecycle and cleanup', (_) async {
    expect(hostId, isNotEmpty, reason: 'Provide LOCHO_HOST_ID');
    expect(capability, isNotEmpty, reason: 'Provide LOCHO_CAPABILITY');
    final client = await LochoClient.create();
    final ticket = ticketText.isEmpty
        ? null
        : await LochoTicket.parse(ticketText);
    final httpClient = http.Client();
    try {
      final first = await client.attach(
        ticket: ticket,
        hostId: ticket == null ? hostId : null,
        capability: ticket == null ? capability : null,
        directAddress: direct.isEmpty ? null : direct,
      );
      expect(first.currentStatus.state, LochoState.connected);
      expect(first.httpBaseUri.host, '127.0.0.1');
      final response = await httpClient
          .get(first.httpBaseUri.resolve('/health'))
          .timeout(const Duration(seconds: 15));
      expect(response.statusCode, 200);
      expect(response.body, contains('locho-notes'));
      final subscription = first.statuses.listen((_) {});
      await subscription.cancel();
      // Unsubscribe must finish while the owner is connected and remains usable.
      final pending = first.statuses.listen((_) {});
      await Future<void>.delayed(const Duration(milliseconds: 100));
      await pending.cancel().timeout(const Duration(seconds: 3));
      final afterUnsubscribe = await httpClient.get(
        first.httpBaseUri.resolve('/health'),
      );
      expect(afterUnsubscribe.statusCode, 200);
      await Future.wait([first.close(), first.close()]);
      expect(first.currentStatus.state, LochoState.closed);
      // Runtime and client survive closing an attachment.
      final second = await client.attach(
        hostId: hostId,
        capability: capability,
        directAddress: direct.isEmpty ? null : direct,
      );
      final again = await httpClient
          .get(second.httpBaseUri.resolve('/health'))
          .timeout(const Duration(seconds: 15));
      expect(again.statusCode, 200);
      await client.close();
      expect(second.currentStatus.state, LochoState.closed);
      final rebound = await ServerSocket.bind('127.0.0.1', second.localPort);
      await rebound.close();
      final recreated = await LochoClient.create();
      try {
        await expectLater(
          recreated.start(hostId: 'invalid', capability: capability),
          throwsA(
            isA<LochoException>().having(
              (e) => e.code,
              'code',
              'invalidHostId',
            ),
          ),
        );
        final parts = capability.split(':');
        final wrong = await recreated.attach(
          hostId: hostId,
          capability: '${parts[0]}:tcp:wrong-secret',
          directAddress: direct.isEmpty ? null : direct,
        );
        expect(wrong.currentStatus.state, LochoState.connected);
        await expectLater(
          httpClient
              .get(wrong.httpBaseUri.resolve('/health'))
              .timeout(const Duration(seconds: 15)),
          throwsException,
        );
        await wrong.close();
        final reserved = await ServerSocket.bind('127.0.0.1', 0);
        final port = reserved.port;
        await reserved.close();
        await expectLater(
          recreated.attach(
            hostId: '00' * 32,
            capability: capability,
            localPort: port,
            directAddress: direct.isEmpty ? null : direct,
            connectTimeout: const Duration(milliseconds: 200),
          ),
          throwsA(
            isA<LochoException>().having(
              (e) => e.code,
              'code',
              'startupTimeout',
            ),
          ),
        );
        final freed = await ServerSocket.bind('127.0.0.1', port);
        await freed.close();
      } finally {
        await recreated.close();
      }
    } finally {
      httpClient.close();
      await client.close();
    }
  });
}
