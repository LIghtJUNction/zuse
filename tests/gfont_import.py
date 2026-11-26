"""Offline .gfont conversion fixtures. Set ZUSE_BIN to include the real CLI/preview test."""
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/import_gfont.py"
spec = importlib.util.spec_from_file_location("import_gfont", SCRIPT)
converter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(converter)
ZUSE = os.environ.get("ZUSE_BIN")


def record(version, ch, coords, flags, advance=317.5):
    if version == 9:
        encoded = ch.encode("utf-8")
        prefix = struct.pack(">H", len(encoded)) + encoded + struct.pack(">f", advance)
    else:
        prefix = struct.pack(">H", ord(ch))
    return (prefix + struct.pack(">I", len(coords) * 2)
            + b"".join(struct.pack(">ff", x, y) for x, y in coords)
            + struct.pack(">I", len(flags)) + bytes(flags))


def container(path, version, items):
    # The metadata deliberately has no interpretable structure; the glyph ZIP is independent.
    with path.open("wb") as handle:
        handle.write(struct.pack(">II", version, 3) + b"abc")
        with zipfile.ZipFile(handle, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for key, value in items:
                archive.writestr(key, value)


def sample(path, version):
    dot = "🖋" if version == 9 else "中"
    container(path, version, [
        (str(ord("A")), record(version, "A", [(0, 0), (120, 0), (30, 60), (90, 60)], [0, 1, 0, 1])),
        (str(ord(dot)), record(version, dot, [(60, 30)], [0])),
        ("32", record(version, " ", [], [], advance=0)),
    ])
    return dot


class GfontImport(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.source = self.dir / "source.gfont"
        self.output = self.dir / "test.zfont"

    def run_tool(self, *options):
        return subprocess.run([sys.executable, str(SCRIPT), str(self.source), *map(str, options)],
                              capture_output=True, text=True,
                              env={**os.environ, "ZUSE_FONT_DIR": str(self.dir/"fonts")})

    def test_three_versions_share_scale_direction_order_dot_and_subset(self):
        for version in (5, 6, 9):
            with self.subTest(version=version):
                dot = sample(self.source, version)
                sink = io.BytesIO()
                result = converter.convert(self.source, sink)
                self.assertEqual((result["glyphs"], result["points"], result["blank_or_spacing_entries"]), (2, 6, 1))
                rows = sink.getvalue().decode().splitlines()
                self.assertEqual(rows[0],"U+0041 0,.25 1,.25 | .25,.75 .75,.75")
                self.assertEqual(rows[1],f"U+{ord(dot):04X} .5,.5 .5,.5")
                sink = io.BytesIO()
                converter.convert(self.source, sink, text=dot)
                self.assertEqual(sink.getvalue().decode().strip(), rows[1])

    def test_reject_bad_records_and_leave_files_unmodified(self):
        base = record(5, "A", [(0, 0), (6, 7)], [0, 1])
        cases = [
            (7, "65", base),
            (5, "66", base),
            (6, "65", base+b"unknown"),
            (5, "65", base[:-1]),
            (5, "65", record(5, "A", [(0, 1)], [1])),
            (6, "65", record(6, "A", [(0, 1), (1, 1)], [0, 2])),
            (5, "65", record(5, "A", [(float("nan"), 1)], [0])),
            (9, "65", record(9, "A", [(2, 3)], [0], advance=float("inf"))),
            (5, "../65", base),
        ]
        for version, key, data in cases:
            with self.subTest(version=version,key=key):
                container(self.source, version, [(key, data)])
                self.assertNotEqual(self.run_tool("--output", self.output).returncode, 0)
                self.assertFalse(self.output.exists())
                self.assertFalse(any(self.dir.glob(".zuse-gfont-*")))
        sample(self.source, 5)
        self.assertNotEqual(self.run_tool("--text","B","--output",self.output).returncode, 0)
        self.assertFalse(self.output.exists())
        self.output.write_bytes(b"previous")
        self.assertNotEqual(self.run_tool("--output",self.output).returncode, 0)
        self.assertEqual(self.output.read_bytes(),b"previous")

    def test_protected_zip_member_and_duplicate_names_are_refused(self):
        sample(self.source, 5)
        source = bytearray(self.source.read_bytes())
        for signature, offset in ((b"PK\x03\x04",6),(b"PK\x01\x02",8)):
            pos = source.index(signature) + offset
            source[pos] |= 1
        self.source.write_bytes(source)
        result = self.run_tool("--output", self.output)
        self.assertIn("protected ZIP", result.stderr)
        self.assertFalse(self.output.exists())
        entry = record(6, "A", [(0, 1)], [0])
        container(self.source, 6, [("65", entry), ("065", entry)])
        self.assertIn("duplicate", self.run_tool("--output", self.output).stderr)
        self.assertFalse(self.output.exists())

    @unittest.skipUnless(ZUSE, "set ZUSE_BIN to test actual Writer installation")
    def test_existing_zuse_font_store_and_preview_without_device(self):
        sample(self.source, 9)
        r = self.run_tool("--install","local-pen","--zuse",ZUSE)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout)["installed"],"local-pen")
        env = {**os.environ, "ZUSE_FONT_DIR": str(self.dir/"fonts")}
        cli = lambda *args: subprocess.check_output([ZUSE,*args],env=env,text=True)
        self.assertIn("local-pen",json.loads(cli("font","list"))["fonts"])
        result = json.loads(cli("-c",'write.preview(text="A",font="local-pen",size=1)'))
        self.assertEqual(result["results"][0]["value"]["strokes"],
                         [[[0,.25],[1,.25]],[[.25,.75],[.75,.75]]])
        font_path = self.dir/"fonts/local-pen.zfont"
        before = font_path.read_bytes()
        self.assertNotEqual(self.run_tool("--install","local-pen","--zuse",ZUSE).returncode,0)
        self.assertEqual(font_path.read_bytes(),before)


if __name__ == "__main__":
    unittest.main()
