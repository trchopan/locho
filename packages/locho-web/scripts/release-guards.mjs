export function assertReleaseBindings(bindings) {
  if (/\b(connect_repro|connect_n0_repro|configure_repro_client_policy|initialize_repro_tracing|repro_stats)\b/.test(bindings)) {
    throw new Error('Investigation WASM bindings must never be published; rebuild without browser-repro');
  }
}
