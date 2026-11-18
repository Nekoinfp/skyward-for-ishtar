"""Audit a packed recovery image: what a bootloader reads before anything runs.

    python tools/audit-image.py skyward-....img

It checks the boot image header, that the ramdisk unpacks to a newc archive
holding what a recovery needs, that the recovery binary is AArch64, and that
the AVB footer names the image and its digest is the salted hash of it. Every
check is something a bootloader does before any of Skyward runs.

The descriptor layout it expects is the one this phone's own recovery uses -
payload at 132, the name's length at 56 - which `crates/skyward-pack/src/avb.rs`
pins with a test.
"""
import hashlib
import struct
import sys

PAGE = 4096
issues = []
notes = []


def check(ok, said):
    (notes if ok else issues).append(said)


def be(b, at, n=8):
    raw = b[at:at + n]
    return int.from_bytes(raw, "big")


def main(path):
    data = open(path, "rb").read()
    print("==", path)
    print("   %d bytes" % len(data))

    # --- the boot image header -------------------------------------------
    check(data[:8] == b"ANDROID!", "the image starts with ANDROID!")
    kernel, ramdisk = struct.unpack_from("<II", data, 8)
    header_size = struct.unpack_from("<I", data, 20)[0]
    header_version = struct.unpack_from("<I", data, 40)[0]
    print("   kernel %d, ramdisk %d, header %d, version %d" % (kernel, ramdisk, header_size, header_version))
    check(header_version == 4, "header version 4, as the stock recovery has")
    check(header_size == 1584, "header size 1584, as version 4 lays it")
    check(kernel == 0, "no kernel, as the stock recovery of this phone has none")
    check(ramdisk > 0, "a ramdisk is there")

    # --- where the ramdisk lies ------------------------------------------
    offset = PAGE + (kernel + PAGE - 1) // PAGE * PAGE
    # A boot image pads its ramdisk to the page, and the footer counts the
    # padded image, not the length the header states.
    image_len = offset + (ramdisk + PAGE - 1) // PAGE * PAGE
    check(image_len <= len(data), "the ramdisk %d..%d is inside the image" % (offset, image_len))
    check(len(data) - image_len >= PAGE, "room for a vbmeta image and its footer after it")

    # --- the ramdisk is an LZ4 legacy stream of a newc archive -----------
    rd = data[offset:image_len]
    check(rd[:4] == bytes.fromhex("02214c18"), "the ramdisk opens with the LZ4 legacy magic")
    flat = lz4_legacy(rd[4:])
    entries = list(cpio_entries(flat))
    check(bool(entries), "the ramdisk unpacks to %d newc entries" % len(entries))
    check(flat.rstrip(b"\0").endswith(b"TRAILER!!!"), "the archive ends with its trailer")
    table = dict(entries)

    # --- what a recovery needs in it -------------------------------------
    for wanted in ["system/bin/recovery", "system/bin/skyward", "skyward/device.toml", "system/bin/se_omapi"]:
        check(wanted in table, "%s is in the ramdisk" % wanted)
    if "system/bin/recovery" in table:
        elf = table["system/bin/recovery"]
        check(elf[:4] == b"\x7fELF", "the recovery binary is an ELF file")
        check(elf[4] == 2, "it is 64-bit")
        machine = int.from_bytes(elf[18:20], "little")
        check(machine == 0xB7, "it is built for AArch64 (e_machine %#x)" % machine)
    if "system/bin/skyward" in table:
        check(table["system/bin/skyward"] == b"recovery", "system/bin/skyward points at recovery")

    # --- the footer a bootloader verifies --------------------------------
    footer = data[-64:]
    check(footer[:4] == b"AVBf", "the partition ends with an AVB footer")
    vbmeta_offset = be(footer, 20)
    vbmeta_size = be(footer, 28)
    original = be(footer, 12)
    check(original == image_len, "the footer names the image's own length (%d)" % original)
    v = data[vbmeta_offset:vbmeta_offset + vbmeta_size]
    check(v[:4] == b"AVB0", "the footer points at a vbmeta image")
    aux = 256 + be(v, 12)
    start = aux + be(v, 96)
    end = start + be(v, 104)
    descriptors = v[start:end]
    check(bool(descriptors), "the vbmeta image holds %d bytes of descriptors" % len(descriptors))

    at = 0
    seen = []
    while at + 16 <= len(descriptors):
        tag = be(descriptors, at)
        length = 16 + be(descriptors, at + 8)
        one = descriptors[at:at + length]
        check(len(one) == length, "descriptor %d at %d is whole" % (tag, at))
        seen.append(tag)
        if tag == 2:  # the hash descriptor
            image_size = be(one, 16)
            # This phone's own recovery puts the payload right after a
            # 32-byte algorithm field and 60 reserved bytes, and so does the
            # packer: the two are byte for byte the same shape.
            algorithm = one[24:56].rstrip(b"\0").decode("ascii", "replace")
            name_len = be(one, 56, 4)
            salt_len = be(one, 60, 4)
            digest_len = be(one, 64, 4)
            body = one[132:]
            check(len(one) - len(body) + name_len + salt_len + digest_len <= len(one),
                  "the descriptor holds the name, salt and digest it declares")
            check(len(one) - (132 + name_len + salt_len + digest_len) in range(0, 8),
                  "and nothing else but padding")
            name = body[:name_len].decode("ascii", "replace")
            salt = body[name_len:name_len + salt_len]
            digest = body[name_len + salt_len:name_len + salt_len + digest_len]
            check(image_size == image_len, "the hash descriptor covers the image (%d)" % image_size)
            check(name.rstrip("\0") == "recovery", "it names the recovery partition (%r)" % name)
            check(algorithm == "sha256", "it hashes with %r" % algorithm)
            want = hashlib.sha256(salt + data[:image_len]).digest()
            check(digest == want, "the digest is sha256(salt + the image)")
            print("   digest %s" % digest.hex())
        at += length
    check(2 in seen, "a hash descriptor is there")
    check(0 in seen, "a property descriptor is there")

    print()
    for said in notes:
        print("   ok   " + said)
    for said in issues:
        print("   FAIL " + said)
    print("\n%d checks passed, %d failed" % (len(notes), len(issues)))
    return 1 if issues else 0


def lz4_block(src, out):
    at, n = 0, len(src)
    while at < n:
        token = src[at]
        at += 1
        literals = token >> 4
        if literals == 15:
            while True:
                b = src[at]
                at += 1
                literals += b
                if b != 255:
                    break
        out += src[at:at + literals]
        at += literals
        if at >= n:
            return
        offset = src[at] | (src[at + 1] << 8)
        at += 2
        length = token & 15
        if length == 15:
            while True:
                b = src[at]
                at += 1
                length += b
                if b != 255:
                    break
        length += 4
        start = len(out) - offset
        if start < 0:
            raise ValueError("offset before the start")
        for i in range(length):
            out.append(out[start + i])


def lz4_legacy(src):
    out, at = bytearray(), 0
    while at + 4 <= len(src):
        size = struct.unpack_from("<I", src, at)[0]
        at += 4
        if size == 0:
            break
        stored = bool(size & 0x80000000)
        size &= 0x7FFFFFFF
        block = src[at:at + size]
        if len(block) != size:
            raise ValueError("short LZ4 block")
        at += size
        if stored:
            out += block
        else:
            lz4_block(block, out)
    return bytes(out)


def cpio_entries(data):
    at = 0
    while at + 110 <= len(data):
        header = data[at:at + 110]
        if header[:6] != b"070701":
            raise ValueError("not a newc archive at %d" % at)
        fields = [int(header[6 + i * 8:14 + i * 8], 16) for i in range(13)]
        namesize, filesize = fields[11], fields[6]
        name_at = at + 110
        name = data[name_at:name_at + namesize - 1].decode("utf-8", "replace")
        body_at = (name_at + namesize + 3) & ~3
        body = data[body_at:body_at + filesize]
        next_at = (body_at + filesize + 3) & ~3
        if name == "TRAILER!!!":
            return
        yield name, body
        at = next_at


sys.exit(main(sys.argv[1]))
