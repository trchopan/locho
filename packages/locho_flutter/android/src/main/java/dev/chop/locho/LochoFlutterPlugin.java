package dev.chop.locho;

import io.flutter.embedding.engine.plugins.FlutterPlugin;

/** Initializes iroh DNS with an application context before Dart creates clients. */
public final class LochoFlutterPlugin implements FlutterPlugin {
    private static native boolean initializeContext(android.content.Context context);

    @Override
    public void onAttachedToEngine(FlutterPluginBinding binding) {
        System.loadLibrary("locho_flutter");
        if (!initializeContext(binding.getApplicationContext())) {
            throw new IllegalStateException("Locho Android context initialization failed");
        }
    }

    @Override
    public void onDetachedFromEngine(FlutterPluginBinding binding) {
        // Native context is the Application, retained for process lifetime.
        // Tunnel ownership is explicitly closed by Dart; engine lifetime is separate.
    }
}
