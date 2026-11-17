import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from native_support import linux_resource_observation


class ResourceObservationTests(unittest.TestCase):
    def test_owned_cpu_cgroup_deltas_and_loopback_socket_queues(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc, cg = root / "proc", root / "cgroup"
            process = proc / "100"
            (process / "fd").mkdir(parents=True)
            (process / "net").mkdir()
            (process / "fd/4").symlink_to("socket:[123]")
            (process / "cgroup").write_text("0::/session\n")
            fields = ["0"] * 50
            fields[11], fields[12], fields[19] = "100", "25", "777"
            (process / "stat").write_text("100 (name with ) parens) " + " ".join(fields))
            (process / "net/tcp").write_text("header\n0: 0100007F:1234 0100007F:5678 01 00000010:00000020 0:0 0 0 0 123\n"
                "1: 0100007F:1234 0100007F:5678 01 000000FF:000000FF 0:0 0 0 0 999\n")
            (process / "net/tcp6").write_text("header\n")
            (process / "net/unix").write_text("header\n0: 2 0 0 0001 03 123 /private/not-retained.sock\n")
            for path in (cg, cg / "session"):
                path.mkdir(exist_ok=True)
                for name, value in {"cpu.max": "max 100000", "cpu.stat": "usage_usec 200\nnr_throttled 2",
                                    "cpu.pressure": "some avg10=0.0 total=12\nfull avg10=0.0 total=4",
                                    "memory.max": "max", "memory.events": "oom_kill 0"}.items():
                    (path / name).write_text(value)
            with patch("native_support.time.monotonic", side_effect=[10, 12]), patch("native_support.os.sysconf", return_value=100):
                first = linux_resource_observation(100, proc_root=proc, cgroup_root=cg)
                fields[11] = "150"
                (process / "stat").write_text("100 (name) " + " ".join(fields))
                (cg / "session/cpu.stat").write_text("usage_usec 400\nnr_throttled 3")
                second = linux_resource_observation(100, first, proc_root=proc, cgroup_root=cg)
            self.assertEqual(second["interval_cpu_percent"], 25)
            self.assertEqual(second["cgroups"][0]["cpu.stat_delta"]["nr_throttled"], 1)
            self.assertEqual([entry["path"] for entry in second["cgroups"]], ["/session", "/"])
            self.assertEqual(len(second["loopback_tcp_sockets"]), 1)
            self.assertEqual(second["loopback_tcp_sockets"][0]["send_queue_bytes"], 16)
            self.assertEqual(second["loopback_tcp_sockets"][0]["receive_queue_bytes"], 32)
            self.assertEqual(second["unix_socket_states"], [{"inode": 123, "state": 3, "socket_type": 1}])
            self.assertNotIn("not-retained", json.dumps(second))
            self.assertIn("unix_socket_queue_bytes_not_exposed_by_proc", second["unavailable"])
            self.assertNotIn("name with", json.dumps(second))

    def test_missing_observations_are_not_zero_or_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = linux_resource_observation(123, proc_root=root, cgroup_root=root)
            self.assertNotIn("interval_cpu_percent", result)
            self.assertEqual(result["cgroups"], [])
            self.assertIn("process_cpu", result["unavailable"])
            self.assertIn("cgroup_v2_hierarchy", result["unavailable"])
            self.assertIn("owned_loopback_socket_queues", result["unavailable"])

    def test_cgroup_path_escape_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "proc/100").mkdir(parents=True)
            (root / "proc/100/cgroup").write_text("0::/../../outside")
            result = linux_resource_observation(100, proc_root=root / "proc", cgroup_root=root / "cgroup")
            self.assertEqual(result["cgroups"], [])
            self.assertIn("cgroup_v2_hierarchy", result["unavailable"])


if __name__ == "__main__":
    unittest.main()
