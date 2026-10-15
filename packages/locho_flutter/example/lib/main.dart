import 'dart:async';
import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'package:locho_flutter/locho_flutter.dart';

void main() => runApp(const MaterialApp(home: TunnelScreen()));

class TunnelScreen extends StatefulWidget {
  const TunnelScreen({super.key});
  @override
  State<TunnelScreen> createState() => _TunnelScreenState();
}

class _TunnelScreenState extends State<TunnelScreen>
    with WidgetsBindingObserver {
  final host = TextEditingController();
  final ticket = TextEditingController();
  final capability = TextEditingController();
  final direct = TextEditingController();
  LochoClient? client;
  LochoAttachment? tunnel;
  http.Client? httpClient;
  StreamSubscription<LochoStatus>? subscription;
  int generation = 0;
  bool desired = false;
  bool connecting = false;
  String state = 'Disconnected';
  String result = '';
  Future<void> teardown = Future.value();

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  Future<void> disconnect({bool preserveIntent = false}) {
    if (!preserveIntent) desired = false;
    generation++;
    final oldClient = client;
    final oldSubscription = subscription;
    final oldHttp = httpClient;
    client = null;
    tunnel = null;
    subscription = null;
    httpClient = null;
    oldHttp?.close();
    connecting = false;
    if (mounted) {
      setState(() {
        state = 'Disconnected';
      });
    }
    return teardown = teardown.then((_) async {
      await oldSubscription?.cancel();
      try {
        await oldClient?.close();
      } on LochoException {
        /* displayed on next action */
      }
    });
  }

  Future<void> connect() async {
    desired = true;
    await disconnect(preserveIntent: true);
    final token = generation;
    await teardown;
    if (!mounted || !desired || token != generation) return;
    setState(() {
      connecting = true;
      state = 'Connecting to host';
      result = '';
    });
    LochoClient? created;
    try {
      created = await LochoClient.create();
      if (!mounted || token != generation) {
        await created.close();
        return;
      }
      client = created;
      final attachment = await created.attach(
        ticket: ticket.text.isEmpty
            ? null
            : await LochoTicket.parse(ticket.text),
        hostId: ticket.text.isEmpty ? host.text.trim() : null,
        capability: ticket.text.isEmpty ? capability.text.trim() : null,
        directAddress: direct.text.trim().isEmpty ? null : direct.text.trim(),
      );
      if (!mounted || token != generation) {
        await created.close();
        return;
      }
      tunnel = attachment;
      httpClient = http.Client();
      subscription = attachment.statuses.listen(
        (status) {
          if (mounted && token == generation) {
            setState(() {
              state =
                  '${status.state.name} / ${status.path.name} / port ${status.localPort}';
            });
          }
        },
        onError: (_) {
          if (mounted && token == generation) {
            setState(() {
              state = 'Status unavailable';
            });
          }
        },
      );
      setState(() {
        connecting = false;
      });
      await health();
    } catch (error) {
      await created?.close();
      if (mounted && token == generation) {
        setState(() {
          connecting = false;
          state = error is LochoException
              ? error.toString()
              : 'Connection failed';
        });
      }
    }
  }

  Future<void> health() async {
    final attachment = tunnel;
    final api = httpClient;
    final token = generation;
    if (attachment == null || api == null) return;
    final clock = Stopwatch()..start();
    try {
      // Health is an explicit API request, distinct from SDK peer connectivity.
      final request = http.AbortableRequest(
        'GET',
        attachment.httpBaseUri.resolve('/health'),
        abortTrigger: Future<void>.delayed(const Duration(seconds: 15)),
      );
      final response = await api.send(request);
      final bytes = <int>[];
      await for (final chunk in response.stream) {
        if (bytes.length + chunk.length > 65536) {
          api.close();
          throw StateError('Response limit');
        }
        bytes.addAll(chunk);
      }
      if (mounted && token == generation) {
        setState(() {
          result =
              'API ${response.statusCode}, ${clock.elapsedMilliseconds} ms\n${String.fromCharCodes(bytes).substring(0, bytes.length.clamp(0, 4096))}';
        });
      }
    } catch (_) {
      if (mounted && token == generation) {
        setState(() {
          result =
              'API request failed. Check service capability/upstream and transport; the cause is not uniquely identified.';
        });
      }
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState lifecycle) {
    if ([
      AppLifecycleState.hidden,
      AppLifecycleState.paused,
      AppLifecycleState.detached,
    ].contains(lifecycle)) {
      unawaited(disconnect(preserveIntent: true));
    } else if (lifecycle == AppLifecycleState.resumed &&
        desired &&
        !connecting &&
        tunnel == null) {
      unawaited(connect());
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    unawaited(disconnect());
    host.dispose();
    capability.dispose();
    ticket.dispose();
    direct.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Locho foreground smoke')),
    body: ListView(
      padding: const EdgeInsets.all(16),
      children: [
        TextField(
          controller: ticket,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: const InputDecoration(
            labelText: 'Ticket (or use development fields below)',
          ),
        ),
        TextField(
          controller: host,
          decoration: const InputDecoration(labelText: 'Host ID'),
        ),
        TextField(
          controller: capability,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: const InputDecoration(labelText: 'TCP capability'),
        ),
        TextField(
          controller: direct,
          decoration: const InputDecoration(
            labelText: 'Direct address (optional)',
          ),
        ),
        const SizedBox(height: 16),
        Wrap(
          spacing: 8,
          children: [
            FilledButton(
              onPressed: connecting ? null : connect,
              child: const Text('Connect'),
            ),
            OutlinedButton(
              onPressed: () => disconnect(),
              child: const Text('Disconnect'),
            ),
            OutlinedButton(
              onPressed: tunnel == null ? null : health,
              child: const Text('GET /health'),
            ),
          ],
        ),
        const SizedBox(height: 16),
        Text(state),
        SelectableText(result),
        const Text(
          'Peer connected is not API readiness. Foreground-only; settings are memory-only.',
        ),
      ],
    ),
  );
}
