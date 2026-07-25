import argparse
import asyncio
import json
import time


MAX_LATENCY_SAMPLES = 100_000


def percentile(values, p):
    if not values:
        return 0
    values = sorted(values)
    return values[min(len(values) - 1, max(0, int(len(values) * p / 100)))]


async def read_http_response(reader):
    header_bytes = await reader.readuntil(b"\r\n\r\n")
    headers = header_bytes.decode("latin1").split("\r\n")
    status = int(headers[0].split()[1])
    header_map = {
        name.lower(): value.strip()
        for line in headers[1:]
        if ":" in line
        for name, value in [line.split(":", 1)]
    }
    if header_map.get("transfer-encoding", "").lower() == "chunked":
        chunks = []
        while True:
            length = int((await reader.readline()).split(b";", 1)[0], 16)
            if length == 0:
                await reader.readexactly(2)
                break
            chunks.append(await reader.readexactly(length))
            await reader.readexactly(2)
        body = b"".join(chunks)
    elif "content-length" in header_map:
        body = await reader.readexactly(int(header_map["content-length"]))
    else:
        body = await reader.read()
    return status, body


async def http_once(request_size, response_size, timeout, stream=None):
    started = time.perf_counter()
    owns_stream = stream is None
    if stream is None:
        stream = await asyncio.wait_for(asyncio.open_connection("locho_client_http", 8765), timeout)
    reader, writer = stream
    payload = b"r" * request_size
    method = "POST" if request_size else "GET"
    request = (
        f"{method} /?size={response_size} HTTP/1.1\r\n"
        "Host: locho_client_http\r\n"
        f"Content-Length: {len(payload)}\r\n"
        "Connection: close\r\n\r\n"
    ).encode() + payload
    writer.write(request)
    await asyncio.wait_for(writer.drain(), timeout)
    status, body = await asyncio.wait_for(read_http_response(reader), timeout)
    if owns_stream:
        writer.close()
    if status != 200:
        raise RuntimeError(f"unexpected HTTP status {status}")
    if len(body) != response_size:
        raise RuntimeError(f"unexpected HTTP body size {len(body)} != {response_size}")
    return time.perf_counter() - started, stream


async def tcp_once(size, timeout, stream=None):
    started = time.perf_counter()
    owns_stream = stream is None
    if stream is None:
        stream = await asyncio.wait_for(asyncio.open_connection("locho_client_tcp", 9876), timeout)
    reader, writer = stream
    payload = b"l" * size
    writer.write(payload)
    await asyncio.wait_for(writer.drain(), timeout)
    received = await asyncio.wait_for(reader.readexactly(size), timeout)
    if received != payload:
        raise RuntimeError("TCP echo mismatch")
    return time.perf_counter() - started, stream


async def main(args):
    deadline = time.monotonic() + args.duration
    lock = asyncio.Lock()
    latencies = []
    counts = {"success": 0, "failure": 0, "timeout": 0, "reset": 0}
    events = 0

    with open(args.events, "w") as event_file:
        async def record(event):
            nonlocal events
            async with lock:
                events += 1
                event_file.write(json.dumps(event) + "\n")
                event_file.flush()

        async def worker():
            stream = None
            while time.monotonic() < deadline:
                try:
                    if args.protocol == "http":
                        latency, _ = await http_once(args.request_size, args.size, args.timeout)
                    else:
                        latency, stream = await tcp_once(args.size, args.timeout, stream)
                    latency_ms = latency * 1000
                    async with lock:
                        counts["success"] += 1
                        if len(latencies) < MAX_LATENCY_SAMPLES:
                            latencies.append(latency_ms)
                    await record({"ts": time.time(), "ok": True, "latency_ms": latency_ms})
                except asyncio.TimeoutError:
                    if stream is not None:
                        stream[1].close()
                        stream = None
                    async with lock:
                        counts["failure"] += 1
                        counts["timeout"] += 1
                    await record({"ts": time.time(), "ok": False, "reason": "timeout"})
                except (ConnectionResetError, BrokenPipeError, ConnectionRefusedError) as error:
                    if stream is not None:
                        stream[1].close()
                        stream = None
                    async with lock:
                        counts["failure"] += 1
                        counts["reset"] += 1
                    await record({"ts": time.time(), "ok": False, "reason": type(error).__name__})
                except Exception as error:
                    if stream is not None:
                        stream[1].close()
                        stream = None
                    async with lock:
                        counts["failure"] += 1
                    await record({"ts": time.time(), "ok": False, "reason": str(error)})
            if stream is not None:
                stream[1].close()

        await asyncio.gather(*(worker() for _ in range(args.concurrency)))

    summary = {
        "protocol": args.protocol,
        "duration_seconds": args.duration,
        "concurrency": args.concurrency,
        "request_size": args.request_size if args.protocol == "http" else None,
        "message_size": args.size if args.protocol == "tcp" else None,
        "response_size": args.size if args.protocol == "http" else None,
        "counts": counts,
        "throughput_per_second": counts["success"] / args.duration if args.duration else 0,
        "latency_ms": {
            "p50": percentile(latencies, 50),
            "p95": percentile(latencies, 95),
            "p99": percentile(latencies, 99),
        },
        "events": events,
        "latency_samples": len(latencies),
    }
    with open(args.output, "w") as output:
        json.dump(summary, output, indent=2)
    print(json.dumps(summary), flush=True)
    return 0 if counts["failure"] == 0 else 1


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--protocol", choices=["http", "tcp"], required=True)
    parser.add_argument("--duration", type=float, required=True)
    parser.add_argument("--concurrency", type=int, required=True)
    parser.add_argument("--size", type=int, required=True)
    parser.add_argument("--request-size", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--output", required=True)
    parser.add_argument("--events", required=True)
    args = parser.parse_args()
    if args.duration <= 0 or args.concurrency <= 0 or args.size < 0 or args.request_size < 0:
        parser.error("duration and concurrency must be positive; sizes cannot be negative")
    return args


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main(parse_args())))
