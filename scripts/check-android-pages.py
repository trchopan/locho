#!/usr/bin/env python3
"""Check every ELF LOAD segment in an APK for Android 16 KiB alignment."""
from pathlib import Path
import struct
import sys
import zipfile

apk = Path(sys.argv[1])
count = 0
with zipfile.ZipFile(apk) as archive:
    for name in archive.namelist():
        if not name.startswith("lib/") or not name.endswith(".so"):
            continue
        data = archive.read(name)
        if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
            raise SystemExit(f"Unsupported ELF encoding: {name}")
        offset = struct.unpack_from("<Q", data, 32)[0]
        size, number = struct.unpack_from("<HH", data, 54)
        for index in range(number):
            values = struct.unpack_from("<IIQQQQQQ", data, offset + index * size)
            kind, _, file_offset, virtual_addr, _, _, _, alignment = values
            if kind == 1 and (alignment < 16384 or (virtual_addr - file_offset) % 16384 != 0):
                raise SystemExit(f"Non-16KiB-aligned LOAD segment: {name}")
        count += 1
if count == 0:
    raise SystemExit("No native libraries found")
print(f"16 KiB ELF LOAD alignment verified for {count} native libraries.")
