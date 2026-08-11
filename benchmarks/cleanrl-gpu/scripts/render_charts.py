#!/usr/bin/env python3
"""Render deterministic benchmark-summary.svg and .png with no dependencies."""

from __future__ import annotations

import argparse
import binascii
import html
import struct
import sys
import zlib
from pathlib import Path
from typing import Any

from contracts import (
    ContractError,
    format_number,
    load_json,
    manifest_run_ids,
    read_metrics_csv,
    validate_manifest,
)


ROOT = Path(__file__).resolve().parents[1]
SAFE_ERROR = "error: benchmark inputs could not be read or validated"
WIDTH, HEIGHT = 960, 540
BACKGROUND = (11, 16, 32)
PANEL = (24, 34, 56)
TEXT = (231, 238, 248)
MUTED = (148, 163, 184)
ACCENT = (45, 212, 191)
SOURCE_COLORS = {
    "factory": (96, 165, 250),
    "model": (244, 114, 182),
    "kubernetes": (167, 139, 250),
    "gpu": (52, 211, 153),
    "rl": (251, 191, 36),
}

# Compact 5x7 bitmap font. Each hexadecimal row stores five active bits.
FONT = {
    " ": (0, 0, 0, 0, 0, 0, 0), "-": (0, 0, 0, 31, 0, 0, 0),
    ".": (0, 0, 0, 0, 0, 12, 12), ":": (0, 12, 12, 0, 12, 12, 0),
    "/": (1, 2, 4, 8, 16, 0, 0), "+": (0, 4, 4, 31, 4, 4, 0),
    "%": (17, 2, 4, 8, 17, 0, 0), "?": (14, 17, 1, 2, 4, 0, 4),
    "0": (14, 17, 19, 21, 25, 17, 14), "1": (4, 12, 4, 4, 4, 4, 14),
    "2": (14, 17, 1, 2, 4, 8, 31), "3": (30, 1, 1, 14, 1, 1, 30),
    "4": (2, 6, 10, 18, 31, 2, 2), "5": (31, 16, 16, 30, 1, 1, 30),
    "6": (14, 16, 16, 30, 17, 17, 14), "7": (31, 1, 2, 4, 8, 8, 8),
    "8": (14, 17, 17, 14, 17, 17, 14), "9": (14, 17, 17, 15, 1, 1, 14),
    "A": (14, 17, 17, 31, 17, 17, 17), "B": (30, 17, 17, 30, 17, 17, 30),
    "C": (14, 17, 16, 16, 16, 17, 14), "D": (30, 17, 17, 17, 17, 17, 30),
    "E": (31, 16, 16, 30, 16, 16, 31), "F": (31, 16, 16, 30, 16, 16, 16),
    "G": (14, 17, 16, 23, 17, 17, 14), "H": (17, 17, 17, 31, 17, 17, 17),
    "I": (14, 4, 4, 4, 4, 4, 14), "J": (7, 2, 2, 2, 2, 18, 12),
    "K": (17, 18, 20, 24, 20, 18, 17), "L": (16, 16, 16, 16, 16, 16, 31),
    "M": (17, 27, 21, 21, 17, 17, 17), "N": (17, 25, 21, 19, 17, 17, 17),
    "O": (14, 17, 17, 17, 17, 17, 14), "P": (30, 17, 17, 30, 16, 16, 16),
    "Q": (14, 17, 17, 17, 21, 18, 13), "R": (30, 17, 17, 30, 20, 18, 17),
    "S": (15, 16, 16, 14, 1, 1, 30), "T": (31, 4, 4, 4, 4, 4, 4),
    "U": (17, 17, 17, 17, 17, 17, 14), "V": (17, 17, 17, 17, 17, 10, 4),
    "W": (17, 17, 17, 21, 21, 21, 10), "X": (17, 17, 10, 4, 10, 17, 17),
    "Y": (17, 17, 10, 4, 4, 4, 4), "Z": (31, 1, 2, 4, 8, 16, 31),
}


def _metric_text(row: dict[str, Any]) -> str:
    parts: list[str] = []
    if "factory" in row["sources"]:
        parts.append(f"{row['status']} / {format_number(row['wall_seconds'])}s / {row['retry_count']} retries")
    if "model" in row["sources"]:
        parts.append(f"{row['total_tokens']} tokens / {row['response_count']} responses")
    if "kubernetes" in row["sources"]:
        parts.append(f"{row['pod_count']} pods / {row['pod_restart_count']} restarts")
    if "gpu" in row["sources"]:
        parts.append(
            f"GPU {format_number(row['gpu_utilization_mean_pct'])}% / "
            f"{format_number(row['gpu_memory_peak_mib'])} MiB peak"
        )
    if "rl" in row["sources"]:
        parts.append(
            f"return {format_number(row['evaluation_return_mean'])} +/- "
            f"{format_number(row['evaluation_return_stddev'])}"
        )
    return " | ".join(parts)


def render_svg(manifest: dict[str, Any], rows: list[dict[str, Any]]) -> bytes:
    row_by_id = {row["run_id"]: row for row in rows}
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">',
        '<rect width="960" height="540" fill="#0b1020"/>',
        '<text x="48" y="56" fill="#e7eef8" font-family="system-ui,sans-serif" font-size="28" font-weight="700">CleanRL GPU benchmark</text>',
        '<text x="48" y="84" fill="#94a3b8" font-family="system-ui,sans-serif" font-size="14">Fixed aggregate-only summary · manifest: 2 issue jobs + 2 RL runs</text>',
    ]
    if not rows:
        lines.extend(
            [
                '<rect x="120" y="154" width="720" height="250" rx="18" fill="#182238" stroke="#334155"/>',
                '<text x="480" y="250" text-anchor="middle" fill="#2dd4bf" font-family="system-ui,sans-serif" font-size="30" font-weight="700">No measured results</text>',
                '<text x="480" y="292" text-anchor="middle" fill="#cbd5e1" font-family="system-ui,sans-serif" font-size="17">metrics.csv contains its canonical header and zero data rows.</text>',
                '<text x="480" y="327" text-anchor="middle" fill="#94a3b8" font-family="system-ui,sans-serif" font-size="14">No performance, reliability, GPU, or RL values are inferred.</text>',
            ]
        )
    else:
        for index, run_id in enumerate(manifest_run_ids(manifest)):
            y = 120 + index * 96
            row = row_by_id.get(run_id)
            lines.append(f'<rect x="48" y="{y}" width="864" height="78" rx="12" fill="#182238"/>')
            lines.append(
                f'<text x="70" y="{y + 29}" fill="#e7eef8" font-family="ui-monospace,monospace" '
                f'font-size="15" font-weight="700">{html.escape(run_id)}</text>'
            )
            if row is None:
                detail = "No aggregate observation"
                source_text = "PLANNED"
            else:
                detail = _metric_text(row)
                source_text = " · ".join(source.upper() for source in row["sources"])
            lines.append(
                f'<text x="70" y="{y + 56}" fill="#94a3b8" font-family="system-ui,sans-serif" '
                f'font-size="13">{html.escape(detail)}</text>'
            )
            lines.append(
                f'<text x="890" y="{y + 29}" text-anchor="end" fill="#2dd4bf" '
                f'font-family="system-ui,sans-serif" font-size="12">{html.escape(source_text)}</text>'
            )
    lines.extend(
        [
            '<text x="48" y="516" fill="#64748b" font-family="system-ui,sans-serif" font-size="12">Only whitelisted aggregates are rendered; absence remains absence.</text>',
            '</svg>',
            '',
        ]
    )
    return "\n".join(lines).encode("utf-8")


class Canvas:
    def __init__(self) -> None:
        self.pixels = bytearray(BACKGROUND * (WIDTH * HEIGHT))

    def rectangle(self, x: int, y: int, width: int, height: int, color: tuple[int, int, int]) -> None:
        x0, y0 = max(0, x), max(0, y)
        x1, y1 = min(WIDTH, x + width), min(HEIGHT, y + height)
        row = bytes(color) * max(0, x1 - x0)
        for py in range(y0, y1):
            start = (py * WIDTH + x0) * 3
            self.pixels[start : start + len(row)] = row

    def text(self, x: int, y: int, value: str, color: tuple[int, int, int], scale: int = 2) -> None:
        cursor = x
        for character in value.upper():
            glyph = FONT.get(character, FONT["?"])
            for gy, bits in enumerate(glyph):
                for gx in range(5):
                    if bits & (1 << (4 - gx)):
                        self.rectangle(cursor + gx * scale, y + gy * scale, scale, scale, color)
            cursor += 6 * scale


def _png_chunk(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", binascii.crc32(kind + payload) & 0xFFFFFFFF)


def _encode_png(canvas: Canvas) -> bytes:
    stride = WIDTH * 3
    raw = b"".join(b"\x00" + canvas.pixels[offset : offset + stride] for offset in range(0, len(canvas.pixels), stride))
    return (
        b"\x89PNG\r\n\x1a\n"
        + _png_chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
        + _png_chunk(b"IDAT", zlib.compress(raw, 9))
        + _png_chunk(b"IEND", b"")
    )


def render_png(manifest: dict[str, Any], rows: list[dict[str, Any]]) -> bytes:
    canvas = Canvas()
    canvas.text(48, 35, "CLEANRL GPU BENCHMARK", TEXT, 3)
    canvas.text(48, 75, "MANIFEST: 2 ISSUE JOBS + 2 RL RUNS", MUTED, 2)
    if not rows:
        canvas.rectangle(120, 154, 720, 250, PANEL)
        canvas.text(315, 224, "NO MEASURED RESULTS", ACCENT, 3)
        canvas.text(252, 284, "METRICS CSV HAS ZERO DATA ROWS", TEXT, 2)
        canvas.text(234, 324, "NO VALUES ARE INFERRED OR FABRICATED", MUTED, 2)
    else:
        row_by_id = {row["run_id"]: row for row in rows}
        for index, run_id in enumerate(manifest_run_ids(manifest)):
            y = 116 + index * 96
            canvas.rectangle(48, y, 864, 78, PANEL)
            canvas.text(68, y + 13, run_id, TEXT, 2)
            row = row_by_id.get(run_id)
            if row is None:
                canvas.text(68, y + 48, "NO AGGREGATE OBSERVATION", MUTED, 1)
                continue
            source_x = 68
            for source in row["sources"]:
                canvas.rectangle(source_x, y + 47, 70, 10, SOURCE_COLORS[source])
                source_x += 78
            if "model" in row["sources"]:
                canvas.text(500, y + 46, f"TOKENS {row['total_tokens']}", MUTED, 1)
            if "gpu" in row["sources"]:
                canvas.rectangle(690, y + 47, 180, 10, (51, 65, 85))
                fill = round(180 * row["gpu_utilization_mean_pct"] / 100)
                canvas.rectangle(690, y + 47, fill, 10, SOURCE_COLORS["gpu"])
    canvas.text(48, 514, "AGGREGATES ONLY - ABSENCE REMAINS ABSENCE", MUTED, 1)
    return _encode_png(canvas)


def _load_inputs(manifest_path: Path, metrics_path: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    with manifest_path.open("r", encoding="utf-8") as stream:
        manifest = validate_manifest(load_json(stream))
    with metrics_path.open("r", encoding="utf-8", newline="") as stream:
        rows = read_metrics_csv(stream, manifest)
    return manifest, rows


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Render fixed benchmark summary charts.")
    parser.add_argument("--manifest", type=Path, default=ROOT / "run-manifest.json")
    parser.add_argument("--metrics", type=Path, default=ROOT / "data" / "metrics.csv")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "charts")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        manifest, rows = _load_inputs(args.manifest, args.metrics)
        if not rows:
            raise ContractError("no measured benchmark rows")
        svg = render_svg(manifest, rows)
        png = render_png(manifest, rows)
        args.output_dir.mkdir(parents=True, exist_ok=True)
        (args.output_dir / "benchmark-summary.svg").write_bytes(svg)
        (args.output_dir / "benchmark-summary.png").write_bytes(png)
        return 0
    except (ContractError, OSError, UnicodeError, BrokenPipeError):
        print(SAFE_ERROR, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
