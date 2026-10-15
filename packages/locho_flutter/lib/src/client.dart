import 'dart:async';
import 'dart:io';
import 'rust/api/simple.dart' as native;
import 'rust/frb_generated.dart';

enum LochoState {
  starting,
  connecting,
  connected,
  reconnecting,
  failed,
  closing,
  closed,
}

enum LochoPath { unknown, direct, relay, mixed }

class LochoException implements Exception {
  final String code;
  const LochoException(this.code);
  @override
  String toString() => 'LochoException($code)';
}

Future<T> _call<T>(Future<T> Function() operation) async {
  try {
    return await operation();
  } catch (error) {
    if (error is LochoException) rethrow;
    // Rust errors are deliberately fixed category strings, never credentials.
    const codes = {
      'closed',
      'cancelled',
      'invalidHostId',
      'invalidCapability',
      'invalidTicket',
      'conflictingOptions',
      'invalidPeerRelay',
      'unsupportedService',
      'invalidDirectAddress',
      'invalidTimeout',
      'startupTimeout',
      'listenerBind',
      'endpointBind',
      'shutdownTimeout',
      'peer',
      'internal',
      'runtime',
      'androidContext',
    };
    throw LochoException(
      error is String && codes.contains(error) ? error : 'internal',
    );
  }
}

class LochoStatus {
  final BigInt revision;
  final LochoState state;
  final LochoPath path;
  final int localPort;
  final String? errorCode;
  final String? connectionErrorCode;
  const LochoStatus({
    required this.revision,
    required this.state,
    required this.path,
    required this.localPort,
    this.errorCode,
    this.connectionErrorCode,
  });
  factory LochoStatus.fromNative(native.NativeStatus status) => LochoStatus(
    revision: status.revision,
    state: LochoState.values.byName(status.state),
    path: LochoPath.values.byName(status.path),
    localPort: status.localPort,
    errorCode: status.errorCode,
    connectionErrorCode: status.connectionErrorCode,
  );
}

class LochoTicket {
  final native.NativeTicket _native;
  final String hostId;
  final String service;
  final String serviceType;
  final List<String> peerRelayUrls;
  LochoTicket._(this._native, native.TicketMetadata metadata)
    : hostId = metadata.hostId,
      service = metadata.service,
      serviceType = metadata.serviceType,
      peerRelayUrls = List.unmodifiable(metadata.peerRelayUrls);

  static Future<LochoTicket> parse(String value) async {
    await LochoClient.initialize();
    final handle = await _call(() => native.parseTicket(value: value));
    return LochoTicket._(handle, await _call(handle.metadata));
  }

  Future<String> export() => _call(_native.export_);
  @override
  String toString() => 'LochoTicket([REDACTED])';
}

class LochoClient {
  static Future<void>? _initializing;
  final native.NativeClient _native;
  final Set<LochoAttachment> _attachments = {};
  bool _closed = false;
  Future<void>? _closing;
  LochoClient._(this._native);

  static Future<void> initialize() async {
    if (RustLib.instance.initialized) return;
    await (_initializing ??= RustLib.init());
  }

  static Future<LochoClient> create() async {
    if (!Platform.isAndroid && !Platform.isIOS) {
      throw const LochoException('unsupportedPlatform');
    }
    await initialize();
    return LochoClient._(await _call(native.createClient));
  }

  Future<LochoAttachment> start({
    String? hostId,
    String? capability,
    LochoTicket? ticket,
    int localPort = 0,
    String? directAddress,
  }) async {
    if (_closed) throw const LochoException('closed');
    if (ticket != null && (hostId != null || capability != null)) {
      throw const LochoException('conflictingOptions');
    }
    if (ticket == null && (hostId == null || capability == null)) {
      throw const LochoException('conflictingOptions');
    }
    if (localPort < 0 || localPort > 65535) {
      throw const LochoException('invalidPort');
    }
    final encodedTicket = ticket == null ? null : await ticket.export();
    final handle = await _call(
      () => _native.start(
        hostId: hostId ?? '',
        capability: capability ?? '',
        ticket: encodedTicket,
        localPort: localPort,
        directAddress: directAddress,
      ),
    );
    final attachment = LochoAttachment._(handle);
    attachment._onClosed = () => _attachments.remove(attachment);
    if (_closed) {
      await attachment.close();
      throw const LochoException('closed');
    }
    try {
      await attachment.snapshot();
      if (_closed) {
        await attachment.close();
        throw const LochoException('closed');
      }
      _attachments.add(attachment);
      return attachment;
    } catch (_) {
      await attachment.close();
      rethrow;
    }
  }

  /// Covers setup and connection. Timeout cleans up even a late native start.
  /// Dart futures are not cancellable: retain the future to close its late result.
  Future<LochoAttachment> attach({
    String? hostId,
    String? capability,
    LochoTicket? ticket,
    int localPort = 0,
    String? directAddress,
    Duration connectTimeout = const Duration(seconds: 15),
  }) async {
    if (connectTimeout <= Duration.zero ||
        connectTimeout > const Duration(seconds: 300)) {
      throw const LochoException('invalidTimeout');
    }
    final clock = Stopwatch()..start();
    final starting = start(
      hostId: hostId,
      capability: capability,
      ticket: ticket,
      localPort: localPort,
      directAddress: directAddress,
    );
    LochoAttachment? attachment;
    try {
      attachment = await starting.timeout(connectTimeout);
      final remaining = connectTimeout - clock.elapsed;
      if (remaining <= Duration.zero) throw TimeoutException('startup');
      await attachment.waitConnected(remaining);
      return attachment;
    } on TimeoutException {
      if (attachment != null) {
        await attachment.close();
      } else {
        try {
          await (await starting).close();
        } on LochoException {
          // Failed native startup owns its own cleanup; retain timeout category.
        }
      }
      throw const LochoException('startupTimeout');
    } catch (_) {
      if (attachment != null) await attachment.close();
      rethrow;
    }
  }

  Future<void> close() => _closing ??= _close();
  Future<void> _close() async {
    _closed = true;
    LochoException? firstError;
    // Rust client closes in-progress starts and all native handles concurrently.
    try {
      await _call(_native.close);
    } catch (error) {
      if (error is LochoException) {
        firstError = error;
      } else {
        firstError = const LochoException('internal');
      }
    }
    for (final attachment in _attachments.toList()) {
      try {
        await attachment.close();
      } catch (error) {
        firstError ??= error is LochoException
            ? error
            : const LochoException('internal');
      }
    }
    _attachments.clear();
    if (firstError != null) {
      throw firstError;
    }
  }
}

class LochoAttachment {
  final native.NativeAttachment _native;
  LochoStatus? _current;
  Future<void>? _closing;
  void Function()? _onClosed;
  LochoAttachment._(this._native);

  LochoStatus get currentStatus => _current!;
  int get localPort => currentStatus.localPort;
  Uri get httpBaseUri =>
      Uri(scheme: 'http', host: '127.0.0.1', port: localPort);

  Future<LochoStatus> snapshot() async {
    final status = LochoStatus.fromNative(await _call(_native.snapshot));
    if (_current == null || status.revision >= _current!.revision) {
      _current = status;
    }
    return _current!;
  }

  /// Pull-based/coalescing: each subscriber has at most one native wait in flight.
  /// Cancelling the subscription does not close its attachment.
  Stream<LochoStatus> get statuses async* {
    var status = await snapshot();
    yield status;
    while (status.state != LochoState.closed) {
      final next = LochoStatus.fromNative(
        await _call(() => _native.nextStatus(revision: status.revision)),
      );
      if (next.revision > status.revision) {
        status = next;
        if (_current == null || status.revision >= _current!.revision) {
          _current = status;
        }
        yield status;
      }
    }
  }

  Future<void> waitConnected(Duration timeout) async {
    if (timeout <= Duration.zero || timeout > const Duration(seconds: 300)) {
      throw const LochoException('invalidTimeout');
    }
    await _call(() => _native.waitConnected(timeoutMs: timeout.inMilliseconds));
    await snapshot();
  }

  Future<void> close() => _closing ??= _close();
  Future<void> _close() async {
    Object? closeError;
    try {
      await _call(_native.close);
    } catch (error) {
      closeError = error;
    }
    try {
      await snapshot();
    } catch (error) {
      closeError ??= error;
    }
    _onClosed?.call();
    if (closeError case final error?) throw error;
  }
}
