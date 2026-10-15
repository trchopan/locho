import initWasm, { connect as connectWasm } from './generated/locho_web.js';

export type LochoState = 'starting' | 'connecting' | 'connected' | 'reconnecting' | 'failed' | 'closing' | 'closed';
export type LochoPath = 'unknown' | 'direct' | 'relay' | 'mixed';

export interface LochoStatus { revision: number; state: LochoState; path: LochoPath }
export class LochoError extends Error {
  constructor(readonly code: string) { super(`Locho error: ${code}`); }
}

function abortable<T>(operation: Promise<T>, signal: AbortSignal | undefined, onAbort: () => void): Promise<T> {
  if (!signal) return operation;
  if (signal.aborted) {
    onAbort();
    return Promise.reject(new LochoError('cancelled'));
  }
  return new Promise<T>((resolve, reject) => {
    const abort = () => { onAbort(); reject(new LochoError('cancelled')); };
    signal.addEventListener('abort', abort, { once: true });
    operation.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
  });
}

type RawClient = {
  status(): string;
  health(capability: string, authority: string): Promise<string>;
  close(): Promise<void>;
};

function status(raw: RawClient): LochoStatus {
  try { return JSON.parse(raw.status()) as LochoStatus; }
  catch { throw new LochoError('invalidStatus'); }
}
function error(value: unknown): LochoError {
  return new LochoError(typeof value === 'string' ? value : 'internal');
}

export class LochoClient {
  private constructor(private readonly raw: RawClient) {}

  static async connect(options: { hostId: string; peerRelayUrl?: string; signal?: AbortSignal }): Promise<LochoClient> {
    await initWasm();
    if (options.signal?.aborted) throw new LochoError('cancelled');
    try {
      const operation = connectWasm(options.hostId, options.peerRelayUrl) as Promise<RawClient>;
      const raw = await abortable(operation, options.signal, () => {
        void operation.then((late) => late.close()).catch(() => undefined);
      });
      return new LochoClient(raw);
    } catch (value) { throw error(value); }
  }
  get status(): LochoStatus { return status(this.raw); }
  async health(capability: string, authority = 'sample-api', signal?: AbortSignal): Promise<Response> {
    const operation = this.healthRaw(capability, authority);
    try {
      const bytes = await abortable(operation, signal, () => { void this.close(); });
      const separator = bytes.indexOf('\r\n\r\n');
      const head = separator < 0 ? bytes : bytes.slice(0, separator);
      const body = separator < 0 ? '' : bytes.slice(separator + 4);
      const line = head.split('\r\n')[0] ?? '';
      const statusCode = Number(line.split(' ')[1]);
      if (!Number.isInteger(statusCode)) throw new LochoError('invalidResponse');
      return new Response(body, { status: statusCode });
    } catch (value) { throw error(value); }
  }
  private async healthRaw(capability: string, authority: string): Promise<string> {
    return this.raw.health(capability, authority);
  }
  close(): Promise<void> { return this.raw.close().catch((value) => { throw error(value); }); }
}
