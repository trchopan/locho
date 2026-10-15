export default function init(input?: RequestInfo | URL | Response | BufferSource | WebAssembly.Module): Promise<void>;
export class WebCancellation {
  constructor();
  cancel(): void;
  free(): void;
}
export function connect(hostId: string, peerRelayUrl: string | undefined, cancellation: WebCancellation): Promise<unknown>;
