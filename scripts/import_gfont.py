#!/usr/bin/env python3
"""Import locally authorized, plain move/line .gfont glyphs into a Zuse Writer font."""
import argparse
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import zipfile
import zlib

# Match handwriting.rs and drawing.rs in Zuse. The ZIP limits bound source parsing.
FONT_BYTES = 12 * 1024 * 1024
FONT_POINTS = 900_000
STROKE_POINTS = 4096
ARCHIVE_BYTES = 128 * 1024 * 1024
ENTRY_BYTES = 1024 * 1024
UNPACKED_BYTES = 256 * 1024 * 1024
VERSIONS = {5, 6, 9}


def entries(archive):
    items = archive.infolist()
    if len(items) > 65_536 or sum(i.file_size for i in items) > UNPACKED_BYTES:
        raise ValueError("source font exceeds archive limits")
    result = {}
    for item in items:
        if item.flag_bits & 0x41:
            raise ValueError("protected ZIP entries are unsupported")
        name = item.orig_filename
        if (item.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
                or item.file_size > ENTRY_BYTES or not (1 <= len(name) <= 7)
                or not name.isascii() or not name.isdecimal()):
            raise ValueError(f"unsupported ZIP member {name[:40]!r}; expected a decimal code point")
        cp = int(name)
        if cp > 0x10FFFF or 0xD800 <= cp <= 0xDFFF or cp in result:
            raise ValueError(f"invalid or duplicate code point U+{cp:04X}")
        result[cp] = item
    if not result:
        raise ValueError("font has no glyph entries")
    return result


def read_strokes(archive, item, cp, version):
    # Only read the normal ZIP entry. The vendor's preceding metadata is never decoded.
    data = memoryview(archive.read(item))
    try:
        if version == 9:
            length, = struct.unpack_from(">H", data)
            if not 1 <= length <= 4 or bytes(data[2:2 + length]) != chr(cp).encode("utf-8"):
                raise ValueError("record character does not match its ZIP name")
            advance, count = struct.unpack_from(">fI", data, 2 + length)
            if not math.isfinite(advance) or advance < 0:
                raise ValueError("invalid character advance")
            pos = 2 + length + 8
        else:
            key, count = struct.unpack_from(">HI", data)
            if key != cp:
                raise ValueError("record character does not match its ZIP name")
            pos = 6
        end = pos + count * 4
        flags_count, = struct.unpack_from(">I", data, end)
        if count % 2 or flags_count != count // 2 or end + 4 + flags_count != len(data):
            raise ValueError("unsupported point/command layout or trailing record")
        flags = data[end + 4:]
        if flags and flags[0] != 0:
            raise ValueError("the first point is not a move")
        strokes = []
        for (x, y), command in zip(struct.iter_unpack(">ff", data[pos:end]), flags):
            if not math.isfinite(x) or not math.isfinite(y):
                raise ValueError("non-finite coordinate")
            if command == 0:
                strokes.append([])
            elif command != 1:
                raise ValueError(f"unsupported drawing command {command}; only move/line are supported")
            strokes[-1].append((x, y))
        return strokes
    except (ValueError, struct.error) as error:
        raise ValueError(f"U+{cp:04X}: {error}") from error


def visible(cp):
    return not (cp <= 31 or 0x7f <= cp <= 0x9f or chr(cp).isspace())


def scan(archive, index, version, wanted):
    min_x = min_y = math.inf
    max_x = max_y = -math.inf
    selected = []
    point_count = 0
    blanks = 0
    for cp, item in sorted(index.items()):
        strokes = read_strokes(archive, item, cp, version)
        if not visible(cp) or not strokes:
            blanks += 1
            continue
        include = wanted is None or cp in wanted
        # One shared square preserves relative glyph sizes, common baseline and aspect
        # ratio. Derive it from the whole font even when emitting a subset.
        for stroke in strokes:
            for x, y in stroke:
                min_x = min(min_x, x)
                max_x = max(max_x, x)
                min_y = min(min_y, y)
                max_y = max(max_y, y)
            if include:
                if len(stroke) > STROKE_POINTS:
                    raise ValueError(f"U+{cp:04X}: stroke exceeds Writer's {STROKE_POINTS} points")
                point_count += max(2, len(stroke))
        if include:
            selected.append(cp)
            if point_count > FONT_POINTS:
                raise ValueError("font exceeds Writer's 900,000 points; select a subset with --text")
    if wanted is not None and (missing := wanted.difference(selected)):
        codes = ", ".join(f"U+{cp:04X}" for cp in sorted(missing)[:8])
        raise ValueError(f"requested characters have no supported ink: {codes}")
    if not selected:
        raise ValueError("no visible glyphs with ink")
    return selected, (min_x, min_y, max_x, max_y), point_count, blanks


def convert(source, output, text=None):
    wanted = None if text is None else {ord(ch) for ch in text if visible(ord(ch))}
    if wanted == set():
        raise ValueError("--text must contain a visible character")
    with source.open("rb") as file:
        if os.fstat(file.fileno()).st_size > ARCHIVE_BYTES:
            raise ValueError("source font exceeds 128 MiB")
        prefix = file.read(8)
        if len(prefix) != 8:
            raise ValueError("truncated .gfont header")
        version = int.from_bytes(prefix[:4], "big")
        if version not in VERSIONS:
            raise ValueError(f"unsupported .gfont version {version}; expected 5, 6 or 9")
        with zipfile.ZipFile(file) as archive:
            index = entries(archive)
            if min(i.header_offset for i in index.values()) < len(prefix):
                raise ValueError("missing .gfont metadata prefix")
            selected, box, points, blanks = scan(archive, index, version, wanted)
            xmin, ymin, xmax, ymax = box
            width, height = xmax - xmin, ymax - ymin
            side = max(width, height) or 1.0

            def unit(value):
                return f"{value:.5f}".rstrip("0").rstrip(".").lstrip("0") or "0"

            written = 0
            for cp in selected:
                paths = []
                for stroke in read_strokes(archive, index[cp], cp, version):
                    pairs = []
                    for x, y in stroke:
                        nx = (x - xmin + (side - width) / 2) / side
                        ny = (y - ymin + (side - height) / 2) / side
                        pairs.append(f"{unit(nx)},{unit(ny)}")
                    # A repeated vertex expresses a dot in Writer's two-point stroke format.
                    if len(pairs) == 1:
                        pairs.append(pairs[0])
                    paths.append(" ".join(pairs))
                row = (f"U+{cp:04X} " + " | ".join(paths) + "\n").encode("ascii")
                written += len(row)
                if written > FONT_BYTES:
                    raise ValueError("font exceeds Writer's 12 MiB; select a subset with --text")
                output.write(row)
    return {"version": version, "glyphs": len(selected), "points": points, "bytes": written,
            "blank_or_spacing_entries": blanks, "source_ink_bounds": box,
            "spacing": "Writer fixed advance; whole-font ink fitted into one common square"}


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        "X and downward-positive Y share a single scale across the font. Writer chooses fixed "
        "spacing; source-specific advance and vendor metadata are not imported. "
        "Only use local glyphs you are authorized to import. No network or device access."))
    parser.add_argument("source", type=Path)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--install", metavar="NAME", help="install through zuse font add")
    target.add_argument("--output", type=Path, metavar="FILE.zfont", help="create a new local font file")
    parser.add_argument("--text", metavar="TEXT", help="include only the visible characters needed for TEXT")
    parser.add_argument("--zuse", default=os.environ.get("ZUSE_BIN") or "zuse",
                        help="Zuse executable for --install (default: ZUSE_BIN or zuse)")
    args = parser.parse_args()
    try:
        # Stage on the destination filesystem for atomic, no-replace publication.
        directory = args.output.parent if args.output is not None else None
        with tempfile.TemporaryDirectory(prefix=".zuse-gfont-", dir=directory) as tmp:
            staged = Path(tmp) / "import.zfont"
            with staged.open("wb") as stream:
                receipt = convert(args.source, stream, args.text)
            if args.output is not None:
                os.link(staged, args.output)
                receipt["output"] = str(args.output)
            else:
                subprocess.run([args.zuse, "font", "add", args.install, str(staged)],
                               check=True, stdout=subprocess.DEVNULL)
                receipt["installed"] = args.install
        print(json.dumps(receipt, ensure_ascii=False))
    except (ValueError, OSError, EOFError, zipfile.BadZipFile, zlib.error, subprocess.CalledProcessError) as error:
        parser.exit(1, f"font import failed: {error}\n")


if __name__ == "__main__":
    main()
