# Handwriting and G-code

Zuse lays text out as ordered pen strokes. A pen lift separates two paths. The same glyph geometry drives a mouse, Linux touch input and an XY pen plotter. The built-in single-line font includes Latin capitals, digits and a small Chinese sample including 你好. Lowercase letters use capital shapes. Install a font with the characters you need for a larger repertoire. A missing glyph reports its Unicode code point before input begins.

## Fonts and spacing

Run `zuse font list` to see the available names. `zuse font add mypen examples/two.zfont` installs the example as `mypen`, and `zuse font remove mypen` removes it. The file contains one Unicode character per line. Points use a unit square with its origin at the top left. A vertical bar lifts the pen. For example, this glyph draws the shorter horizontal of 二 first and the longer one second.

~~~text
U+4E8C 0.2,0.2 0.8,0.2 | 0,0.8 1,0.8
~~~

~~~sh
zuse -c 'write.preview(text="二二",font="mypen",size=10,advance=12,x=20,y=30)'
zuse run examples/handwriting.zuse
~~~

| Argument | Meaning |
| --- | --- |
| `size` | Side length of the glyph's unit square, in design units |
| `advance`, `line_height` | Distance between character origins and between line origins |
| `reference_units`, `reference_distance` | Calibrate the drawing; 10 reference units and a distance of 5 produce 0.5 output units per design unit |
| `x`, `y` | Origin on the target, in output units; these coordinates are added after scaling |
| `offset_x`, `offset_y` | Independent maximum random displacements in design units, applied once per character. Both default to zero |
| `seed` | Repeat the layout exactly with the same font and settings. With offsets enabled, omitting the seed chooses a new one; `write.preview` returns it for reuse on a device |
| `interval` | Milliseconds per input vertex for mouse and touch writing |

For example, `size=10,advance=12,reference_units=10,reference_distance=5` gives a 5-unit glyph and 6 units between adjacent origins. The screen uses downward-positive Y. On a Cartesian XY plotter the first glyph begins at the specified top-left work coordinate and its downstrokes decrease machine Y. Set `swap_xy` to exchange the local axes, then `flip_x` and/or `flip_y` to reverse the corresponding machine directions. These transforms precede the final origin offset.

### Import a local KuiXiang font

From the extracted release directory, run the Python 3 importer on an unencrypted `.gfont` file you have permission to use. Versions 5, 6 and 9 are supported.

```sh
python3 scripts/import_gfont.py my-font.gfont --output my-font.zfont
./zuse font add local-pen my-font.zfont
```

Add `--text "Hello"` to convert only the characters in that text. To install directly, use `--install local-pen --zuse ./zuse` in place of `--output my-font.zfont`. On Windows, use `python` and `.\\zuse.exe`. Writer keeps a common scale for all glyphs and applies its own character spacing. Existing output files are preserved.

On GNU/Linux, open Writer with a local paper preset:

```sh
./zuse-writer --kuixiang-page page_settings.json "Letter"
```

Use the exact preset name from your file. You can omit it when the file contains a single preset. Writer reads the page width, height and four margins in millimetres. A calibrated page keeps its taught size and takes the imported margins. Font size, spacing, directions and machine settings stay under Writer's control.

## Desktop input

~~~sh
zuse -c 'write.mouse(text="Z",x=200,y=150,size=48)' --dry-run
zuse -c 'write.touchpad(text="一",x=100,y=100,size=50,width=1000,height=700)' --dry-run
zuse -c 'write.touchscreen(text="你好",x=150,y=120,size=36,width=1920,height=1080)' --dry-run
~~~

Remove `--dry-run` to submit input. Mouse writing uses the system pointer. On Linux, touch writing creates a virtual absolute input device via writable `/dev/uinput`. A touchscreen supplies direct contacts mapped by the compositor. A touchpad supplies indirect finger contacts; the handwriting application or input method determines how to interpret them. Width and height describe the logical device surface. A point at x=width or y=height is outside it.

## Writing machine

~~~sh
zuse -c 'write.export(text="Z",path="letter.nc",x=5,y=15,size=5,pen_up=3,pen_down=0,feed=120)'
zuse machine grbl-send letter.nc /dev/ttyUSB0 115200 --dry-run
~~~

`write.gcode` returns program text in JSON. `write.export` creates a new file for a controller or sender. XY and pen Z values are millimetres; feed is millimetres per minute. Use the work origin, travel range and pen-up/down values for the selected machine. The Z-lift profile requires a physical or firmware-mapped Z axis. Both Z directions are supported: enter the actual up/down positions even when lowering the pen increases Z. `zuse machine probe PORT BAUD` queries the state and firmware. Supported controller identities are GRBL 1.1 and the BtWriter 1.3a.20221114 firmware with its matching device marker. `grbl-send` verifies the controller and sends one line at a time. The execution receipt requires an acknowledgement for each program line and a final Idle status. Actual ink transfer needs a paper or camera observation.

`zuse machine grbl-check letter.nc PORT 115200` sends the trajectory to the controller's parser in its verified Check state. This mode suppresses axis motion. The preflight checks stored startup blocks before toggling the mode, since GRBL performs its own soft reset on exit. Success requires a final observed Idle state. Use `--dry-run` for a purely local syntax check.

## Taught travel and work coordinates

Run `zuse-writer --align` with an empty pen holder to teach two opposite XY corners and the physical upper and lower Z positions. The writer uses these measurements to position the holder for mounting. With the holder at the verified height, seat the pen tip against the working paper and clamp the pen. The installation height becomes pen-down. The lift distance follows the measured upward direction, including on machines where lifting decreases Z. The [Writer guide](../apps/writer/README.md) describes the controls.

An AI client can use the same named session as the GUI. Read `plotter.status(work=true,offset=true)` for fresh millimetre coordinates and the measured work offset (WCO), and `plotter.bounds()` for active limits. Once limits are applied, the session verifies the starting position and every linear endpoint before sending the first motion block. It also checks individual jogs and pen moves. Explicit `G21` must appear on or before the first feed word so a program has a defined metric feed.

The saved profile belongs to the measured device and work offset. On machines without homing, the carriage can be moved with power off while the reported WCO stays the same. Compare the physical origin and axis directions on the machine before reusing a saved profile. With an observed controller restart, re-establish the physical reference before reapplying stale limits. The active limits apply to clients using that same retained session. An independently opened serial connection has its own state.

## X/Z lathe

`zuse machine lathe-check PART.ngc` checks the supported metric, absolute, linear-motion subset offline. Run `zuse machine lathe-run PART.ngc /path/to/active.ini --dry-run` to inspect that same program. Real dispatch uses an existing LinuxCNC installation through its native Python module. It requires the matching active X/Z lathe configuration, homed axes, enabled controller and idle AUTO state. The command does not home or power a machine. `--max-wait-ms` bounds observation. After a lost acknowledgement or a timeout, inspect the controller's state before issuing another run.
