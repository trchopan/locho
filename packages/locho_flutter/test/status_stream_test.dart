import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:locho_flutter/src/status_stream.dart';

void main() {
  test(
    'resume delivers held update and completes at terminal without extra waits',
    () async {
      final wait = Completer<int>();
      final values = <int>[];
      int calls = 0;
      final streams = StatusStreams<int>(
        snapshot: () async => 0,
        next: (_) {
          calls++;
          return wait.future;
        },
        terminal: (v) => v == 9,
        newer: (a, b) => a > b,
        limitError: () => StateError('limit'),
      );
      final done = Completer<void>();
      final sub = streams.observe().listen(values.add, onDone: done.complete);
      await Future<void>.delayed(Duration.zero);
      sub.pause();
      wait.complete(9);
      await Future<void>.delayed(Duration.zero);
      expect(values, [0]);
      sub.resume();
      await done.future;
      expect(values, [0, 9]);
      expect(calls, 1);
    },
  );
  test(
    'unsubscribe during unchanged wait stops scheduling without owner close',
    () async {
      final wait = Completer<int>();
      int calls = 0;
      final values = <int>[];
      final streams = StatusStreams<int>(
        snapshot: () async => 0,
        next: (_) {
          calls++;
          return wait.future;
        },
        terminal: (v) => v == 9,
        newer: (a, b) => a > b,
        limitError: () => StateError('limit'),
      );
      final sub = streams.observe().listen(values.add);
      await Future<void>.delayed(Duration.zero);
      final cancelled = sub.cancel();
      wait.complete(0);
      await cancelled;
      await Future<void>.delayed(Duration.zero);
      expect(calls, 1);
      expect(values, [0]);
    },
  );
  test(
    'pause holds bounded delivery and cancellation releases paused pump',
    () async {
      final wait = Completer<int>();
      int calls = 0;
      final streams = StatusStreams<int>(
        snapshot: () async => 0,
        next: (_) {
          calls++;
          return wait.future;
        },
        terminal: (v) => v == 9,
        newer: (a, b) => a > b,
        limitError: () => StateError('limit'),
      );
      final values = <int>[];
      final sub = streams.observe().listen(values.add);
      await Future<void>.delayed(Duration.zero);
      sub.pause();
      wait.complete(1);
      await Future<void>.delayed(Duration.zero);
      expect(values, [0]);
      expect(calls, 1);
      await sub.cancel();
    },
  );
  test('observer cap and terminal snapshot', () async {
    final waits = List.generate(16, (_) => Completer<int>());
    int index = 0;
    final streams = StatusStreams<int>(
      snapshot: () async => 0,
      next: (_) => waits[index++].future,
      terminal: (v) => v == 9,
      newer: (a, b) => a > b,
      limitError: () => StateError('limit'),
    );
    final subs = List.generate(16, (_) => streams.observe().listen((_) {}));
    await Future<void>.delayed(Duration.zero);
    await expectLater(streams.observe(), emitsError(isA<StateError>()));
    final cancellation = Future.wait(subs.map((s) => s.cancel()));
    for (final wait in waits) {
      wait.complete(0);
    }
    await cancellation;
    final terminal = StatusStreams<int>(
      snapshot: () async => 9,
      next: (_) async => throw StateError('must not wait'),
      terminal: (v) => v == 9,
      newer: (a, b) => a > b,
      limitError: () => StateError('limit'),
    );
    await expectLater(terminal.observe(), emitsInOrder([9, emitsDone]));
  });
}
