const milestones = 'locho=debug,locho_browser_repro=debug,n0_trace=debug,locho_core=debug';
export function hostTraceFilter(mode) {
  if (mode === 'none') return 'off';
  if (mode === 'milestones') return milestones;
  if (mode === 'frames') return `${milestones},noq=debug,noq_proto::connection::packet_builder=trace`;
  if (mode === 'transport') return `${milestones},iroh=debug,iroh_relay=debug,noq=debug,noq_proto::connection=trace`;
  throw new Error('invalidHostTraceMode');
}
