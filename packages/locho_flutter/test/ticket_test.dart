import 'dart:convert';
import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:locho_flutter/locho_flutter.dart';
import 'package:locho_flutter/src/rust/frb_generated.dart';
import 'package:locho_flutter/src/rust/api/simple.dart' as native;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

void main() {
  test(
    'shared ticket vectors use the Rust bridge',
    () async {
      final library = Platform.environment['LOCHO_TEST_LIBRARY'];
      if (library == null) {
        throw StateError(
          'LOCHO_TEST_LIBRARY must point to the built native test library',
        );
      }
      await RustLib.init(externalLibrary: ExternalLibrary.open(library));
      final vectors =
          jsonDecode(
                File(
                  '../../crates/locho-core/tests/vectors/tickets-v1.json',
                ).readAsStringSync(),
              )
              as Map<String, dynamic>;
      for (final vector in vectors['valid'] as List) {
        final ticket = await LochoTicket.parse(vector['ticket'] as String);
        expect(ticket.hostId, vector['host_id']);
        expect(await ticket.export(), vector['canonical']);
        expect(ticket.toString(), 'LochoTicket([REDACTED])');
        final client = await native.createClient();
        try {
          await expectLater(
            client.start(
              hostId: 'mixed',
              capability: '',
              localPort: 0,
              ticket: await ticket.export(),
            ),
            throwsA('conflictingOptions'),
          );
          if (ticket.serviceType == 'http') {
            await expectLater(
              client.start(
                hostId: '',
                capability: '',
                localPort: 0,
                ticket: await ticket.export(),
              ),
              throwsA('unsupportedService'),
            );
          }
        } finally {
          await client.close();
        }
      }
      for (final value in vectors['invalid'] as List) {
        await expectLater(
          LochoTicket.parse(value as String),
          throwsA(
            isA<LochoException>().having(
              (e) => e.code,
              'code',
              'invalidTicket',
            ),
          ),
        );
      }
    },
    skip: !Platform.environment.containsKey('LOCHO_TEST_LIBRARY'),
  );
}
