"""Read an AVB footer and the vbmeta header it points at.

The bootloader reads both before it starts anything: the footer says where the
image ends, and the vbmeta says who signed it, what it covers, and the
rollback index it may not fall below. Printing them side by side is how two
images that both look well-formed are told apart.
"""

import struct
import sys


def be(blob, at, size):
    return int.from_bytes(blob[at : at + size], "big")


def footer(path):
    with open(path, "rb") as f:
        f.seek(0, 2)
        end = f.tell()
        f.seek(end - 64)
        blob = f.read(64)
    if blob[0:4] != b"AVBf":
        return {"magic": blob[0:4], "error": "no AVBf footer"}
    return {
        "magic": "AVBf",
        "version": (be(blob, 4, 4), be(blob, 8, 4)),
        "original_image_size": be(blob, 12, 8),
        "vbmeta_offset": be(blob, 20, 8),
        "vbmeta_size": be(blob, 28, 8),
        "file_size": end,
    }


def vbmeta(path, at, size):
    with open(path, "rb") as f:
        f.seek(at)
        blob = f.read(size)
    if blob[0:4] != b"AVB0":
        return {"magic": blob[0:4], "error": "no AVB0 header"}
    return {
        "magic": "AVB0",
        "required_version": (be(blob, 4, 4), be(blob, 8, 4)),
        "auth_block": be(blob, 12, 8),
        "aux_block": be(blob, 20, 8),
        "algorithm": be(blob, 28, 4),
        "hash_size": be(blob, 40, 8),
        "signature_size": be(blob, 56, 8),
        "public_key_size": be(blob, 72, 8),
        "descriptors_offset": be(blob, 96, 8),
        "descriptors_size": be(blob, 104, 8),
        # The value the bootloader compares its own record against.
        "rollback_index": be(blob, 112, 8),
        "flags": be(blob, 120, 4),
        "rollback_index_location": be(blob, 124, 4),
        "release_string": blob[128:176].split(b"\x00")[0].decode("utf-8", "replace"),
    }


FLAGS = {0: "none", 1: "HASHTREE_DISABLED", 2: "VERIFICATION_DISABLED"}


def show(label, path):
    print(f"=== {label}")
    print(f"  {path}")
    f = footer(path)
    if "error" in f:
        print(f"  {f['error']}: {f['magic']!r}")
        return
    for key, value in f.items():
        print(f"  {key:22} {value}")
    v = vbmeta(path, f["vbmeta_offset"], f["vbmeta_size"])
    if "error" in v:
        print(f"  vbmeta: {v['error']}: {v['magic']!r}")
        return
    print("  --- vbmeta")
    for key, value in v.items():
        if key == "flags":
            value = f"{value} ({FLAGS.get(value, '?')})"
        print(f"  {key:22} {value}")
    print()


if __name__ == "__main__":
    if len(sys.argv) < 3:
        raise SystemExit("usage: avb-read.py <label> <image> [<label> <image> ...]")
    pairs = list(zip(sys.argv[1::2], sys.argv[2::2]))
    for label, path in pairs:
        show(label, path)
