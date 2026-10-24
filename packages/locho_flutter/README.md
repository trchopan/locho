# Locho for Flutter

Connect an Android or iOS Flutter app to a private HTTP API through an encrypted
Locho tunnel. Your app uses familiar Dart HTTP clients such as `package:http` or
Dio; the SDK provides the local tunnel.

This beta is for **foreground Android and iOS apps**. Flutter Web, background
services and automatic request replay are not supported.

## Install

Add the package and an HTTP client to `pubspec.yaml`:

```yaml
dependencies:
  locho_flutter: 0.1.0-beta.3
  http: ^1.6.0
```

Then run:

```sh
flutter pub get
```

The package builds its native Rust library during your app build. You do not need
a Locho checkout, GitHub credentials or Rust code-generation commands.

## Requirements

- Flutter 3.44.5 or newer
- Dart 3.12.2 or newer
- Android API 23 or newer, JDK 21 and Android NDK 28.2.13676358
- Rust 1.96 or newer
- iOS deployment target 13 or newer, with Xcode and CocoaPods

The first native build can take several minutes while Rust dependencies compile.
The initial Android ABI set is ARM64 and x86_64. The package is not available for
Flutter Web or desktop Flutter targets.

## How It Works

```text
Your Flutter app
    -> http://127.0.0.1:<local-port>
    -> Locho attachment
    -> encrypted Locho connection
    -> private API on your homelab
```

The Locho ticket authorizes access to one configured service. It is separate from
your API's own login token or user authentication. Treat a ticket like a password.

## Get A Ticket

Configure the private API as a TCP service on the machine where it runs:

```toml
[[services]]
name = "sample-api"
type = "tcp"
endpoint = "127.0.0.1:8080"
```

Start Locho and create a ticket:

```sh
locho host --config locho.toml
locho share sample-api --config locho.toml --ticket
```

Copy the complete `locho1:...` value into your app. Do not put tickets in URLs,
logs, analytics or source control. For development, the SDK also accepts an
explicit host ID and capability instead of a ticket.

## First Request

The complete connection lifecycle looks like this:

```dart
import 'package:http/http.dart' as http;
import 'package:locho_flutter/locho_flutter.dart';

Future<void> checkApi(String ticketText) async {
  final ticket = await LochoTicket.parse(ticketText.trim());
  final client = await LochoClient.create();
  final api = http.Client();
  LochoAttachment? attachment;

  try {
    attachment = await client.attach(
      ticket: ticket,
      connectTimeout: const Duration(seconds: 15),
    );

    // Peer connection is not API readiness. Check the API explicitly.
    final response = await api
        .get(attachment.httpBaseUri.resolve('/health'))
        .timeout(const Duration(seconds: 15));

    if (response.statusCode != 200) {
      throw Exception('API health check failed: ${response.statusCode}');
    }
    print(response.body);
  } finally {
    api.close();
    await attachment?.close();
    await client.close();
  }
}
```

For a real screen or session, create the client, attachment and HTTP client once;
reuse them for multiple requests. Do not create a new tunnel for every request.
`attachment.httpBaseUri` is a loopback URI whose port is selected by the operating
system by default.

## Use It In An App

Keep tunnel ownership in a session/controller object rather than in a short-lived
widget:

```dart
final attachment = await client.attach(ticket: ticket);
final subscription = attachment.statuses.listen((status) {
  print('${status.state} via ${status.path} on port ${status.localPort}');
});

// Reuse attachment.httpBaseUri for package:http or Dio requests.

await subscription.cancel();
await attachment.close();
```

- `LochoState.connected` means the Locho host identity is reachable. It does not
  prove that the service capability or your API is ready. Call `/health` yourself.
- `LochoPath` reports whether the connection is direct, relayed or mixed when the
  path is observable.
- `LochoException.code` provides stable categories such as `invalidTicket`,
  `startupTimeout`, `unsupportedPlatform`, `unsupportedService`, `closed` and
  `resourceLimit`.
- A wrong capability can appear to an ordinary HTTP client as a closed or invalid
  local socket. It is not always observable as a typed authorization error.
- Close the HTTP client, status subscription, attachment and Locho client when a
  session ends. `close()` is safe to call more than once.
- Failed or interrupted writes are not retried automatically. Refresh explicitly
  before deciding whether a mutation succeeded.

## Android Setup

Add Internet permission to your app's **main** manifest, not only a debug manifest:

```xml
<uses-permission android:name="android.permission.INTERNET" />
```

The SDK uses HTTPS for relay traffic. If your private API is reached through the
local loopback HTTP address, allow cleartext traffic only for `127.0.0.1`:

```xml
<application
    android:networkSecurityConfig="@xml/network_security_config"
    ...>
</application>
```

```xml
<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
    <domain-config cleartextTrafficPermitted="true">
        <domain includeSubdomains="false">127.0.0.1</domain>
    </domain-config>
</network-security-config>
```

Do not enable cleartext traffic globally and do not disable TLS certificate
validation.

## iOS Setup

Use Xcode and CocoaPods with an iOS deployment target of 13 or newer. Add a local
network usage description to the consuming app's `Info.plist` when your setup needs
local network access:

```xml
<key>NSLocalNetworkUsageDescription</key>
<string>Connect to my private API through Locho.</string>
```

Signing, provisioning, App Store configuration and device permissions belong to the
consuming app. Physical iOS runtime coverage is still limited in this beta.

## Troubleshooting

### `invalidTicket`

Generate a new ticket with `locho share ... --ticket` and paste the complete value.
Do not add it to a URL or edit its contents. The app may trim clipboard whitespace
before parsing.

### `startupTimeout`

Check that the Locho host is running, the host can reach the configured upstream,
and the phone can reach the host directly or through the configured relay. A peer
connection alone does not validate the service.

### `/health` fails after attachment

Check the service capability, upstream address and API process separately. The SDK
does not automatically probe or silently retry the upstream request.

### Android native build fails

Check Flutter, Rust, JDK and NDK versions above, then retry from a clean app build.
The package compiles native code and needs registry/toolchain network access on a
cold machine.

## Example And Advanced Documentation

- [Runnable package example](https://github.com/trchopan/locho/tree/main/packages/locho_flutter/example)
- [Flutter package and source-build details](https://github.com/trchopan/locho/blob/main/docs/flutter-package.md)
- [Portable ticket format](https://github.com/trchopan/locho/blob/main/docs/tickets.md)
- [SDK release evidence and limitations](https://github.com/trchopan/locho/blob/main/docs/sdk-releases.md)
- [Full notes app using this SDK](https://github.com/trchopan/locho_sdk_examples/tree/main/apps/flutter)

The package owns the tunnel, not application accounts or credential storage. Store
remembered tickets through your app's secure-storage policy and never store API
passwords in the SDK.

## Beta Limitations

- Foreground Android and iOS only; no Flutter Web, desktop or background service.
- No automatic request replay, cookie jar or application authentication manager.
- QR scanning, physical iOS runtime, broad network-transition and authenticated
  custom-relay acceptance remain separate work.
- Native Rust source builds are required; prebuilt native binaries are not included.
