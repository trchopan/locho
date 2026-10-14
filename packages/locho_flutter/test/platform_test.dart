import 'package:flutter_test/flutter_test.dart';
import 'package:locho_flutter/locho_flutter.dart';

void main() {
  test(
    'desktop consumer rejects before attempting to load mobile bindings',
    () async {
      await expectLater(
        LochoClient.create(),
        throwsA(
          isA<LochoException>().having(
            (error) => error.code,
            'code',
            'unsupportedPlatform',
          ),
        ),
      );
    },
  );
}
