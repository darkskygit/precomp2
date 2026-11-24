#!/usr/bin/env python3
"""Head-to-head Rust (--release) vs Pion (--release) benchmark suite for precomp2."""

import argparse
import base64
import bz2
import hashlib
import json
import math
import os
import pathlib
import platform
import re
import shutil
import statistics
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


def parse_pcf2_kinds(buf: bytes) -> list[int]:
    require(buf[:4] == b"PCF2", f"bad PCF2 magic: {buf[:4]!r}")
    pos = 18
    kinds: list[int] = []
    while pos < len(buf):
        kind = buf[pos]
        pos += 2
        _ol, pos = read_varint(buf, pos)
        ml, pos = read_varint(buf, pos)
        dl, pos = read_varint(buf, pos)
        pos += ml + dl
        kinds.append(kind)
    return kinds


def run_measured(
    bin_path: pathlib.Path,
    args: list[str],
    taskset_cpu: int | None = None,
) -> tuple[float, int, float | None]:
    """Run CLI binary without pipe deadlock and return (per_iter_ms, out_bytes, peak_rss_mib_or_none)."""
    cmd = [str(bin_path), *args]
    if taskset_cpu is not None:
        cmd = ["taskset", "-c", str(taskset_cpu), *cmd]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
        )
    m = re.search(
        r"elapsed_us=(\d+)\s+iters=(\d+)\s+out_bytes=(\d+)(?:\s+vmhwm_kib=(\d+))?",
        proc.stdout,
    )
    if not m:
        raise RuntimeError(f"unexpected CLI output: {proc.stdout!r}")
    elapsed_us = int(m.group(1))
    iters = int(m.group(2))
    out_bytes = int(m.group(3))
    vmhwm_kib = int(m.group(4)) if m.group(4) else 0
    per_iter_ms = (elapsed_us / iters) / 1000.0
    rss_mib = round(vmhwm_kib / 1024.0, 2) if vmhwm_kib > 0 else None
    return per_iter_ms, out_bytes, rss_mib


def run_simple(bin_path: pathlib.Path, args: list[str]) -> str:
    proc = subprocess.run([str(bin_path), *args], capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {bin_path.name} {' '.join(args)}\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
        )
    return proc.stdout.strip()


def percentile(values: list[float], p: float) -> float:
    s = sorted(values)
    if len(s) == 1:
        return s[0]
    k = (len(s) - 1) * (p / 100.0)
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return s[int(k)]
    return s[f] + (s[c] - s[f]) * (k - f)


def summarize(
    times_ms: list[float],
    rss_mibs: list[float | None],
    input_bytes: int,
    out_bytes: int,
) -> dict:
    med = statistics.median(times_ms)
    mn = min(times_ms)
    p95 = round(percentile(times_ms, 95.0), 3) if len(times_ms) >= 10 else None
    mib_s = (input_bytes / (1024.0 * 1024.0)) / (med / 1000.0) if med > 0 else 0.0
    valid_rss = [r for r in rss_mibs if r is not None]
    peak_rss = round(max(valid_rss), 2) if valid_rss else None
    return {
        "rounds": len(times_ms),
        "median_ms": round(med, 3),
        "min_ms": round(mn, 3),
        "p95_ms": p95,
        "throughput_mibs": round(mib_s, 2),
        "peak_rss_mib": peak_rss,
        "out_bytes": out_bytes,
    }


def make_structured_payload(seed: int, length: int) -> bytes:
    pat = b"precomp2 structured benchmark payload with repeated phrases and metadata; "
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


def make_text_payload(length: int) -> bytes:
    lines = [
        b"2026-11-24T00:00:00Z INFO  precomp2::bench stream_id=0042 bytes=16384 status=ok\n",
        b"2026-11-24T00:00:01Z DEBUG precomp2::codec segment=Zlib orig_len=65536 ratio=0.412\n",
        b"The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs.\n",
        b"{\"id\":1024,\"codec\":\"preflate\",\"lossless\":true,\"tags\":[\"pcf2\",\"bench\",\"pion\"]}\n",
    ]
    out = bytearray()
    i = 0
    while len(out) < length:
        line = lines[i % len(lines)]
        tag = f"[{i:06d}] ".encode("ascii")
        out.extend(tag)
        out.extend(line)
        i += 1
    return bytes(out[:length])


def make_gzip(plain: bytes, level: int = 6) -> bytes:
    comp = zlib.compressobj(level, zlib.DEFLATED, -15)
    raw_deflate = comp.compress(plain) + comp.flush()
    header = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff"
    footer = struct.pack("<II", zlib.crc32(plain) & 0xFFFFFFFF, len(plain) & 0xFFFFFFFF)
    return header + raw_deflate + footer


def make_png_rgba(width: int, height: int) -> tuple[bytes, bytes]:
    """Return (png_bytes, raw_rgba_pixels) with deterministic alpha==0 non-zero RGB pixels."""
    rgba = bytearray()
    raw = bytearray()
    for y in range(height):
        raw.append(1 if (y % 2 == 1) else 0)  # alternate None and Sub filters
        prev = [0, 0, 0, 0]
        for x in range(width):
            r = (x * 3 + y * 5) & 0xFF
            g = (x * 7 + y * 2 + 40) & 0xFF
            b = ((x ^ y) * 5 + 90) & 0xFF
            a = 0 if ((x + y) % 17 == 0) else (255 - ((x + y) & 0x3F))
            px = [r, g, b, a]
            rgba.extend(px)
            if y % 2 == 1:
                for c in range(4):
                    raw.append((px[c] - prev[c]) & 0xFF)
                prev = px
            else:
                raw.extend(px)

    def chunk(ctype: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(ctype + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + ctype + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    out = bytearray(b"\x89PNG\r\n\x1a\n")
    out.extend(chunk(b"IHDR", ihdr))
    out.extend(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
    out.extend(chunk(b"IEND", b""))
    return bytes(out), bytes(rgba)


def build_unityfs_bundle(
    rust_bin: pathlib.Path,
    tmp: pathlib.Path,
    variant: str,
    block_count: int,
    block_size: int,
) -> bytes:
    payloads = [make_structured_payload(0x1000 + i * 31, block_size) for i in range(block_count)]
    stored = []
    for idx, p in enumerate(payloads):
        in_f = tmp / f"_u_{idx}.in"
        out_f = tmp / f"_u_{idx}.out"
        in_f.write_bytes(p)
        run_measured(rust_bin, ["lz4hc", variant, str(in_f), str(out_f)])
        stored.append(out_f.read_bytes())

    info = bytearray(16)
    info.extend(struct.pack(">I", len(payloads)))
    for raw, comp in zip(payloads, stored):
        info.extend(struct.pack(">IIH", len(raw), len(comp), 3))
    info.extend(struct.pack(">I", 0))
    in_info = tmp / "_u_info.in"
    out_info = tmp / "_u_info.out"
    in_info.write_bytes(bytes(info))
    run_measured(rust_bin, ["lz4hc", variant, str(in_info), str(out_info)])
    info_bytes = out_info.read_bytes()

    f = bytearray(b"UnityFS\x00")
    f.extend(struct.pack(">I", 6))
    f.extend(b"5.x.x\x002022.3.62f3\x00")
    size_off = len(f)
    f.extend(struct.pack(">Q", 0))
    f.extend(struct.pack(">III", len(info_bytes), len(info), 0x43))
    f.extend(info_bytes)
    for st in stored:
        f.extend(st)
    f[size_off : size_off + 8] = struct.pack(">Q", len(f))
    return bytes(f)


def extract_jpegs_from_pdf(pdf_bytes: bytes) -> list[bytes]:
    """Extract all valid JPEG streams embedded in a PDF, largest first."""
    found: list[bytes] = []
    pos = 0
    while True:
        start = pdf_bytes.find(b"\xff\xd8\xff", pos)
        if start < 0:
            break
        end = pdf_bytes.find(b"endstream", start)
        if end < 0:
            break
        chunk = pdf_bytes[start:end].rstrip(b"\r\n")
        if chunk.endswith(b"\xff\xd9"):
            found.append(chunk)
        pos = end + 9
    found.sort(key=len, reverse=True)
    if not found:
        raise RuntimeError("no embedded JPEG found in PDF")
    return found


def select_shared_jpeg(
    pdf_bytes: bytes,
    rust_bin: pathlib.Path,
    pion_bin: pathlib.Path,
    tmp: pathlib.Path,
) -> bytes:
    """Pick the largest embedded JPEG that both Rust and Pion encode as SegmentKind::Jpg (kind 5)."""
    candidates = extract_jpegs_from_pdf(pdf_bytes)
    for idx, jpg in enumerate(candidates):
        in_p = tmp / f"_cand_{idx}.jpg"
        r_p = tmp / f"_cand_{idx}.r.pcf"
        p_p = tmp / f"_cand_{idx}.p.pcf"
        in_p.write_bytes(jpg)
        run_measured(rust_bin, ["encode", str(in_p), str(r_p)])
        run_measured(pion_bin, ["encode", str(in_p), str(p_p)])
        r_kinds = parse_pcf2_kinds(r_p.read_bytes())
        p_kinds = parse_pcf2_kinds(p_p.read_bytes())
        if r_kinds == [5] and p_kinds == [5]:
            return jpg
    raise RuntimeError("no embedded JPEG in PDF encoded as SegmentKind::Jpg (5) in both Rust and Pion")


def bench_interleaved(
    rust_bin: pathlib.Path,
    pion_bin: pathlib.Path,
    rust_args: list[str],
    pion_args: list[str],
    warmup: int,
    rounds: int,
    input_bytes: int,
    taskset_cpu: int | None,
) -> tuple[dict, dict]:
    for _ in range(warmup):
        run_measured(rust_bin, rust_args, taskset_cpu)
        run_measured(pion_bin, pion_args, taskset_cpu)

    r_times: list[float] = []
    r_rss: list[float | None] = []
    p_times: list[float] = []
    p_rss: list[float | None] = []
    r_out = 0
    p_out = 0
    for rd in range(rounds):
        if rd % 2 == 0:
            tm, ob, rss = run_measured(rust_bin, rust_args, taskset_cpu)
            r_times.append(tm)
            r_rss.append(rss)
            r_out = ob
            tm, ob, rss = run_measured(pion_bin, pion_args, taskset_cpu)
            p_times.append(tm)
            p_rss.append(rss)
            p_out = ob
        else:
            tm, ob, rss = run_measured(pion_bin, pion_args, taskset_cpu)
            p_times.append(tm)
            p_rss.append(rss)
            p_out = ob
            tm, ob, rss = run_measured(rust_bin, rust_args, taskset_cpu)
            r_times.append(tm)
            r_rss.append(rss)
            r_out = ob

    return (
        summarize(r_times, r_rss, input_bytes, r_out),
        summarize(p_times, p_rss, input_bytes, p_out),
    )


def format_ratio(pion_ms: float, rust_ms: float) -> float:
    return round(pion_ms / rust_ms, 2) if rust_ms > 0 else 0.0


def collect_env(warmup: int, rounds: int, slow_rounds: int, taskset_cpu: int | None) -> dict:
    def cmd_out(cmd: list[str]) -> str:
        try:
            p = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
            return p.stdout.strip() if p.returncode == 0 else "unknown"
        except Exception:
            return "unknown"

    cpu_model = "unknown"
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    except Exception:
        pass

    load_avg = "unknown"
    try:
        l1, l5, l15 = os.getloadavg()
        load_avg = f"{l1:.2f}, {l5:.2f}, {l15:.2f}"
    except Exception:
        pass

    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
        "cpu_model": cpu_model,
        "load_avg_1_5_15": load_avg,
        "rustc_version": cmd_out(["rustc", "--version"]),
        "cargo_version": cmd_out(["cargo", "--version"]),
        "pionc_version": cmd_out(["pionc", "--version"]),
        "python_version": platform.python_version(),
        "taskset_cpu": taskset_cpu,
        "warmup": warmup,
        "rounds": rounds,
        "slow_rounds": slow_rounds,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Head-to-head Rust vs Pion precomp2 benchmark suite.")
    parser.add_argument("--rust-bin", required=True, type=pathlib.Path)
    parser.add_argument("--pion-bin", required=True, type=pathlib.Path)
    parser.add_argument("--fixtures-dir", required=True, type=pathlib.Path)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--rounds", type=int, default=15)
    parser.add_argument("--slow-rounds", type=int, default=11)
    parser.add_argument("--pin-cpu", type=str, default="0", help="CPU core for taskset ('none' or '-1' to disable)")
    parser.add_argument("--json-out", type=pathlib.Path, default=None)
    args = parser.parse_args()

    taskset_cpu: int | None = None
    if args.pin_cpu.lower() not in ("none", "-1") and shutil.which("taskset"):
        cpu_id = int(args.pin_cpu)
        if cpu_id >= 0:
            chk = subprocess.run(["taskset", "-c", str(cpu_id), "true"], capture_output=True)
            if chk.returncode == 0:
                taskset_cpu = cpu_id

    env_info = collect_env(args.warmup, args.rounds, args.slow_rounds, taskset_cpu)
    rows: list[dict] = []

    with tempfile.TemporaryDirectory(prefix="precomp2_bench_") as td:
        tmp = pathlib.Path(td)

        # 1. LZ4HC (3 compressors) on 4 MiB structured payload
        print("=== 1. LZ4HC Compressors (4 MiB structured payload) ===")
        lz4_data = make_structured_payload(0xCAFEBABE, 4 * 1024 * 1024)
        lz4_in = tmp / "lz4_4m.bin"
        lz4_in.write_bytes(lz4_data)
        for variant in ["v1_10_0_l12", "v1_9_1_l12", "r131_l16"]:
            r_out = tmp / f"lz4_{variant}.rust"
            p_out = tmp / f"lz4_{variant}.pion"
            r_stat, p_stat = bench_interleaved(
                args.rust_bin,
                args.pion_bin,
                ["lz4hc", variant, str(lz4_in), str(r_out)],
                ["lz4hc", variant, str(lz4_in), str(p_out)],
                args.warmup,
                args.slow_rounds,
                len(lz4_data),
                taskset_cpu,
            )
            require(
                sha256_bytes(r_out.read_bytes()) == sha256_bytes(p_out.read_bytes()),
                f"LZ4HC {variant} output SHA-256 mismatch!",
            )
            row = {
                "workload": f"lz4hc/{variant}",
                "op": "compress",
                "input_bytes": len(lz4_data),
                "rust": r_stat,
                "pion": p_stat,
                "ratio_pion_over_rust": format_ratio(p_stat["median_ms"], r_stat["median_ms"]),
                "sha256_verified": True,
                "note": "byte-exact match",
            }
            rows.append(row)
            print(
                f"  {row['workload']:28s} Rust={r_stat['median_ms']:8.2f}ms ({r_stat['throughput_mibs']:6.2f} MiB/s) | "
                f"Pion={p_stat['median_ms']:8.2f}ms ({p_stat['throughput_mibs']:6.2f} MiB/s) | "
                f"ratio={row['ratio_pion_over_rust']:.2f}x"
            )

        # Prepare codec workloads
        print("=== 2. Generating Codec Workloads ===")
        text_2m = make_text_payload(2 * 1024 * 1024)
        text_512k = make_text_payload(512 * 1024)
        b64_zlib = base64.b64encode(zlib.compress(text_2m, 6))
        b64_lines = bytearray(b"Content-Transfer-Encoding: base64\r\n\r\n")
        for pos in range(0, len(b64_zlib), 76):
            b64_lines.extend(b64_zlib[pos : pos + 76])
            b64_lines.extend(b"\r\n")

        unityfs_4m = build_unityfs_bundle(args.rust_bin, tmp, "v1_10_0_l12", 32, 128 * 1024)
        png_512, png_512_rgba = make_png_rgba(512, 512)
        img_pdf = (args.fixtures_dir / "qpdf/image-streams.pdf").read_bytes()
        img_pdf_small = (args.fixtures_dir / "qpdf/image-streams-small.pdf").read_bytes()
        extracted_jpg = select_shared_jpeg(img_pdf, args.rust_bin, args.pion_bin, tmp)
        minimal_jpg = (args.fixtures_dir / "minimal.jpg").read_bytes()

        # (name, data, enc_flags, iters, rounds_for_case, test_display, track_kind)
        codec_workloads = [
            ("unityfs/v6_4m", unityfs_4m, [], 1, args.slow_rounds, False, 11),
            ("zlib/L1_2m", zlib.compress(text_2m, 1), [], 1, args.rounds, False, 2),
            ("zlib/L6_2m", zlib.compress(text_2m, 6), [], 1, args.rounds, False, 2),
            ("zlib/L9_2m", zlib.compress(text_2m, 9), [], 1, args.rounds, False, 2),
            ("gzip/L6_2m", make_gzip(text_2m, 6), [], 1, args.rounds, False, 3),
            ("bzip2/L6_512k", bz2.compress(text_512k, 6), [], 1, args.rounds, False, 8),
            ("bzip2/L9_512k", bz2.compress(text_512k, 9), [], 1, args.rounds, False, 8),
            ("png_webp/rgba_512x512", png_512, ["--png-webp"], 1, args.slow_rounds, True, 4),
            ("jpeg/minimal_222b", minimal_jpg, [], 100, args.rounds, False, 5),
            ("jpeg/pdf_extracted", extracted_jpg, [], 5, args.rounds, False, 5),
            ("pdf/image-streams-small", img_pdf_small, [], 5, args.rounds, False, 10),
            ("pdf/image-streams_2.97m", img_pdf, [], 1, args.slow_rounds, False, 10),
            ("base64/mime_zlib", bytes(b64_lines), [], 1, args.rounds, False, 7),
        ]

        print("=== 3. Codec Encode / Decode Benchmarks ===")
        for name, data, enc_flags, iters, n_rounds, test_display, track_kind in codec_workloads:
            tag = name.replace("/", "_")
            orig_sha = sha256_bytes(data)
            in_p = tmp / f"{tag}.in"
            r_pcf = tmp / f"{tag}.rust.pcf"
            p_pcf = tmp / f"{tag}.pion.pcf"
            r_dec = tmp / f"{tag}.rust.dec"
            p_dec = tmp / f"{tag}.pion.dec"
            in_p.write_bytes(data)

            iter_flag = ["--iters", str(iters)] if iters > 1 else []
            r_enc, p_enc = bench_interleaved(
                args.rust_bin,
                args.pion_bin,
                ["encode", str(in_p), str(r_pcf), *enc_flags, *iter_flag],
                ["encode", str(in_p), str(p_pcf), *enc_flags, *iter_flag],
                args.warmup,
                n_rounds,
                len(data),
                taskset_cpu,
            )
            r_dec_s, p_dec_s = bench_interleaved(
                args.rust_bin,
                args.pion_bin,
                ["decode", str(r_pcf), str(r_dec), *iter_flag],
                ["decode", str(p_pcf), str(p_dec), *iter_flag],
                args.warmup,
                n_rounds,
                len(data),
                taskset_cpu,
            )
            require(sha256_bytes(r_dec.read_bytes()) == orig_sha, f"{name} Rust decode SHA-256 mismatch!")
            require(sha256_bytes(p_dec.read_bytes()) == orig_sha, f"{name} Pion decode SHA-256 mismatch!")

            r_segs = sum(1 for k in parse_pcf2_kinds(r_pcf.read_bytes()) if k == track_kind)
            p_segs = sum(1 for k in parse_pcf2_kinds(p_pcf.read_bytes()) if k == track_kind)
            require(r_segs >= 1, f"{name} Rust did not emit expected segment kind {track_kind}")
            require(p_segs >= 1, f"{name} Pion did not emit expected segment kind {track_kind}")
            seg_note = f"segs R={r_segs} P={p_segs}" if (track_kind in (5, 10) or r_segs != p_segs) else ""

            for op, rs, ps in [("encode", r_enc, p_enc), ("decode", r_dec_s, p_dec_s)]:
                row = {
                    "workload": name,
                    "op": op,
                    "input_bytes": len(data),
                    "rust": rs,
                    "pion": ps,
                    "rust_transformed_segs": r_segs,
                    "pion_transformed_segs": p_segs,
                    "ratio_pion_over_rust": format_ratio(ps["median_ms"], rs["median_ms"]),
                    "sha256_verified": True,
                    "note": seg_note,
                }
                rows.append(row)
                print(
                    f"  {name:24s} {op:7s} Rust={rs['median_ms']:8.2f}ms ({rs['throughput_mibs']:6.2f} MiB/s) | "
                    f"Pion={ps['median_ms']:8.2f}ms ({ps['throughput_mibs']:6.2f} MiB/s) | "
                    f"ratio={row['ratio_pion_over_rust']:.2f}x | {seg_note}"
                )

            if test_display:
                r_disp_out = tmp / f"{tag}.rust.webp"
                p_disp_out = tmp / f"{tag}.pion.webp"
                r2p_disp_out = tmp / f"{tag}.r2p.webp"
                p2r_disp_out = tmp / f"{tag}.p2r.webp"
                r_ds, p_ds = bench_interleaved(
                    args.rust_bin,
                    args.pion_bin,
                    ["display", str(r_pcf), str(r_disp_out), "--iters", "20"],
                    ["display", str(p_pcf), str(p_disp_out), "--iters", "20"],
                    args.warmup,
                    args.rounds,
                    len(data),
                    taskset_cpu,
                )
                # Cross-language display consistency + pixel-level SHA-256 verification
                run_simple(args.pion_bin, ["display", str(r_pcf), str(r2p_disp_out)])
                run_simple(args.rust_bin, ["display", str(p_pcf), str(p2r_disp_out)])
                rb = r_disp_out.read_bytes()
                pb = p_disp_out.read_bytes()
                require(rb[:4] == b"RIFF" and rb[8:12] == b"WEBP", f"{name} Rust display invalid WEBP")
                require(pb[:4] == b"RIFF" and pb[8:12] == b"WEBP", f"{name} Pion display invalid WEBP")
                require(sha256_bytes(rb) == sha256_bytes(r2p_disp_out.read_bytes()), f"{name} R->R != R->P display")
                require(sha256_bytes(pb) == sha256_bytes(p2r_disp_out.read_bytes()), f"{name} P->P != P->R display")

                expected_rgba_sha = sha256_bytes(png_512_rgba)
                for lbl, wp in [("rust", r_disp_out), ("pion", p_disp_out)]:
                    raw_out = tmp / f"{tag}.{lbl}.rgba"
                    run_simple(args.rust_bin, ["webp-decode", str(wp), str(raw_out)])
                    require(
                        sha256_bytes(raw_out.read_bytes()) == expected_rgba_sha,
                        f"{name} ({lbl}) display WebP decoded RGBA pixels SHA-256 mismatch!",
                    )

                row = {
                    "workload": name,
                    "op": "display",
                    "input_bytes": len(data),
                    "rust": r_ds,
                    "pion": p_ds,
                    "ratio_pion_over_rust": format_ratio(p_ds["median_ms"], r_ds["median_ms"]),
                    "sha256_verified": True,
                    "note": "RGBA pixels + cross-lang display verified",
                }
                rows.append(row)
                print(
                    f"  {name:24s} {'display':7s} Rust={r_ds['median_ms']:8.3f}ms ({r_ds['throughput_mibs']:6.2f} MiB/s) | "
                    f"Pion={p_ds['median_ms']:8.3f}ms ({p_ds['throughput_mibs']:6.2f} MiB/s) | "
                    f"ratio={row['ratio_pion_over_rust']:.2f}x (RGBA pixels verified)"
                )

    result = {
        "env": env_info,
        "benchmarks": rows,
    }
    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(result, indent=2) + "\n")

    print("\nALL BENCHMARKS & SHA-256 VERIFICATIONS PASSED!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
