export default function init(input?: RequestInfo | URL | Response | BufferSource | WebAssembly.Module): Promise<void>;
export class WebCancellation {
  constructor();
  cancel(): void;
  free(): void;
}
export class WebResponse {
  metadata(): string;
  body(): Uint8Array;
  free(): void;
}
export class WebClient {
  status(): string;
  health(capability: string, authority: string, cancellation: WebCancellation): Promise<string>;
  request(capability: string, options: string, body: Uint8Array, cancellation: WebCancellation): Promise<WebResponse>;
  close(): Promise<void>;
}
export class WebTicket {
  metadata(): string;
  export(): string;
  connect(cancellation: WebCancellation): Promise<WebClient>;
  health(client: WebClient, authority: string, cancellation: WebCancellation): Promise<string>;
  request(client: WebClient, options: string, body: Uint8Array, cancellation: WebCancellation): Promise<WebResponse>;
}
export function parse_ticket(value: string): WebTicket;
export function connect(hostId: string, peerRelayUrl: string | undefined, cancellation: WebCancellation): Promise<WebClient>;
