#!/usr/bin/env python3
"""Cross-language PCF2 interoperability and shared fixtures verification harness."""

import argparse
import base64
import bz2
import hashlib
import json
import pathlib
import struct
import subprocess
import sys
import tempfile
import zlib


def require(cond: bool, msg: str) -> None:
    if not cond:
        raise RuntimeError(msg)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def run_cli(bin_path: pathlib.Path, args: list[str]) -> str:
    proc = subprocess.run(
        [str(bin_path), *args],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {bin_path.name} {' '.join(args)}\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
        )
    return proc.stdout.strip()


def write_varint(val: int, out: bytearray) -> None:
    while True:
        b = val & 0x7F
        val >>= 7
        if val:
            out.append(b | 0x80)
        else:
            out.append(b)
            break


def read_varint(buf: bytes, pos: int) -> tuple[int, int]:
    val = 0
    shift = 0
    while True:
        b = buf[pos]
        pos += 1
        val |= (b & 0x7F) << shift
        if (b & 0x80) == 0:
            return val, pos
        shift += 7


def parse_pcf2_segments(buf: bytes) -> list[tuple[int, bytes, bytes]]:
    require(buf[:4] == b"PCF2", f"bad PCF2 magic: {buf[:4]!r}")
    pos = 18
    segs = []
    while pos < len(buf):
        kind = buf[pos]
        _flags = buf[pos + 1]
        pos += 2
        _orig_len, pos = read_varint(buf, pos)
        meta_len, pos = read_varint(buf, pos)
        data_len, pos = read_varint(buf, pos)
        meta = buf[pos : pos + meta_len]
        pos += meta_len
        data = buf[pos : pos + data_len]
        pos += data_len
        segs.append((kind, meta, data))
    return segs


def build_nested_pcf2(inner_pcf: bytes, orig_size: int) -> bytes:
    """Wrap an existing PCF2 container inside a single SegmentKind::Nested (kind 6) container."""
    out = bytearray(b"PCF2")
    out.extend(struct.pack("<HHHQ", 2, 0, 0, orig_size))
    out.append(6)  # SegmentKind::Nested
    out.append(0)  # flags
    write_varint(orig_size, out)
    write_varint(1, out)  # meta_len = 1
    write_varint(len(inner_pcf), out)
    out.append(0)  # nested meta_version = 0
    out.extend(inner_pcf)
    return bytes(out)


def parse_base64_line_case(meta: bytes) -> int:
    require(len(meta) >= 3, f"base64 meta too short: {len(meta)}")
    require(meta[0] == 0, f"unexpected base64 meta_version: {meta[0]}")
    header_len, pos = read_varint(meta, 2)
    pos += header_len
    require(pos < len(meta), "base64 meta truncated before line_case")
    return meta[pos]


def make_png(
    width: int,
    height: int,
    color_type: int,
    pixels: bytes,
    extra_chunks: list[tuple[bytes, bytes]],
) -> bytes:
    bpp = 4 if color_type == 6 else 3
    row_bytes = width * bpp
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        raw.extend(pixels[y * row_bytes : (y + 1) * row_bytes])

    def chunk(ctype: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(ctype + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + ctype + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    out = bytearray(b"\x89PNG\r\n\x1a\n")
    out.extend(chunk(b"IHDR", ihdr))
    for ct, cd in extra_chunks:
        out.extend(chunk(ct, cd))
    out.extend(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
    out.extend(chunk(b"IEND", b""))
    return bytes(out)


def make_valid_iccp_chunk() -> bytes:
    profile = bytearray(132)
    profile[0:4] = struct.pack(">I", 132)
    profile[16:20] = b"RGB "
    profile[36:40] = b"acsp"
    return b"sRGB\x00\x00" + zlib.compress(bytes(profile), 6)


def make_structured_payload(seed: int, length: int) -> bytes:
    pat = b"UnityFS serialized asset data with repeated structure; "
    s = seed & 0xFFFFFFFFFFFFFFFF
    out = bytearray()
    while len(out) < length:
        s ^= (s << 13) & 0xFFFFFFFFFFFFFFFF
        s ^= (s >> 7) & 0xFFFFFFFFFFFFFFFF
        s ^= (s << 17) & 0xFFFFFFFFFFFFFFFF
        if s % 3 == 0:
            take = min(len(pat), length - len(out))
            out.extend(pat[:take])
        else:
            out.append((s >> 33) & 0xFF)
    return bytes(out)


def build_unityfs_bundle(
    rust_bin: pathlib.Path,
    tmp: pathlib.Path,
    variant: str,
    unity_ver: str,
    archive_ver: int,
    flags: int,
    payloads: list[bytes],
) -> bytes:
    def lz4_compress(raw: bytes) -> bytes:
        in_f = tmp / f"_u_in_{variant}.bin"
        out_f = tmp / f"_u_out_{variant}.bin"
        in_f.write_bytes(raw)
        run_cli(rust_bin, ["lz4hc", variant, str(in_f), str(out_f)])
        return out_f.read_bytes()

    stored = [lz4_compress(p) for p in payloads]
    info = bytearray(16)
    info.extend(struct.pack(">I", len(payloads)))
    for raw, comp in zip(payloads, stored):
        info.extend(struct.pack(">IIH", len(raw), len(comp), 3))
    info.extend(struct.pack(">I", 0))
    info_bytes = lz4_compress(bytes(info)) if (flags & 0x3F) != 0 else bytes(info)

    f = bytearray(b"UnityFS\x00")
    f.extend(struct.pack(">I", archive_ver))
    f.extend(b"5.x.x\x00")
    f.extend(unity_ver.encode("ascii") + b"\x00")
    size_off = len(f)
    f.extend(struct.pack(">Q", 0))
    f.extend(struct.pack(">III", len(info_bytes), len(info), flags))
    if archive_ver >= 7:
        while len(f) % 16 != 0:
            f.append(0)
    if (flags & 0x80) == 0:
        f.extend(info_bytes)
    if (flags & 0x200) != 0:
        while len(f) % 16 != 0:
            f.append(0)
    for st in stored:
        f.extend(st)
    if (flags & 0x80) != 0:
        f.extend(info_bytes)
    f[size_off : size_off + 8] = struct.pack(">Q", len(f))
    return bytes(f)


def make_mime_base64(payload: bytes, line_lens: list[int], with_header: bool) -> bytes:
    b64 = base64.b64encode(payload)
    out = bytearray()
    if with_header:
        out.extend(b"Content-Transfer-Encoding: base64\r\n\r\n")
    pos = 0
    idx = 0
    while pos < len(b64):
        step = line_lens[idx] if idx < len(line_lens) else line_lens[-1]
        out.extend(b64[pos : pos + step])
        out.extend(b"\r\n")
        pos += step
        idx += 1
    return bytes(out)


def check_cross_interop(
    name: str,
    data: bytes,
    rust_bin: pathlib.Path,
    pion_bin: pathlib.Path,
    tmp: pathlib.Path,
    expected_kinds: list[int],
    expected_b64_line_case: int | None = None,
    check_zero_bzip2_penalty: bool = False,
    expected_nested_child_kinds: list[int] | None = None,
    check_nested_zero_bzip2_penalty: bool = False,
    require_exact_pcf2: bool = True,
) -> dict:
    orig_sha = sha256_bytes(data)
    in_p = tmp / f"{name}.in"
    r_pcf = tmp / f"{name}.rust.pcf"
    p_pcf = tmp / f"{name}.pion.pcf"
    r2p_out = tmp / f"{name}.r2p.out"
    p2r_out = tmp / f"{name}.p2r.out"
    in_p.write_bytes(data)

    run_cli(rust_bin, ["encode", str(in_p), str(r_pcf)])
    run_cli(pion_bin, ["encode", str(in_p), str(p_pcf)])

    r_bytes = r_pcf.read_bytes()
    p_bytes = p_pcf.read_bytes()

    for lang, pcf_bytes in [("rust", r_bytes), ("pion", p_bytes)]:
        segs = parse_pcf2_segments(pcf_bytes)
        got_kinds = [s[0] for s in segs]
        require(
            got_kinds == expected_kinds,
            f"{name} ({lang}): expected segment kinds {expected_kinds}, got {got_kinds}",
        )
        if expected_b64_line_case is not None:
            lc = parse_base64_line_case(segs[0][1])
            require(
                lc == expected_b64_line_case,
                f"{name} ({lang}): expected base64 line_case={expected_b64_line_case}, got {lc}",
            )
        if check_zero_bzip2_penalty:
            penalty_count, _ = read_varint(segs[0][1], 3)
            require(penalty_count == 0, f"{name} ({lang}): expected 0 penalty, got {penalty_count}")
        if expected_nested_child_kinds is not None:
            require(
                len(segs[0][1]) >= 2 and segs[0][1][1] == 1,
                f"{name} ({lang}): expected nested payload_kind=1 in meta[1], got {segs[0][1][:2]!r}",
            )
            child_segs = parse_pcf2_segments(segs[0][2])
            child_kinds = [s[0] for s in child_segs]
            require(
                child_kinds == expected_nested_child_kinds,
                f"{name} ({lang}): expected nested child kinds {expected_nested_child_kinds}, got {child_kinds}",
            )
            if check_nested_zero_bzip2_penalty:
                child_pen, _ = read_varint(child_segs[0][1], 3)
                require(
                    child_pen == 0,
                    f"{name} ({lang}): expected 0 penalty in nested bzip2 child, got {child_pen}",
                )

    if require_exact_pcf2:
        require(
            sha256_bytes(r_bytes) == sha256_bytes(p_bytes),
            f"{name}: Rust and Pion PCF2 wire bytes differ ({len(r_bytes)}B vs {len(p_bytes)}B)",
        )

    run_cli(pion_bin, ["decode", str(r_pcf), str(r2p_out)])
    run_cli(rust_bin, ["decode", str(p_pcf), str(p2r_out)])

    require(sha256_bytes(r2p_out.read_bytes()) == orig_sha, f"{name}: Rust->Pion SHA-256 mismatch!")
    require(sha256_bytes(p2r_out.read_bytes()) == orig_sha, f"{name}: Pion->Rust SHA-256 mismatch!")

    return {
        "name": name,
        "orig_bytes": len(data),
        "rust_pcf_bytes": len(r_bytes),
        "pion_pcf_bytes": len(p_bytes),
        "segment_kinds": expected_kinds,
        "pcf2_byte_exact": sha256_bytes(r_bytes) == sha256_bytes(p_bytes),
        "rust_to_pion": "PASS",
        "pion_to_rust": "PASS",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Verify Rust <-> Pion PCF2 interop and shared fixtures.")
    parser.add_argument("--rust-bin", required=True, type=pathlib.Path)
    parser.add_argument("--pion-bin", required=True, type=pathlib.Path)
    parser.add_argument("--fixtures-dir", required=True, type=pathlib.Path)
    parser.add_argument("--json-out", type=pathlib.Path, default=None)
    args = parser.parse_args()

    results = {
        "cross_interop": [],
        "png_webp_display_interop": [],
        "lz4hc_byte_exact": [],
        "fixtures_roundtrip": [],
        "legacy_v1_pcf": [],
    }

    with tempfile.TemporaryDirectory(prefix="precomp2_interop_") as td:
        tmp = pathlib.Path(td)

        # 1. Cross-language PCF2 interop: Lit, Nested, Base64 (line cases 0/1/2 + nested), Bzip2, UnityFS
        print("=== 1. Cross-Language PCF2 Interop (Rust <-> Pion) ===")
        lit_data = b"Plain literal payload with no compressible header 0123456789!\n" * 16
        b64_payload = b"Base64 cross-language interop payload for PCF2 verification."  # 60B -> 80 base64 chars
        b64_case0 = make_mime_base64(b64_payload, [16], True)  # 5 x 16 chars -> line_case 0
        b64_case1 = make_mime_base64(b64_payload, [24], True)  # 24, 24, 24, 8 -> line_case 1
        b64_case2 = make_mime_base64(b64_payload, [10, 14, 12, 18, 26], True)  # irregular -> line_case 2
        b64_nested_bz2 = make_mime_base64(bz2.compress(b"nested bzip2 inside base64! " * 64, 6), [64], True)

        bz2_small = b"bzip2 interop payload data! " * 128
        bz2_l1 = bz2.compress(bz2_small, 1)
        bz2_l6 = bz2.compress(bz2_small, 6)
        bz2_l9 = bz2.compress(bz2_small, 9)
        bz2_multi_plain = bytes(((i * 29 + (i // 251) * 17 + 7) & 0xFF) for i in range(130000))
        bz2_multi_l1 = bz2.compress(bz2_multi_plain, 1)

        interop_cases = [
            ("lit", lit_data, [0], None, False, None, False),
            ("base64_case0_uniform", b64_case0, [7], 0, False, None, False),
            ("base64_case1_last_short", b64_case1, [7], 1, False, None, False),
            ("base64_case2_irregular", b64_case2, [7], 2, False, None, False),
            ("base64_nested_bzip2", b64_nested_bz2, [7], 1, False, [8], True),
            ("bzip2_L1_single", bz2_l1, [8], None, True, None, False),
            ("bzip2_L6_single", bz2_l6, [8], None, True, None, False),
            ("bzip2_L9_single", bz2_l9, [8], None, True, None, False),
            ("bzip2_L1_multiblock", bz2_multi_l1, [8], None, True, None, False),
        ]

        u_payloads = [make_structured_payload(100 + i, 16384) for i in range(3)]
        for variant, uver, aver, uflags, expected_u_kinds in [
            ("v1_10_0_l12", "2022.3.62f3", 8, 0x243, [0, 11, 0, 11, 11, 11]),
            ("v1_9_1_l12", "2021.3.35f1", 8, 0x243, [0, 11, 0, 11, 11, 11]),
            ("r131_l16", "2017.4.39f1", 6, 0x43, [0, 11, 11, 11, 11]),
        ]:
            ubundle = build_unityfs_bundle(args.rust_bin, tmp, variant, uver, aver, uflags, u_payloads)
            interop_cases.append((f"unityfs_{variant}", ubundle, expected_u_kinds, None, False, None, False))

        for name, data, exp_kinds, exp_lc, check_pen, child_kinds, child_bz2_pen in interop_cases:
            r = check_cross_interop(
                name,
                data,
                args.rust_bin,
                args.pion_bin,
                tmp,
                expected_kinds=exp_kinds,
                expected_b64_line_case=exp_lc,
                check_zero_bzip2_penalty=check_pen,
                expected_nested_child_kinds=child_kinds,
                check_nested_zero_bzip2_penalty=child_bz2_pen,
                require_exact_pcf2=True,
            )
            results["cross_interop"].append(r)
            print(
                f"  [PASS] {name}: orig={r['orig_bytes']}B pcf={r['rust_pcf_bytes']}B "
                f"kinds={r['segment_kinds']} exact={r['pcf2_byte_exact']}"
            )

        # Explicit SegmentKind::Nested (kind 6) bidirectional decode verification
        nested_orig = bz2_l6
        nested_in = tmp / "nested_inner.in"
        nested_r_inner = tmp / "nested_inner.rust.pcf"
        nested_p_inner = tmp / "nested_inner.pion.pcf"
        nested_in.write_bytes(nested_orig)
        run_cli(args.rust_bin, ["encode", str(nested_in), str(nested_r_inner)])
        run_cli(args.pion_bin, ["encode", str(nested_in), str(nested_p_inner)])
        r_nested_pcf = build_nested_pcf2(nested_r_inner.read_bytes(), len(nested_orig))
        p_nested_pcf = build_nested_pcf2(nested_p_inner.read_bytes(), len(nested_orig))
        require(sha256_bytes(r_nested_pcf) == sha256_bytes(p_nested_pcf), "nested PCF2 wire mismatch")
        require([s[0] for s in parse_pcf2_segments(r_nested_pcf)] == [6], "expected Nested segment kind 6")
        r_nested_path = tmp / "nested.rust.pcf"
        p_nested_path = tmp / "nested.pion.pcf"
        r2p_nested = tmp / "nested.r2p.out"
        p2r_nested = tmp / "nested.p2r.out"
        r_nested_path.write_bytes(r_nested_pcf)
        p_nested_path.write_bytes(p_nested_pcf)
        run_cli(args.pion_bin, ["decode", str(r_nested_path), str(r2p_nested)])
        run_cli(args.rust_bin, ["decode", str(p_nested_path), str(p2r_nested)])
        require(sha256_bytes(r2p_nested.read_bytes()) == sha256_bytes(nested_orig), "nested R->P mismatch")
        require(sha256_bytes(p2r_nested.read_bytes()) == sha256_bytes(nested_orig), "nested P->R mismatch")
        nested_entry = {
            "name": "nested_bzip2_segment",
            "orig_bytes": len(nested_orig),
            "rust_pcf_bytes": len(r_nested_pcf),
            "pion_pcf_bytes": len(p_nested_pcf),
            "segment_kinds": [6],
            "pcf2_byte_exact": True,
            "rust_to_pion": "PASS",
            "pion_to_rust": "PASS",
        }
        results["cross_interop"].append(nested_entry)
        print(f"  [PASS] nested_bzip2_segment: orig={len(nested_orig)}B pcf={len(r_nested_pcf)}B kinds=[6] exact=True")

        # 2. LZ4HC byte-exact cross-check
        print("=== 2. LZ4HC Byte-Exact Output Match (Rust vs Pion) ===")
        lz4_sample = make_structured_payload(0xDEADBEEF, 262144)
        lz4_in = tmp / "lz4_sample.bin"
        lz4_in.write_bytes(lz4_sample)
        for variant in ["v1_10_0_l12", "v1_9_1_l12", "r131_l16"]:
            r_out = tmp / f"lz4_{variant}.rust"
            p_out = tmp / f"lz4_{variant}.pion"
            run_cli(args.rust_bin, ["lz4hc", variant, str(lz4_in), str(r_out)])
            run_cli(args.pion_bin, ["lz4hc", variant, str(lz4_in), str(p_out)])
            r_bytes = r_out.read_bytes()
            p_bytes = p_out.read_bytes()
            require(sha256_bytes(r_bytes) == sha256_bytes(p_bytes), f"LZ4HC {variant} mismatch!")
            results["lz4hc_byte_exact"].append(
                {
                    "variant": variant,
                    "in_bytes": len(lz4_sample),
                    "out_bytes": len(r_bytes),
                    "sha256": sha256_bytes(r_bytes),
                }
            )
            print(f"  [PASS] {variant}: {len(lz4_sample)}B -> {len(r_bytes)}B sha256={sha256_bytes(r_bytes)[:16]}...")

        # 3. PNG + WebP Lossless: [Lit, PngIdat, Lit] + same-lang roundtrip + cross-lang display + pixel SHA-256
        print("=== 3. PNG + WebP Lossless Roundtrip & Cross-Language Display Interop ===")
        rgb_pix = bytes(((i * 53 + 17) & 0xFF) for i in range(64 * 64 * 3))
        rgba_pix = bytearray(64 * 64 * 4)
        for p in range(64 * 64):
            rgba_pix[p * 4] = (p * 37 + 11) & 0xFF
            rgba_pix[p * 4 + 1] = (p * 73 + 29) & 0xFF
            rgba_pix[p * 4 + 2] = (p * 113 + 43) & 0xFF
            rgba_pix[p * 4 + 3] = 0 if (p % 3 == 0) else ((p * 19 + 80) & 0xFF)
        rgba_bytes = bytes(rgba_pix)
        png_cases = [
            ("png_rgb_64x64", make_png(64, 64, 2, rgb_pix, []), rgb_pix),
            ("png_rgba_alpha0_64x64", make_png(64, 64, 6, rgba_bytes, []), rgba_bytes),
            (
                "png_rgba_iccp_64x64",
                make_png(64, 64, 6, rgba_bytes, [(b"iCCP", make_valid_iccp_chunk())]),
                rgba_bytes,
            ),
        ]
        for name, png_data, expected_pixels in png_cases:
            orig_sha = sha256_bytes(png_data)
            expected_pix_sha = sha256_bytes(expected_pixels)
            in_p = tmp / f"{name}.png"
            r_pcf = tmp / f"{name}.rust.pcf"
            p_pcf = tmp / f"{name}.pion.pcf"
            r_dec = tmp / f"{name}.rust.dec"
            p_dec = tmp / f"{name}.pion.dec"
            r2r_disp = tmp / f"{name}.r2r.webp"
            r2p_disp = tmp / f"{name}.r2p.webp"
            p2p_disp = tmp / f"{name}.p2p.webp"
            p2r_disp = tmp / f"{name}.p2r.webp"
            in_p.write_bytes(png_data)

            run_cli(args.rust_bin, ["encode", str(in_p), str(r_pcf), "--png-webp"])
            run_cli(args.pion_bin, ["encode", str(in_p), str(p_pcf), "--png-webp"])

            for lang, pcf_path in [("rust", r_pcf), ("pion", p_pcf)]:
                segs = parse_pcf2_segments(pcf_path.read_bytes())
                kinds = [s[0] for s in segs]
                require(kinds == [0, 4, 0], f"{name} ({lang}): expected [0, 4, 0], got {kinds}")
                require(segs[1][1][2] == 1, f"{name} ({lang}): expected png_payload_kind=1 (WebP)")

            run_cli(args.rust_bin, ["decode", str(r_pcf), str(r_dec)])
            run_cli(args.pion_bin, ["decode", str(p_pcf), str(p_dec)])
            require(sha256_bytes(r_dec.read_bytes()) == orig_sha, f"{name} Rust roundtrip mismatch")
            require(sha256_bytes(p_dec.read_bytes()) == orig_sha, f"{name} Pion roundtrip mismatch")

            # Cross-language decode_image_for_display byte-exact consistency
            run_cli(args.rust_bin, ["display", str(r_pcf), str(r2r_disp)])
            run_cli(args.pion_bin, ["display", str(r_pcf), str(r2p_disp)])
            run_cli(args.pion_bin, ["display", str(p_pcf), str(p2p_disp)])
            run_cli(args.rust_bin, ["display", str(p_pcf), str(p2r_disp)])

            r2r_bytes = r2r_disp.read_bytes()
            r2p_bytes = r2p_disp.read_bytes()
            p2p_bytes = p2p_disp.read_bytes()
            p2r_bytes = p2r_disp.read_bytes()
            require(r2p_bytes[:4] == b"RIFF" and r2p_bytes[8:12] == b"WEBP", f"{name} R->P display not WEBP")
            require(p2r_bytes[:4] == b"RIFF" and p2r_bytes[8:12] == b"WEBP", f"{name} P->R display not WEBP")
            require(
                sha256_bytes(r2r_bytes) == sha256_bytes(r2p_bytes),
                f"{name}: Rust->Rust display differs from Rust->Pion display!",
            )
            require(
                sha256_bytes(p2p_bytes) == sha256_bytes(p2r_bytes),
                f"{name}: Pion->Pion display differs from Pion->Rust display!",
            )

            # Pixel-level RGBA/RGB SHA-256 verification (including RGB under alpha==0!)
            for tag, webp_path in [("r2p", r2p_disp), ("p2r", p2r_disp)]:
                pix_out = tmp / f"{name}.{tag}.raw"
                run_cli(args.rust_bin, ["webp-decode", str(webp_path), str(pix_out)])
                got_pix = pix_out.read_bytes()
                require(
                    sha256_bytes(got_pix) == expected_pix_sha,
                    f"{name} ({tag}): decoded display WebP pixels SHA-256 mismatch!",
                )

            results["png_webp_display_interop"].append(
                {
                    "name": name,
                    "orig_bytes": len(png_data),
                    "rust_pcf_bytes": r_pcf.stat().st_size,
                    "pion_pcf_bytes": p_pcf.stat().st_size,
                    "r2p_webp_bytes": len(r2p_bytes),
                    "p2r_webp_bytes": len(p2r_bytes),
                    "display_byte_exact": True,
                    "pixels_sha256_verified": True,
                }
            )
            print(
                f"  [PASS] {name}: orig={len(png_data)}B rust_pcf={r_pcf.stat().st_size}B "
                f"pion_pcf={p_pcf.stat().st_size}B R->P_disp={len(r2p_bytes)}B P->R_disp={len(p2r_bytes)}B (pixels verified)"
            )

        # 4. Full Shared fixtures/ Roundtrip in BOTH Rust and Pion with SegmentKind assertions
        print("=== 4. Shared fixtures/ Roundtrip (Rust & Pion) ===")
        fixture_specs = [
            ("minimal.jpg", 5),  # Jpg
            ("minimal.png", 4),  # PngIdat
            ("pdf-v1-0.pdf", 10),  # PdfStream
            ("pdf-v1-1.pdf", 10),
            ("pdf-v1-2.pdf", 10),
            ("qpdf/filter-abbreviation.pdf", 10),
            ("qpdf/jpeg-qstream.pdf", 10),
            ("qpdf/image-streams-small.pdf", 10),
            ("qpdf/image-streams.pdf", 10),
        ]
        for rel, required_kind in fixture_specs:
            fpath = args.fixtures_dir / rel
            orig = fpath.read_bytes()
            orig_sha = sha256_bytes(orig)
            tag = rel.replace("/", "_")
            r_pcf = tmp / f"fix_{tag}.rust.pcf"
            p_pcf = tmp / f"fix_{tag}.pion.pcf"
            r_dec = tmp / f"fix_{tag}.rust.dec"
            p_dec = tmp / f"fix_{tag}.pion.dec"

            run_cli(args.rust_bin, ["encode", str(fpath), str(r_pcf)])
            run_cli(args.rust_bin, ["decode", str(r_pcf), str(r_dec)])
            require(sha256_bytes(r_dec.read_bytes()) == orig_sha, f"fixture {rel} Rust roundtrip mismatch")

            run_cli(args.pion_bin, ["encode", str(fpath), str(p_pcf)])
            run_cli(args.pion_bin, ["decode", str(p_pcf), str(p_dec)])
            require(sha256_bytes(p_dec.read_bytes()) == orig_sha, f"fixture {rel} Pion roundtrip mismatch")

            r_segs = parse_pcf2_segments(r_pcf.read_bytes())
            p_segs = parse_pcf2_segments(p_pcf.read_bytes())
            r_count = sum(1 for s in r_segs if s[0] == required_kind)
            p_count = sum(1 for s in p_segs if s[0] == required_kind)
            require(r_count >= 1, f"fixture {rel} Rust missing expected segment kind {required_kind}")
            require(p_count >= 1, f"fixture {rel} Pion missing expected segment kind {required_kind}")

            entry = {
                "fixture": rel,
                "orig_bytes": len(orig),
                "rust_pcf_bytes": r_pcf.stat().st_size,
                "pion_pcf_bytes": p_pcf.stat().st_size,
                "required_kind": required_kind,
                "rust_transformed_segs": r_count,
                "pion_transformed_segs": p_count,
            }
            results["fixtures_roundtrip"].append(entry)
            print(
                f"  [PASS] {rel}: orig={len(orig)}B rust_pcf={entry['rust_pcf_bytes']}B (segs={r_count}) "
                f"pion_pcf={entry['pion_pcf_bytes']}B (segs={p_count})"
            )

        # 5. Legacy v1 fixtures/pdf-v1-{0,1,2}.pcf decoded by BOTH Rust and Pion
        print("=== 5. Legacy v1 PCF2 Fixtures (fixtures/pdf-v1-{0,1,2}.pcf) ===")
        for idx in range(3):
            pcf_path = args.fixtures_dir / f"pdf-v1-{idx}.pcf"
            pdf_path = args.fixtures_dir / f"pdf-v1-{idx}.pdf"
            expected_sha = sha256_bytes(pdf_path.read_bytes())
            r_out = tmp / f"v1_{idx}.rust.pdf"
            p_out = tmp / f"v1_{idx}.pion.pdf"
            run_cli(args.rust_bin, ["decode", str(pcf_path), str(r_out)])
            run_cli(args.pion_bin, ["decode", str(pcf_path), str(p_out)])
            require(sha256_bytes(r_out.read_bytes()) == expected_sha, f"pdf-v1-{idx}.pcf Rust decode mismatch")
            require(sha256_bytes(p_out.read_bytes()) == expected_sha, f"pdf-v1-{idx}.pcf Pion decode mismatch")
            segs = parse_pcf2_segments(pcf_path.read_bytes())
            seg_kinds = [s[0] for s in segs]
            require(seg_kinds == [0, 0, 0], f"pdf-v1-{idx}.pcf unexpected kinds {seg_kinds}")
            results["legacy_v1_pcf"].append(
                {"fixture": f"pdf-v1-{idx}.pcf", "segment_kinds": seg_kinds, "rust": "PASS", "pion": "PASS"}
            )
            print(f"  [PASS] pdf-v1-{idx}.pcf: segment_kinds={seg_kinds} Rust=PASS Pion=PASS")

    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(results, indent=2) + "\n")

    print("\nALL INTEROP & FIXTURE CHECKS PASSED!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
