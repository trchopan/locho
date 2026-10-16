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

export interface LochoStatus { revision: number; state: LochoState; path: LochoPath; connectionErrorCode: string | null; errorCode: string | null }
export interface LochoStatusOptions {
  signal?: AbortSignal;
  onError?: (error: LochoError) => void;
}
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
export interface LochoStreamRequestOptions extends Omit<LochoRequestOptions, 'timeoutMs'> {
  /** Connect, authorize, write request and receive final headers. Default 15000. */
  setupTimeoutMs?: number;
  /** Upstream read inactivity while pulling; consumer pauses do not count. Default 75000. */
  idleTimeoutMs?: number;
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
  private readonly streams = new Set<() => Promise<void>>();
  private closing?: Promise<void>;
  private wakeObservers!: () => void;
  private readonly closeStarted = new Promise<void>((resolve) => { this.wakeObservers = resolve; });
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
  /** Returns shared async unsubscribe; observation never owns client shutdown. */
  subscribeStatus(listener: (status: LochoStatus) => void | Promise<void>, options: LochoStatusOptions = {}): () => Promise<void> {
    if (options.signal?.aborted) return () => Promise.resolve();
    let observer;
    try { observer = this.raw.observe_status(); }
    catch (value) { throw error(value); }
    const cancellation = new WebCancellation();
    let stopped = false;
    let wake!: () => void;
    const aborted = new Promise<void>((resolve) => { wake = resolve; });
    const abort = () => { if (!stopped) { stopped = true; cancellation.cancel(); wake(); } };
    options.signal?.addEventListener('abort', abort, { once: true });
    const run = async () => {
      try {
        while (!stopped) {
          const value = await observer.next(cancellation);
          if (stopped || value === undefined) break;
          const status = JSON.parse(value) as LochoStatus;
          // Callbacks are serialized, so slow listeners coalesce watch revisions.
          await Promise.race([Promise.resolve().then(() => { if (!stopped) return listener(status); }), aborted, this.closeStarted]);
          if (status.state === 'closed') break;
        }
      } catch (value) {
        if (!stopped) { try { options.onError?.(error(value)); } catch { /* listener errors never affect transport */ } }
      } finally {
        stopped = true;
        options.signal?.removeEventListener('abort', abort);
        observer.free(); cancellation.free();
      }
    };
    const finished = run();
    return () => { abort(); return finished; };
  }
  private prepare(path: string, options: LochoRequestOptions) {
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
    return { method, body, options: JSON.stringify({ method, path, authority: this.authority, headers, timeout_ms: timeoutMs }) };
  }
  async fetch(path: string, options: LochoRequestOptions = {}): Promise<Response> {
    const request = this.prepare(path, options);
    const raw = await cancellable(options.signal, (cancel) => rawTicket(this.ticket!).request(this.raw,
      request.options, request.body, cancel));
    try {
      if (options.signal?.aborted) throw new LochoError('cancelled');
      const metadata = JSON.parse(raw.metadata());
      const bytes = new Uint8Array(raw.body()).buffer;
      return new Response(request.method === 'HEAD' || [204, 205, 304].includes(metadata.status) ? null : bytes,
        { status: metadata.status, headers: metadata.headers });
    } catch (value) { throw error(value); }
    finally { raw.free(); }
  }
  /** Resolves at final headers. Caller must consume or cancel the body. */
  async fetchStream(path: string, options: LochoStreamRequestOptions = {}): Promise<Response> {
    const allowed = new Set(['method', 'headers', 'body', 'signal', 'setupTimeoutMs', 'idleTimeoutMs']);
    if (Object.keys(options).some((key) => !allowed.has(key))) throw new LochoError('unsupportedOption');
    const idle = options.idleTimeoutMs ?? 75000;
    if (!Number.isInteger(idle) || idle < 1 || idle > 300000) throw new LochoError('invalidTimeout');
    const { setupTimeoutMs, idleTimeoutMs, ...base } = options;
    const request = this.prepare(path, { ...base, timeoutMs: setupTimeoutMs });
    const cancellation = new WebCancellation();
    let stop: (() => Promise<void>) | undefined;
    const abort = () => { cancellation.cancel(); void stop?.(); };
    options.signal?.addEventListener('abort', abort, { once: true });
    let raw: Awaited<ReturnType<WebTicket['request_stream']>> | undefined;
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      if (stop) this.streams.delete(stop);
      options.signal?.removeEventListener('abort', abort);
      raw?.free(); cancellation.free();
    };
    try {
      raw = await rawTicket(this.ticket!).request_stream(this.raw, request.options, request.body, idle, cancellation);
      if (options.signal?.aborted) throw new LochoError('cancelled');
      const metadata = JSON.parse(raw.metadata());
      if (request.method === 'HEAD' || [204, 205, 304].includes(metadata.status)) {
        const response = new Response(null, { status: metadata.status, headers: metadata.headers });
        release(); return response;
      }
      let pending: Promise<void> = Promise.resolve();
      let controller: ReadableStreamDefaultController<Uint8Array>;
      let stopped = false;
      let stopping: Promise<void> | undefined;
      const body = new ReadableStream<Uint8Array>({
        start: (value) => { controller = value; },
        pull: (controller) => pending = (async () => {
          try {
            const bytes = await raw!.next();
            if (stopped) return;
            if (bytes === undefined) { controller.close(); release(); }
            else controller.enqueue(bytes);
          } catch (value) { if (!stopped) { controller.error(error(value)); release(); } }
        })(),
        cancel: async () => {
          // next() borrows the WASM object until its promise settles. Signal first;
          // do not free a borrowed handle while a read is in flight.
          await stop!();
        },
      }, { highWaterMark: 0 });
      stop = () => {
        if (released) return Promise.resolve();
        return stopping ??= (async () => {
          stopped = true; raw!.cancel();
          controller.error(new LochoError(this.closing ? 'closed' : 'cancelled'));
          await pending;
          if (!released) { await raw!.finish(); release(); }
        })();
      };
      this.streams.add(stop);
      const response = new Response(body, { status: metadata.status, headers: metadata.headers });
      if (this.closing || options.signal?.aborted) { await stop(); throw new LochoError(this.closing ? 'closed' : 'cancelled'); }
      return response;
    } catch (value) { release(); throw error(value); }
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
    this.wakeObservers();
    if (!this.closing) {
      const shutdown = this.raw.close().catch((value) => { throw error(value); });
      this.closing = shutdown;
      this.closing = Promise.all([shutdown, ...Array.from(this.streams, (stop) => stop())]).then(() => undefined);
    }
    return this.closing;
  }
}
