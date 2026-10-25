import { initializeLocho, LochoClient } from '../../src/index';
import { connect_repro, configure_repro_client_policy, initialize_repro_tracing, WebCancellation } from '../../dist/repro/generated/locho_web';
import { installRunner } from './runner.js';

installRunner({ initializeLocho, LochoClient,
  initializeTracing: initialize_repro_tracing,
  configurePolicy: configure_repro_client_policy,
  diagnosticStats: (client: any) => JSON.parse(client.raw.repro_stats()),
  connectControlled: async (options: any) => {
    const cancel = new WebCancellation();
    try {
      const raw = await connect_repro(options.hostId, options.relayUrl, options.homeRelayUrl ?? options.relayUrl, options.stale, options.lookupDelayMs, options.canonicalHome ?? false, cancel);
      // The wrapper and private fixture bindings share the SAME WASM module via
      // the build alias. Never cross module-owned handles.
      return new (LochoClient as any)(raw);
    } finally { cancel.free(); }
  },
});
