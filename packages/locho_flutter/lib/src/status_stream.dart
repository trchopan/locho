import 'dart:async';

/// One bounded native wait per observer, with no buffered updates while paused.
class StatusStreams<T> {
  final Future<T> Function() snapshot;
  final Future<T> Function(T) next;
  final bool Function(T) terminal;
  final bool Function(T, T) newer;
  final Object Function() limitError;
  int _active = 0;
  StatusStreams({
    required this.snapshot,
    required this.next,
    required this.terminal,
    required this.newer,
    required this.limitError,
  });

  Stream<T> observe() {
    late StreamController<T> controller;
    bool cancelled = false, paused = false, registered = false;
    Completer<void>? resume;
    Future<void>? running;
    Future<void> pump() async {
      try {
        var value = await snapshot();
        if (cancelled) return;
        controller.add(value);
        while (!terminal(value) && !cancelled) {
          if (paused) {
            resume ??= Completer<void>();
            await resume!.future;
          }
          if (cancelled) return;
          final update = await next(value);
          if (cancelled) return;
          if (paused) {
            resume ??= Completer<void>();
            await resume!.future;
          }
          if (cancelled) return;
          if (newer(update, value)) {
            value = update;
            controller.add(value);
          }
        }
      } catch (error, stack) {
        if (!cancelled) controller.addError(error, stack);
      } finally {
        if (registered) {
          registered = false;
          _active--;
        }
        if (!cancelled) unawaited(controller.close());
      }
    }

    controller = StreamController<T>(
      onListen: () {
        if (_active >= 16) {
          controller.addError(limitError());
          unawaited(controller.close());
          return;
        }
        registered = true;
        _active++;
        running = pump();
      },
      onPause: () {
        paused = true;
      },
      onResume: () {
        paused = false;
        final waiting = resume;
        resume = null;
        waiting?.complete();
      },
      onCancel: () async {
        cancelled = true;
        final waiting = resume;
        resume = null;
        waiting?.complete();
        await running;
      },
    );
    return controller.stream;
  }
}
