import initWasm, { connect as connectWasm, WebCancellation } from './generated/locho_web.js';

export type LochoState = 'starting' | 'connecting' | 'connected' | 'reconnecting' | 'failed' | 'closing' | 'closed';
export type LochoPath = 'unknown' | 'direct' | 'relay' | 'mixed';

export interface LochoStatus { revision: number; state: LochoState; path: LochoPath }
export class LochoError extends Error {
  constructor(readonly code: string) { super(`Locho error: ${code}`); }
}

type RawClient = {
  status(): string;
  health(capability: string, authority: string, cancellation: WebCancellation): Promise<string>;
  close(): Promise<void>;
};

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
  private constructor(private readonly raw: RawClient) {}

  static async connect(options: { hostId: string; peerRelayUrl?: string; signal?: AbortSignal }): Promise<LochoClient> {
    if (options.signal?.aborted) throw new LochoError('cancelled');
    try {
      await initWasm();
      return await cancellable(options.signal, async (cancel) => {
        const raw = await connectWasm(options.hostId, options.peerRelayUrl, cancel) as RawClient;
        // Abort may race the completion of the WASM promise.
        if (options.signal?.aborted) {
          await raw.close();
          throw new LochoError('cancelled');
        }
        return new LochoClient(raw);
      });
    } catch (value) { throw error(value); }
  }
  get status(): LochoStatus {
    try { return JSON.parse(this.raw.status()) as LochoStatus; }
    catch { throw new LochoError('invalidStatus'); }
  }
  async health(capability: string, authority = 'sample-api', signal?: AbortSignal): Promise<Response> {
    if (this.closing) throw new LochoError('closed');
    const bytes = await cancellable(signal, (cancel) => this.raw.health(capability, authority, cancel));
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
