// Integration-only workload shared by page and service-worker soak tests.
import { LochoClient, LochoTicket, type LochoResponseWithMetadata } from '../../src/index';

const buckets = [100, 250, 500, 1000, 2500, 5000, 15000, 60000, Infinity];
function check(value: unknown, code: string): asserts value {
  if (!value) throw new Error(code); // Never include headers, bodies or tickets in errors.
}

export class MetadataSoak {
  private cycles = 0;
  private reconnects = 0;
  private capacityChecks = 0;
  private maxCycleMs = 0;
  private readonly histogram = buckets.map(() => 0);
  private constructor(private readonly ticket: LochoTicket, private client: LochoClient, private readonly authority: string) {}

  static async open(ticketText: string, authority: string): Promise<MetadataSoak> {
    const ticket = await LochoTicket.parse(ticketText);
    return new MetadataSoak(ticket, await LochoClient.connect({ ticket, http: { authority } }), authority);
  }

  private async consume(result: LochoResponseWithMetadata, status: number) {
    try {
      check(result.response.status === status, 'unexpectedStatus');
      await result.response.arrayBuffer();
    } finally { await result.response.body?.cancel().catch(() => {}); }
  }

  async batch() {
    for (let index = 0; index < 5; index++) {
      const start = performance.now();
      const request = this.cycles % 2 ? this.client.fetchStreamWithMetadata.bind(this.client) : this.client.fetchWithMetadata.bind(this.client);
      const login = await request('/cookie-login', { method: 'POST' });
      try {
        const cookies = login.upstreamHeaders.filter(([name]) => name === 'set-cookie');
        check(cookies.length === 2 && cookies[0][1] === 'session=synthetic; Path=/; HttpOnly; Secure; SameSite=Lax'
          && cookies[1][1] === 'preference=one; Expires=Wed, 21 Oct 2037 07:28:00 GMT; Path=/', 'cookieHeaderMismatch');
        check(Object.isFrozen(login.upstreamHeaders) && cookies.every(Object.isFrozen), 'mutableHeaders');
        await this.consume(login, 200);
        let session: string | undefined = cookies[0][1].split(';')[0];
        await this.consume(await request('/cookie-session', { headers: { Cookie: session } }), 200);
        const logout = await request('/cookie-logout', { method: 'POST', headers: { Cookie: session } });
        check(logout.upstreamHeaders.some(([name, value]) => name === 'set-cookie'
          && value === 'session=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; HttpOnly'), 'deletionHeaderMismatch');
        await this.consume(logout, 204);
        session = undefined;
        await this.consume(await request('/cookie-session', { headers: session ? { Cookie: session } : {} }), 401);
      } finally { await login.response.body?.cancel().catch(() => {}); }

      // Alternate cancellation of a pending read with AbortSignal cancellation.
      const abort = new AbortController();
      const stream = await this.client.fetchStreamWithMetadata('/stream-idle', { signal: abort.signal });
      try {
        check(stream.upstreamHeaders.some(([name, value]) => name === 'set-cookie' && value === 'stream=synthetic; Path=/; HttpOnly'), 'streamHeaderMismatch');
        const reader = stream.response.body!.getReader();
        try {
          check(!(await reader.read()).done, 'earlyStreamEnd');
          const pending = reader.read().then(() => 'finished', (error) => error.code);
          if (this.cycles % 2) { abort.abort(); check(await pending === 'cancelled', 'abortFailed'); }
          else { await reader.cancel(); await pending; }
        } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
      } finally { await stream.response.body?.cancel().catch(() => {}); }

      if (this.cycles % 5 === 0) {
        const bodies: Response[] = [];
        try {
          for (let count = 0; count < 16; count++) bodies.push((await this.client.fetchStreamWithMetadata('/stream-idle')).response);
          const excess = await this.client.fetchWithMetadata('/health').then(() => 'success', (error) => error.code);
          check(excess === 'resourceLimit', 'capacityLimitFailed');
        } finally { await Promise.all(bodies.map((response) => response.body?.cancel())); }
        await this.consume(await this.client.fetchWithMetadata('/health'), 200);
        this.capacityChecks++;
      }

      if (this.cycles % 10 === 0) {
        const unread = await this.client.fetchStreamWithMetadata('/stream-idle');
        const reading = await this.client.fetchStreamWithMetadata('/stream-idle');
        const reader = reading.response.body!.getReader();
        try {
          await reader.read();
          const pending = reader.read().then(() => 'finished', (error) => error.code);
          await this.client.close();
          check(await pending === 'closed', 'pendingCloseFailed');
          const closed = await unread.response.body!.getReader().read().then(() => 'finished', (error) => error.code);
          check(closed === 'closed', 'unreadCloseFailed');
        } finally { reader.releaseLock(); }
        this.client = await LochoClient.connect({ ticket: this.ticket, http: { authority: this.authority } });
        this.reconnects++;
      }

      const elapsed = performance.now() - start;
      this.maxCycleMs = Math.max(this.maxCycleMs, elapsed);
      this.histogram[buckets.findIndex((limit) => elapsed <= limit)]++;
      this.cycles++;
    }
    const percentile = (fraction: number) => {
      let count = 0;
      const upper = buckets.find((_, index) => { count += this.histogram[index]; return count >= Math.ceil(this.cycles * fraction); })!;
      return Number.isFinite(upper) ? upper : this.maxCycleMs;
    };
    return { cycles: this.cycles, reconnects: this.reconnects, capacityChecks: this.capacityChecks,
      cycleLatencyMs: { p50UpperBound: percentile(0.5), p95UpperBound: percentile(0.95), max: this.maxCycleMs } };
  }

  close() { return this.client.close(); }
}
