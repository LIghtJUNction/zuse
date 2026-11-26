#!/usr/bin/env python3
"""Convert pinned Make Me a Hanzi and Hershey data to distributable .zfont assets."""

import argparse
import json
from pathlib import Path


HANZI_REV = "bddc96d41bef78427ed0e034e9f7e31d71fd1b92"
HERSHEY_REV = "1356bf2f83d380fcef68c887e88675eb9d445d86"


def glyph(character, strokes):
    return f"U+{ord(character):04X} " + " | ".join(
        " ".join(f"{x:.4f},{y:.4f}" for x, y in stroke) for stroke in strokes
    )


def hanzi(source):
    def normalize(value):
        return min(1.0, max(0.0, value / 1024.0))

    for line in source.read_text(encoding="utf-8").splitlines():
        item = json.loads(line)
        strokes = [
            [(normalize(x), normalize(900 - y)) for x, y in stroke]
            for stroke in item["medians"]
            if len(stroke) >= 2
        ]
        if strokes:
            yield glyph(item["character"], strokes)

    yield from (
        "U+3002 .4,.75 .52,.72 .59,.79 .53,.87 .41,.84 .4,.75",
        "U+FF0C .5,.7 .56,.75 .48,.88",
        "U+3001 .4,.69 .57,.83",
        "U+FF01 .5,.14 .5,.65 | .5,.81 .51,.84",
        "U+FF1F .3,.31 .37,.18 .57,.15 .7,.26 .68,.4 .51,.56 .5,.66 | .5,.81 .51,.84",
        "U+FF1A .5,.43 .51,.45 | .5,.72 .51,.74",
        "U+FF1B .5,.43 .51,.45 | .56,.71 .49,.88",
        "U+201C .4,.18 .34,.29 | .56,.18 .5,.29",
        "U+201D .38,.18 .32,.29 | .54,.18 .48,.29",
        "U+FF08 .65,.12 .5,.3 .45,.51 .5,.73 .65,.88",
        "U+FF09 .35,.12 .5,.3 .55,.51 .5,.73 .35,.88",
    )


def hershey(source):
    origin = ord("R")
    records = source.read_text(encoding="ascii").splitlines()
    for character, record in zip(map(chr, range(32, 127)), records, strict=False):
        if character == " ":
            continue
        pairs = record[10:]
        points = zip(pairs[::2], pairs[1::2], strict=True)
        left, right = (ord(c) - origin for c in record[8:10])
        strokes = [[]]
        for x, y in points:
            if (x, y) == (" ", "R"):
                strokes.append([])
            else:
                strokes[-1].append((ord(x) - origin, ord(y) - origin))
        vertices = [point for stroke in strokes for point in stroke]
        if not vertices:
            continue
        center = (left + right) / 2
        normalized = [
            [
                (min(1.0, max(0.0, 0.5 + (x - center) / 36.0)),
                 min(1.0, max(0.0, (y + 16.0) / 32.0)))
                for x, y in stroke
            ]
            for stroke in strokes
            if len(stroke) >= 2
        ]
        if normalized:
            yield glyph(character, normalized)


def write_pack(directory, name, introduction, license_text, glyphs):
    rows = list(glyphs)
    if len({line.split()[0] for line in rows}) != len(rows):
        raise ValueError(f"duplicate glyph in {name}")
    license_header = "\n".join(f"# {line}" for line in license_text.splitlines())
    result = f"# {introduction}\n{license_header}\n" + "\n".join(rows) + "\n"
    (directory / f"{name}.zfont").write_text(result, encoding="utf-8")
    print(f"{name}: {len(rows)} glyphs, {len(result.encode('utf-8'))} bytes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hanzi", type=Path, required=True)
    parser.add_argument("--arphic-license", type=Path, required=True)
    parser.add_argument("--hershey-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)

    write_pack(
        args.output,
        "hanzi-kaiti",
        f"2026-11-26: Zuse converted Make Me a Hanzi ({HANZI_REV}) stroke medians "
        "to normalized y-down single-line glyphs and added punctuation. "
        "Derived from Arphic PL KaitiM GB and Arphic PL UKai.",
        args.arphic_license.read_text(encoding="utf-8"),
        hanzi(args.hanzi),
    )

    licensing = (args.hershey_dir / "COPYING").read_text(encoding="utf-8")
    notice = licensing.split(" Hershey fonts (hershey-fonts/*.jhf) license\n")[-1].strip()
    for name, file in [("latin-script", "scripts.jhf"), ("latin-cursive", "cursive.jhf")]:
        write_pack(
            args.output,
            name,
            f"Zuse converted Hershey {file} ({HERSHEY_REV}) from JHF to normalized ASCII stroke glyphs.",
            notice,
            hershey(args.hershey_dir / "hershey-fonts" / file),
        )


if __name__ == "__main__":
    main()
