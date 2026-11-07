#!/usr/bin/env python3
"""Turns Mozilla's trusted roots into the files justOS reads.

Source: tools/assets/src/cacert.pem, the bundle of the `certifi` package
(Mozilla's CA list, MPL-2.0). Outputs:

    rootfs/system/certs.bin               for justtls: "JCRT", u32 count, then
                                          per root u16 + subject (DER Name),
                                          u16 + SubjectPublicKeyInfo (DER)
    rootfs/etc/ssl/certs/ca-certificates.crt
                                          the PEM bundle, where Linux programs
                                          look for it

Only the standard library is needed: the DER is walked by hand.

Usage: tools/assets/gen_certs.py [rootfs folder]
"""
import base64
import os
import shutil
import struct
import sys

SRC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "src", "cacert.pem")


def tlv(b, i):
    """(tag, start of contents, end) of the TLV at i."""
    tag = b[i]
    l0 = b[i + 1]
    if l0 < 0x80:
        return tag, i + 2, i + 2 + l0
    n = l0 & 0x7F
    length = int.from_bytes(b[i + 2:i + 2 + n], "big")
    return tag, i + 2 + n, i + 2 + n + length


def children(b, start, end):
    out = []
    i = start
    while i < end:
        tag, s, e = tlv(b, i)
        out.append((tag, i, s, e))
        i = e
    return out


def subject_and_spki(der):
    _, s, e = tlv(der, 0)
    tbs = children(der, s, e)[0]
    fields = children(der, tbs[2], tbs[3])
    o = 1 if fields[0][0] == 0xA0 else 0
    subj = fields[o + 4]
    spki = fields[o + 5]
    return der[subj[1]:subj[3]], der[spki[1]:spki[3]]


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "rootfs"
    pem = open(SRC).read()
    ders = []
    b64 = None
    for line in pem.splitlines():
        if line.startswith("-----BEGIN CERTIFICATE"):
            b64 = []
        elif line.startswith("-----END CERTIFICATE"):
            ders.append(base64.b64decode("".join(b64)))
            b64 = None
        elif b64 is not None:
            b64.append(line.strip())
    out = bytearray(b"JCRT" + struct.pack(">I", len(ders)))
    for der in ders:
        subj, spki = subject_and_spki(der)
        out += struct.pack(">H", len(subj)) + subj + struct.pack(">H", len(spki)) + spki
    os.makedirs(os.path.join(root, "system"), exist_ok=True)
    with open(os.path.join(root, "system", "certs.bin"), "wb") as f:
        f.write(out)
    os.makedirs(os.path.join(root, "etc", "ssl", "certs"), exist_ok=True)
    shutil.copy(SRC, os.path.join(root, "etc", "ssl", "certs", "ca-certificates.crt"))
    print(f"{root}/system/certs.bin: {len(ders)} roots, {len(out)} bytes")


if __name__ == "__main__":
    main()
