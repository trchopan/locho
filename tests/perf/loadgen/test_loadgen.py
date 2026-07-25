import asyncio
import unittest

from loadgen import MAX_LATENCY_SAMPLES, percentile, read_http_response


class LoadgenUnitTests(unittest.TestCase):
    def test_percentile_handles_empty_and_bounds(self):
        self.assertEqual(percentile([], 50), 0)
        self.assertEqual(percentile([1, 2, 3], 0), 1)
        self.assertEqual(percentile([1, 2, 3], 99), 3)

    def test_latency_sample_limit_is_bounded(self):
        self.assertEqual(MAX_LATENCY_SAMPLES, 100_000)

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


if __name__ == "__main__":
    unittest.main()
