"""Take one partition out of an Android full-OTA package.

A full OTA holds `payload.bin`: a protobuf manifest naming each partition's
operations, followed by every operation's data. This reads that manifest far
enough to find one partition and writes it out as the image the bootloader
expects — the whole partition, which is what `skyward-pack --stock` wants.

    python tools/extract-partition.py package.zip recovery out/recovery.img

Only the operation types a full package uses are handled: REPLACE, REPLACE_BZ,
REPLACE_XZ and ZERO. An incremental package's operations (SOURCE_COPY and the
rest) need the partition they patch, and are refused by name rather than
guessed at.
"""

import sys
import zipfile

BLOCK = 4096

# InstallOperation.Type, as update_metadata.proto numbers them.
REPLACE, REPLACE_BZ, MOVE, BSDIFF, SOURCE_COPY, SOURCE_BSDIFF, ZERO, DISCARD, REPLACE_XZ = range(9)
TYPE_NAMES = {
    REPLACE: "REPLACE",
    REPLACE_BZ: "REPLACE_BZ",
    MOVE: "MOVE",
    BSDIFF: "BSDIFF",
    SOURCE_COPY: "SOURCE_COPY",
    SOURCE_BSDIFF: "SOURCE_BSDIFF",
    ZERO: "ZERO",
    DISCARD: "DISCARD",
    REPLACE_XZ: "REPLACE_XZ",
}


def varint(buf, at):
    """The number at `at`, and where it ends."""
    value, shift = 0, 0
    while True:
        if at >= len(buf):
            raise ValueError("a number runs past the end")
        byte = buf[at]
        at += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, at
        shift += 7


def fields(buf):
    """Every field of a protobuf message, as (number, wire type, value)."""
    at = 0
    while at < len(buf):
        key, at = varint(buf, at)
        number, wire = key >> 3, key & 7
        if wire == 0:
            value, at = varint(buf, at)
        elif wire == 2:
            length, at = varint(buf, at)
            value = buf[at : at + length]
            at += length
        elif wire == 5:
            value = buf[at : at + 4]
            at += 4
        elif wire == 1:
            value = buf[at : at + 8]
            at += 8
        else:
            raise ValueError(f"wire type {wire} in field {number}")
        yield number, wire, value


def one(buf, wanted):
    """The last `wanted` field of a message."""
    found = None
    for number, _, value in fields(buf):
        if number == wanted:
            found = value
    return found


def all_of(buf, wanted):
    return [value for number, _, value in fields(buf) if number == wanted]


def text(buf):
    return buf.decode("utf-8") if isinstance(buf, bytes) else str(buf)


def read_manifest(stream):
    """The manifest of a payload, and where its data starts."""
    head = stream.read(4)
    if head != b"CrAU":
        raise ValueError("not a payload: it does not begin with CrAU")
    version = int.from_bytes(stream.read(8), "big")
    size = int.from_bytes(stream.read(8), "big")
    if version != 2:
        raise ValueError(f"payload version {version}, which this does not read")
    manifest = stream.read(size)
    return manifest, 4 + 8 + 8 + size


def partitions(manifest):
    """Each partition's name, new size, and operations."""
    for number, _, update in fields(manifest):
        if number != 13:
            continue
        name = None
        size = 0
        operations = []
        for field, _, value in fields(update):
            if field == 1:
                name = text(value)
            elif field == 7:
                # PartitionInfo: size is field 1.
                size = one(value, 1) or 0
            elif field == 8:
                operations.append(operation(value))
        yield name, size, operations


def operation(buf):
    """One install operation: what it does, where its data is, where it goes."""
    kind, offset, length = None, 0, 0
    extents = []
    for field, _, value in fields(buf):
        if field == 1:
            kind = value
        elif field == 2:
            offset = value
        elif field == 3:
            length = value
        elif field == 6:
            # Extent: start_block 1, num_blocks 2.
            start = one(value, 1) or 0
            count = one(value, 2) or 0
            extents.append((start, count))
    return kind, offset, length, extents


def extract(package, wanted, out_path):
    with zipfile.ZipFile(package) as archive:
        name = None
        for entry in archive.namelist():
            if entry.endswith("payload.bin"):
                name = entry
                break
        if name is None:
            raise SystemExit(f"no payload.bin in {package}")
        with archive.open(name) as stream:
            manifest, data_at = read_manifest(stream)
            print(f"清单 {len(manifest)} 字节，数据从 {data_at} 开始")
            found = None
            for part, size, ops in partitions(manifest):
                if part == wanted:
                    found = (size, ops)
                    break
            if found is None:
                named = [p for p, _, _ in partitions(manifest)]
                raise SystemExit(f"没有 {wanted} 分区；包里有：{', '.join(n for n in named if n)}")
            size, ops = found
            print(f"{wanted}: {size} 字节，{len(ops)} 个操作")
            image = bytearray(size)
            for index, (kind, offset, length, extents) in enumerate(ops):
                what = TYPE_NAMES.get(kind, f"type {kind}")
                if kind == ZERO:
                    continue  # already zero
                if kind not in (REPLACE, REPLACE_BZ, REPLACE_XZ):
                    raise SystemExit(
                        f"操作 {index} 是 {what}：这是增量包，需要手机上的原分区才能合，本工具不做"
                    )
                stream.seek(data_at + offset)
                data = stream.read(length)
                if kind == REPLACE_BZ:
                    import bz2

                    data = bz2.decompress(data)
                elif kind == REPLACE_XZ:
                    import lzma

                    data = lzma.decompress(data)
                if len(data) != sum(count for _, count in extents) * BLOCK:
                    # A last operation may be short of a whole block; the
                    # manifest's size is what the partition is.
                    pass
                at = 0
                for start, count in extents:
                    want = count * BLOCK
                    chunk = data[at : at + want]
                    at += want
                    begin = start * BLOCK
                    end = min(begin + len(chunk), size)
                    image[begin:end] = chunk[: end - begin]
            with open(out_path, "wb") as out:
                out.write(image)
            print(f"写出 {out_path}（{size} 字节）")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("用法: extract-partition.py <包.zip> <分区名> <输出.img>")
    extract(sys.argv[1], sys.argv[2], sys.argv[3])
