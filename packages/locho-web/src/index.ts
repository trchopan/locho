import initWasm, { connect as connectWasm, parse_ticket, WebTicket, WebClient, WebCancellation } from './generated/locho_web.js';

export type LochoWasmInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;
let initialization: Promise<void> | undefined;
/** Optional explicit asset location. Configure before parsing tickets or connecting. */
export function initializeLocho(input?: LochoWasmInput): Promise<void> {
  if (!initialization) {
    initialization = initWasm(input === undefined ? undefined : { module_or_path: input }).then(() => undefined);
    initialization.catch(() => { initialization = undefined; });
  }
  return initialization;
}
const tickets = new WeakMap<LochoTicket, WebTicket>();
function rawTicket(ticket: LochoTicket): WebTicket {
  const raw = tickets.get(ticket);
  if (!raw) throw new LochoError('invalidTicket');
  return raw;
}

export type LochoState = 'starting' | 'connecting' | 'connected' | 'reconnecting' | 'failed' | 'closing' | 'closed';
export type LochoPath = 'unknown' | 'direct' | 'relay' | 'mixed';

export interface LochoStatus { revision: number; state: LochoState; path: LochoPath }
export class LochoError extends Error {
  constructor(readonly code: string) { super(`Locho error: ${code}`); }
}

export interface TicketMetadata {
  hostId: string; service: string; serviceType: 'http' | 'tcp'; peerRelayUrls: readonly string[];
}
export interface LochoRequestOptions {
  method?: 'GET' | 'HEAD' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'OPTIONS';
  headers?: HeadersInit;
  body?: string | Uint8Array | ArrayBuffer;
  signal?: AbortSignal;
  timeoutMs?: number;
}
export class LochoTicket {
  private constructor(raw: WebTicket) { tickets.set(this, raw); }
  static async parse(value: string): Promise<LochoTicket> {
    try { await initializeLocho(); return new LochoTicket(parse_ticket(value)); }
    catch (value) { throw error(value); }
  }
  get metadata(): TicketMetadata { return JSON.parse(rawTicket(this).metadata()); }
  export(): string { return rawTicket(this).export(); }
  toString(): string { return 'LochoTicket([REDACTED])'; }
  toJSON(): string { return this.toString(); }
  async connect(signal?: AbortSignal): Promise<LochoClient> {
    return LochoClient.connect({ ticket: this, signal });
  }
}

type RawClient = WebClient;

function error(value: unknown): LochoError {
  if (value instanceof LochoError) return value;
  return new LochoError(typeof value === 'string' ? value : 'internal');
}

async function cancellable<T>(signal: AbortSignal | undefined, run: (cancel: WebCancellation) => Promise<T>): Promise<T> {
  if (signal?.aborted) throw new LochoError('cancelled');
  const cancellation = new WebCancellation();
  const abort = () => cancellation.cancel();
  signal?.addEventListener('abort', abort, { once: true });
  try { return await run(cancellation); }
  catch (value) { throw error(value); }
  finally {
    signal?.removeEventListener('abort', abort);
    cancellation.free();
  }
}

export class LochoClient {
  private closing?: Promise<void>;
  private constructor(private readonly raw: RawClient, private readonly ticket?: LochoTicket, private readonly authority = 'locho.local') {}

  static async connect(options: { hostId?: string; peerRelayUrl?: string; ticket?: LochoTicket; signal?: AbortSignal; http?: { authority: string } }): Promise<LochoClient> {
    if (options.http && Object.keys(options.http).some((key) => key !== 'authority')) throw new LochoError('unsupportedOption');
    if (options.ticket ? options.hostId !== undefined || options.peerRelayUrl !== undefined : options.hostId === undefined) {
      throw new LochoError('conflictingOptions');
    }
    if (options.signal?.aborted) throw new LochoError('cancelled');
    try {
      await initializeLocho();
      return await cancellable(options.signal, async (cancel) => {
        const raw = options.ticket ? await rawTicket(options.ticket).connect(cancel)
          : await connectWasm(options.hostId!, options.peerRelayUrl, cancel) as RawClient;
        // Abort may race the completion of the WASM promise.
        if (options.signal?.aborted) {
          await raw.close();
          throw new LochoError('cancelled');
        }
        return new LochoClient(raw, options.ticket, options.http?.authority);
      });
    } catch (value) { throw error(value); }
  }
  get status(): LochoStatus {
    try { return JSON.parse(this.raw.status()) as LochoStatus; }
    catch { throw new LochoError('invalidStatus'); }
  }
  async fetch(path: string, options: LochoRequestOptions = {}): Promise<Response> {
    if (this.closing) throw new LochoError('closed');
    if (!this.ticket) throw new LochoError('ticketRequired');
    if (options.signal?.aborted) throw new LochoError('cancelled');
    const allowed = new Set(['method', 'headers', 'body', 'signal', 'timeoutMs']);
    if (Object.keys(options).some((key) => !allowed.has(key))) throw new LochoError('unsupportedOption');
    if (typeof path !== 'string') throw new LochoError('invalidPath');
    let body: Uint8Array;
    if (options.body === undefined) body = new Uint8Array();
    else if (typeof options.body === 'string') {
      if (options.body.length > 4 * 1024 * 1024) throw new LochoError('requestLimit');
      body = new TextEncoder().encode(options.body);
    }
    else if (options.body instanceof Uint8Array) body = options.body;
    else if (options.body instanceof ArrayBuffer) body = new Uint8Array(options.body);
    else throw new LochoError('invalidBody');
    if (body.byteLength > 4 * 1024 * 1024) throw new LochoError('requestLimit');
    const method = options.method ?? 'GET';
    let headers: [string, string][];
    try {
      const entries = options.headers instanceof Headers ? Array.from(options.headers.entries())
        : Array.isArray(options.headers) ? options.headers : Object.entries(options.headers ?? {});
      if (entries.some(([name, value]) => /[\r\n]/.test(name) || /[\r\n]/.test(value))) throw new Error();
      headers = Array.from(new Headers(options.headers).entries());
    }
    catch { throw new LochoError('invalidHeader'); }
    const timeoutMs = options.timeoutMs ?? 15000;
    if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 300000) throw new LochoError('invalidTimeout');
    const raw = await cancellable(options.signal, (cancel) => rawTicket(this.ticket!).request(this.raw,
      JSON.stringify({ method, path, authority: this.authority, headers, timeout_ms: timeoutMs }), body, cancel));
    try {
      if (options.signal?.aborted) throw new LochoError('cancelled');
      const metadata = JSON.parse(raw.metadata());
      const bytes = new Uint8Array(raw.body()).buffer;
      return new Response(method === 'HEAD' || [204, 205, 304].includes(metadata.status) ? null : bytes,
        { status: metadata.status, headers: metadata.headers });
    } catch (value) { throw error(value); }
    finally { raw.free(); }
  }
  async health(capability?: string, authority = 'sample-api', signal?: AbortSignal): Promise<Response> {
    if (this.closing) throw new LochoError('closed');
    if (this.ticket ? capability !== undefined : capability === undefined) throw new LochoError('conflictingOptions');
    const bytes = await cancellable(signal, (cancel) => this.ticket
      ? rawTicket(this.ticket).health(this.raw, authority, cancel) : this.raw.health(capability!, authority, cancel));
    if (signal?.aborted) throw new LochoError('cancelled');
    try {
      const separator = bytes.indexOf('\r\n\r\n');
      const head = separator < 0 ? bytes : bytes.slice(0, separator);
      const body = separator < 0 ? '' : bytes.slice(separator + 4);
      const statusCode = Number(head.split('\r\n')[0]?.split(' ')[1]);
      if (!Number.isInteger(statusCode)) throw new LochoError('invalidResponse');
      return new Response(body, { status: statusCode });
    } catch (value) { throw error(value); }
  }
  close(): Promise<void> {
    this.closing ??= this.raw.close().catch((value) => { throw error(value); });
    return this.closing;
  }
}
