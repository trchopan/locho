// No transport dependencies: identical orchestration can run an unchanged
// packaged SDK or the explicitly instrumented developer build.
export function installRunner({ initializeLocho, LochoClient, initializeTracing, connectControlled, diagnosticStats, configurePolicy }) {
  globalThis.__lochoReproTrace ??= () => {};
  globalThis.lochoRepro = async (options) => {
    await initializeLocho();
    if (options.canonicalN0Home && !configurePolicy) throw new Error('instrumentedBuildRequired');
    configurePolicy?.(options.canonicalN0Home === true);
    if (options.trace !== 'none') initializeTracing?.(options.trace === 'transport');
    return await Promise.all([0, 1].map(async (index) => {
      let client;
      let stage = 'connect';
      const started = performance.now();
      const timeline = [];
      const mark = (stage) => timeline.push({ atMs: performance.now() - started, stage });
      mark('connect_start');
      try {
        if (options.mode === 'controlled') {
          if (!connectControlled) throw new Error('controlledBuildRequired');
          client = await connectControlled(options);
        } else {
          client = await LochoClient.connect({ hostId: options.hostId,
            peerRelayUrl: options.stale ? 'https://127.0.0.1:1/' : options.relayUrl });
        }
        mark('connected'); stage = 'health'; mark('health_start');
        const response = await client.health(options.capability, `repro-${options.cycle}-${index}`);
        const correctBody = (await response.text()).trim() === 'locho-repro-health';
        mark('health_complete');
        return { index, status: response.status, correctBody, path: client.status.path, timeline };
      } catch (error) {
        mark('failed');
        const code = error?.code;
        let transportStats;
        // No counter inspection until AFTER the operation has already failed.
        // Successful-client close timing and pending-request scheduling stay intact.
        if (client && diagnosticStats) {
          try { transportStats = diagnosticStats(client); } catch { transportStats = { unavailable: true }; }
        }
        let hostTransportStats;
        // The optional fixture callback runs only after failure and BEFORE close,
        // while the failed peer connection is still available for inspection.
        if (typeof globalThis.__lochoReproFailure === 'function') {
          try { hostTransportStats = await globalThis.__lochoReproFailure(); }
          catch { hostTransportStats = { unavailable: true }; }
        }
        return { index, stage, errorCode: typeof code === 'string' && /^[a-zA-Z]+$/.test(code) ? code : 'internal',
          status: client?.status, timeline, transportStats, hostTransportStats };
      } finally {
        try { await client?.close(); mark('closed'); }
        catch { mark('close_failed'); }
      }
    }));
  };
}
