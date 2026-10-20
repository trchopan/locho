import asyncio
import json
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from loadgen import (
    MAX_FAILURE_EVENTS,
    MAX_LATENCY_SAMPLES,
    expected_http_body,
    http_once,
    main,
    percentile,
    read_http_response,
)


class LoadgenUnitTests(unittest.TestCase):
    def test_percentile_handles_empty_and_bounds(self):
        self.assertEqual(percentile([], 50), 0)
        self.assertEqual(percentile([1, 2, 3], 0), 1)
        self.assertEqual(percentile([1, 2, 3], 99), 3)

    def test_latency_sample_limit_is_bounded(self):
        self.assertEqual(MAX_LATENCY_SAMPLES, 100_000)

    def test_failure_event_limit_is_bounded(self):
        self.assertEqual(MAX_FAILURE_EVENTS, 1_000)

    def test_failure_events_are_capped_but_counts_continue(self):
        async def always_fails(*_args, **_kwargs):
            raise ConnectionRefusedError("test")

        async def run():
            with tempfile.TemporaryDirectory() as directory:
                args = SimpleNamespace(
                    protocol="http",
                    duration=0.05,
                    concurrency=1,
                    size=0,
                    request_size=0,
                    timeout=0.01,
                    churn_interval=0,
                    success_sample_rate=1,
                    interval=0.01,
                    output=f"{directory}/summary.json",
                    events=f"{directory}/events.jsonl",
                )
                with patch("loadgen.http_once", side_effect=always_fails):
                    result = await main(args)
                with open(args.output) as source:
                    summary = json.load(source)
                return result, summary

        result, summary = asyncio.run(run())
        self.assertEqual(result, 1)
        self.assertEqual(summary["failure_events_recorded"], MAX_FAILURE_EVENTS)
        self.assertTrue(summary["failure_events_limited"])
        self.assertGreater(summary["counts"]["failure"], MAX_FAILURE_EVENTS)

    def test_chunked_http_response_is_decoded(self):
        async def run():
            reader = asyncio.StreamReader()
            reader.feed_data(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
                b"4\r\ntest\r\n0\r\n\r\n"
            )
            reader.feed_eof()
            return await read_http_response(reader)

        self.assertEqual(asyncio.run(run()), (200, b"test"))

    def test_expected_http_body_is_deterministic(self):
        expected = expected_http_body("GET", 64)
        self.assertEqual(expected, expected_http_body("GET", 64))
        self.assertNotEqual(expected, expected_http_body("POST", 64))

    def test_http_response_content_is_validated_not_just_sized(self):
        async def run():
            reader = asyncio.StreamReader()
            reader.feed_data(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nBAD!")
            reader.feed_eof()

            class Writer:
                def __init__(self):
                    self.closed = False

                def write(self, _data):
                    pass

                async def drain(self):
                    pass

                def close(self):
                    self.closed = True

                async def wait_closed(self):
                    pass

            writer = Writer()
            with self.assertRaisesRegex(RuntimeError, "response body"):
                with patch("loadgen.asyncio.open_connection", return_value=(reader, writer)):
                    await http_once(0, 4, 1)
            return writer.closed

        self.assertTrue(asyncio.run(run()))

    def test_allow_transient_failures_does_not_allow_validation_failures(self):
        async def run(error):
            with tempfile.TemporaryDirectory() as directory:
                args = SimpleNamespace(
                    protocol="http",
                    duration=0.01,
                    concurrency=1,
                    size=0,
                    request_size=0,
                    timeout=0.01,
                    churn_interval=0,
                    success_sample_rate=1,
                    interval=0.01,
                    allow_transient_failures=True,
                    output=f"{directory}/summary.json",
                    events=f"{directory}/events.jsonl",
                )
                with patch("loadgen.http_once", side_effect=error):
                    result = await main(args)
                with open(args.output) as source:
                    summary = json.load(source)
                return result, summary

        result, summary = asyncio.run(run(ConnectionRefusedError("test")))
        self.assertEqual(result, 0)
        self.assertGreater(summary["counts"]["transient_failure"], 0)

        result, summary = asyncio.run(run(RuntimeError("corrupt response")))
        self.assertEqual(result, 1)
        self.assertGreater(summary["counts"]["validation_failure"], 0)


if __name__ == "__main__":
    unittest.main()
