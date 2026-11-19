# Native serial and USB I/O

zuse exposes serial streams and USB interfaces through its Rust library and the shared CLI/MCP executor. Device selection is explicit. Serial uses platform APIs directly; USB loads the installed libusb 1.0 library through Project Z's zsys loader. Neither adapter launches a serial or USB command-line helper.

## Implementation and verification scope

The transfer acceptance below exercised Linux PTYs and a synthetic C implementation of the libusb ABI. It did not open a physical serial port or perform a physical USB claim or transfer. A separate real-system check enumerated serial candidate names and USB descriptors without opening handles. The [evidence record](evidence/device-io-2026-11-20.json) distinguishes these scopes.

| Adapter | Implemented path | Evidence from this acceptance |
| --- | --- | --- |
| Linux serial | termios, nonblocking file I/O and poll | Real kernel PTY: binary transfers, raw/8N1 settings, baud, timeout, backpressure and restoration |
| macOS serial | Darwin termios and poll | Source implementation; no macOS execution in this acceptance |
| Windows serial | Win32 COM handle, DCB and COMMTIMEOUTS | Source and ABI review; no Windows execution in this acceptance |
| Linux USB | System libusb 1.0 loaded through zsys | Real Rust-to-C calls into a synthetic shared library, plus separate real-system descriptor enumeration |
| macOS / Windows USB | System libusb 1.0 through the corresponding zsys loader | Source implementation; no live platform or device verification in this acceptance |

The USB fixture checks argument layout, buffer contents, result handling and native resource lifetime. It does not establish transfer compatibility with firmware, a kernel driver, cable, hub or operating-system USB backend. Serial candidate paths are operating-system names, not a count of attached physical serial devices.

## Serial API

Opening configures the selected port as 8N1 with software and hardware flow control disabled. The adapter captures the previous configuration first. Linux and macOS save termios; Windows saves the DCB and communication timeouts. See the [implementation](../src/serial.rs), Microsoft's [DCB definition](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-dcb) and [COMMTIMEOUTS definition](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts).

| Operation | Parameters and behavior |
| --- | --- |
| `serial.ports()` | Lists candidate names without opening or configuring them |
| `serial.open(path, baud=115200)` | Selects one device path and configures 8N1 |
| `serial.status()` | Reports the current connection's path, baud and format |
| `serial.read(length=4096, timeout=1000)` | Returns an available chunk up to the requested length; timeout is in milliseconds |
| `serial.write(text=..., timeout=1000)` | Writes the text's UTF-8 bytes |
| `serial.write(base64=..., timeout=1000)` | Writes decoded binary bytes; exactly one payload field is required |
| `serial.close()` | Attempts configuration restoration, then closes the handle |

Unix paths must explicitly name a device below `/dev`. Windows accepts `COM1` through `COM4096`, with the optional Win32 device prefix. Supported baud values are 50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 9600, 19200, 38400, 57600, 115200 and 230400. A driver can still reject a rate accepted by argument validation.

The following example uses a placeholder device path. Replace it with the port selected for the task:

~~~zuse
serial.ports()
serial.open(path="/dev/serial/by-id/your-device", baud=115200)
serial.status()
serial.write(text="ping\n", timeout=1000)
serial.read(length=256, timeout=1000)
serial.close()
~~~

A serial read returns when data becomes available; it does not promise to fill the requested buffer. The agent supplies application framing, such as a length prefix or line terminator. An empty timeout result carries `timed_out=true`.

Serial writes advance through the remaining buffer after a short system write. Already written bytes are not sent again. A deadline or I/O failure reports the cumulative `bytes_written` count and stops subsequent program steps.

Normal close and Rust drop attempt to restore the captured configuration. Explicit close reports restoration failure; drop cannot return that error to its caller. Forced process termination cannot run Rust cleanup. The PTY acceptance verifies restoration after explicit close, normal session completion and a runtime write failure.

## USB API

USB selection requires bus number, device address, vendor ID and product ID together. The adapter rechecks the descriptor at the selected bus/address before opening the handle. Changed identity or ambiguous selection produces an error.

Opening claims one interface without requesting automatic kernel-driver detachment or changing the active configuration. A driver or application already owning the interface produces an explicit busy error. Libusb documents the relevant [device and interface operations](https://libusb.sourceforge.io/api-1.0/group__libusb__dev.html).

| Operation | Parameters and defaults |
| --- | --- |
| `usb.devices()` | Enumerates numeric descriptors without opening handles |
| `usb.open(...)` | Required: `bus`, `address`, `vendor_id`, `product_id`; optional: `interface=0` |
| `usb.status()` | Reports selected identity/interface and `kernel_driver_detached=false` |
| `usb.bulk_read(...)` / `usb.interrupt_read(...)` | Required: `endpoint`; defaults: `length=4096`, `timeout=1000` |
| `usb.bulk_write(...)` / `usb.interrupt_write(...)` | Required: `endpoint`, `base64`; default: `timeout=1000` |
| `usb.control_read(...)` | Required: `request_type`, `request`; defaults: `value=0`, `index=0`, `length=64`, `timeout=1000` |
| `usb.control_write(...)` | Required: `request_type`, `request`, `base64`; defaults: `value=0`, `index=0`, `timeout=1000` |
| `usb.close()` | Releases the interface and closes the handle and context |

Bulk and interrupt endpoint numbers are 1 through 15, with the direction bit set for reads. Decimal 1 is OUT endpoint 1; decimal 129 is IN endpoint 1. Control reads require the IN bit in `request_type`; control writes require OUT. The executor validates direction and integer widths before executing any step.

This program uses the synthetic identifiers from the acceptance fixture. Its identifiers and transfer requests are examples for that fixture:

~~~zuse
usb.devices()
usb.open(bus=7, address=9, vendor_id=4660, product_id=43981, interface=2)
usb.status()
usb.bulk_write(endpoint=1, base64="AAECAw==", timeout=1000)
usb.bulk_read(endpoint=129, length=256, timeout=1000)
usb.interrupt_write(endpoint=2, base64="BAUGBw==", timeout=1000)
usb.interrupt_read(endpoint=130, length=256, timeout=1000)
usb.control_write(request_type=64, request=90, value=4660, index=513, base64="AQID", timeout=1000)
usb.control_read(request_type=192, request=91, value=4661, index=514, length=32, timeout=1000)
usb.close()
~~~

Each bulk, interrupt or control write makes one transfer call. A short write or transfer error fails the operation; zuse does not replay the request or continue to later steps. Errors include a transferred-byte count when the API supplies one. Control-transfer failures report that completion is unknown because that API has no separate partial byte count.

Bulk and interrupt reads can return data together with `timed_out=true`, preserving bytes received before timeout. A failed control read reports unknown completion instead of inventing a partial count. See [usb.rs](../src/usb.rs) and libusb's [synchronous transfer API](https://libusb.sourceforge.io/api-1.0/group__libusb__syncio.html).

USB cleanup releases the interface before closing the handle, exiting the context and unloading the library. Handle/context cleanup continues even when release fails. Libusb documents that release resets the interface to its first alternate setting, normally alternate setting 0. This does not restore every original device setting. The adapter does not explicitly call set-configuration, set-alternate-setting or reset-device during open/close.

## Bytes, limits and time budgets

Read values share this JSON shape. `bytes` is the actual count, `base64` always contains the data, and `text` contains UTF-8 or null when decoding fails. This is an operation's value; CLI execution also wraps values in its program result:

~~~json
{
  "bytes": 5,
  "base64": "aGVsbG8=",
  "text": "hello",
  "timed_out": false
}
~~~

Native serial, bulk and interrupt buffers are limited to 65,536 bytes. Their read lengths must be at least one byte. Control data is limited to 65,535 bytes by the 16-bit length field; zero-length control data is permitted. Successful writes return `bytes_written` and `complete=true`.

The DSL separately limits each string to 65,536 bytes. Base64 expansion makes the practical binary payload in one DSL string about 48 KiB, while the native library accepts 64 KiB buffers. Prefer chunks of at most 32 KiB before encoding when the device's protocol permits chunking. USB transfer boundaries can be meaningful, so zuse does not split a transfer automatically. These limits are defined in [language.rs](../src/language.rs) and [device_io.rs](../src/device_io.rs).

Each read/write timeout must be 1 through 30,000 milliseconds. The executor adds configured device timeouts to the program's existing wait budget, whose default limit is 60,000 milliseconds. This is a conservative sum of requested budgets, not measured elapsed time. Embedded callers can configure `runtime::Options.max_wait_ms`. [runtime.rs](../src/runtime.rs) validates the entire program before execution.

Transfer timeouts do not impose a hard wall-clock deadline on every USB operation. Enumeration, open, claim, release and close have no independently enforced timeout in this adapter. Interface release is a blocking libusb call with no timeout argument; setting a transfer timeout does not bound later cleanup.

Whole-program validation rejects invalid types, directions, sizes, baud rates, payload combinations and base64 before earlier steps execute. Runtime failures, including busy interfaces and disconnection, stop subsequent steps but retain earlier effects. Bytes already delivered to a device cannot be undone by the executor.

## Acceptance evidence

The [sanitized evidence](evidence/device-io-2026-11-20.json) identifies the PTY/C ABI executable by SHA-256:

~~~
ef1394bc6947eb6085dc057a1a084ee9509c6af5ba0ed8e67d6bced8c88eaa44
~~~

The final transfer reports were written on 2026-11-19 at approximately 22:33 UTC. The evidence filename follows the release reporting date, 2026-11-20. Initial and corrected transfer harness runs used the same executable. The separate real-system enumeration used a later executable and has its own SHA-256 in the record.

| Check | Observed result |
| --- | --- |
| Linux serial PTY | 5/5 groups: all 256 byte values in both directions; raw 8N1 at 115200; 80 ms empty-read timeout; saved termios restoration |
| Serial backpressure | 15,360 of 32,768 bytes written before timeout; error count matched peer data; later write stopped; configuration restored |
| Serial preflight | 7 invalid programs rejected by both check and eval; no earlier write reached the PTY |
| Synthetic libusb C ABI | 11/11 groups: exact identity selection, busy interface, 256-byte bulk/interrupt transfers, 32-byte control transfers, argument widths and buffers |
| USB partial/error cases | Four partial-write scenarios made one transfer each; 17 bytes survived read timeout; invalid counts and unknown control completion rejected |
| USB cleanup/preflight | Release/close/exit/unload ordering, cleanup after release failure, and 11 invalid programs rejected before loading the fake library |
| Real-system enumeration | 32 serial candidate paths and 9 USB descriptors observed in read-only mode; no port open, USB claim or transfer |

The first transfer run exposed two harness assumptions. It expected all JSON on stdout, while CLI validation failures use a nonzero exit with structured JSON on stderr. It also put 65,536 raw bytes into one base64 string, exceeding the DSL string limit before the intended backpressure case could begin. The scripts now accept structured stderr only on failure and use a 32 KiB backpressure payload. Initial failure messages remain in the evidence.

The USB harness proves fake-library selection through enumeration before attempting any open or transfer test. Only its CLI child processes receive a private `LD_LIBRARY_PATH`. The C image uses system libusb declarations but contains no physical USB backend. The serial harness accepts no device-path argument and creates its own PTYs.

To reproduce the transfer checks, point the scripts at the executable being evaluated. Serial uses Python's standard library; USB additionally requires a C compiler and the libusb development header:

~~~bash
python3 tests/native_serial.py --binary target/debug/zuse --output target/native-serial
python3 tests/native_usb.py --binary target/debug/zuse --output target/native-usb
~~~

Each script writes a JSON report, and USB also records synthetic native-call traces. Physical hardware transfers and live Windows/macOS behavior remain separate verification work.
