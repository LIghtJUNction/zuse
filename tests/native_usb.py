#!/usr/bin/env python3
"""Exercise native USB through a synthetic C libusb image, never a physical device.

The fake library uses the installed libusb header for ABI declarations but links
no libusb backend. Only each CLI subprocess receives its private LD_LIBRARY_PATH.
An enumeration-only probe must prove the fixture was loaded before any test
attempts usb.open or a transfer. Use --compile-only while the CLI is being built.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_C = r"""#define _POSIX_C_SOURCE 200809L
#include <libusb-1.0/libusb.h>
#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* This image has no USB backend and never opens a device or a device node. */
struct libusb_context { unsigned magic; };
struct libusb_device { unsigned index; };
struct libusb_device_handle { unsigned magic; unsigned device; int claimed; };
static struct libusb_device fixture_devices[] = {{0}, {1}};
static FILE *events;
static unsigned handles;

_Static_assert(sizeof(struct libusb_device_descriptor) == 18, "descriptor ABI");
_Static_assert(sizeof(ssize_t) == sizeof(void *), "device-list result ABI");

static const char *scenario(void) {
    const char *value = getenv("ZUSE_USB_FIXTURE_MODE");
    return value ? value : "success";
}
static void event(const char *name, const char *fields, ...) {
    if (!events) abort();
    fprintf(events, "{\"event\":\"%s\"", name);
    if (fields) {
        fputc(',', events);
        va_list args;
        va_start(args, fields);
        vfprintf(events, fields, args);
        va_end(args);
    }
    fputs("}\n", events);
    fflush(events);
}
static void require(int condition) {
    if (!condition) {
        event("violation", "\"reason\":\"fixture ABI or lifetime contract\"");
        abort();
    }
}
__attribute__((constructor))
static void fixture_load(void) {
    const char *enabled = getenv("ZUSE_USB_FIXTURE");
    const char *path = getenv("ZUSE_USB_FIXTURE_LOG");
    if (!enabled || strcmp(enabled, "1") || !path || !*path) _Exit(99);
    events = fopen(path, "a");
    if (!events) _Exit(99);
    event("load", "\"fixture\":\"zuse-synthetic-libusb-v1\"");
}
__attribute__((destructor))
static void fixture_unload(void) {
    if (!events) return;
    require(handles == 0);
    event("unload", NULL);
    fclose(events);
    events = NULL;
}
int LIBUSB_CALL libusb_init(libusb_context **out) {
    require(out != NULL);
    *out = calloc(1, sizeof(**out));
    if (!*out) return LIBUSB_ERROR_NO_MEM;
    (*out)->magic = 0x5a555345u;
    event("init", NULL);
    return 0;
}
void LIBUSB_CALL libusb_exit(libusb_context *context) {
    require(context && context->magic == 0x5a555345u && handles == 0);
    event("exit", NULL);
    context->magic = 0;
    free(context);
}
ssize_t LIBUSB_CALL libusb_get_device_list(libusb_context *context, libusb_device ***out) {
    require(context && context->magic == 0x5a555345u && out);
    *out = calloc(3, sizeof(**out));
    if (!*out) return LIBUSB_ERROR_NO_MEM;
    (*out)[0] = &fixture_devices[0];
    (*out)[1] = &fixture_devices[1];
    event("list", "\"count\":2");
    return 2;
}
void LIBUSB_CALL libusb_free_device_list(libusb_device **list, int unref) {
    require(list && unref == 1);
    event("free_list", "\"unref\":%d", unref);
    free(list);
}
uint8_t LIBUSB_CALL libusb_get_bus_number(libusb_device *device) {
    require(device == &fixture_devices[0] || device == &fixture_devices[1]);
    return 7;
}
uint8_t LIBUSB_CALL libusb_get_device_address(libusb_device *device) {
    require(device == &fixture_devices[0] || device == &fixture_devices[1]);
    return (uint8_t)(9 + device->index);
}
int LIBUSB_CALL libusb_get_device_descriptor(libusb_device *device,
                                           struct libusb_device_descriptor *out) {
    require((device == &fixture_devices[0] || device == &fixture_devices[1]) && out);
    memset(out, 0, sizeof(*out));
    out->bLength = sizeof(*out);
    out->bDescriptorType = LIBUSB_DT_DEVICE;
    out->bcdUSB = 0x0200;
    out->bMaxPacketSize0 = 64;
    out->idVendor = device->index ? 0x9999 : 0x1234;
    out->idProduct = device->index ? 0x0001 : 0xabcd;
    out->bcdDevice = 0x0102;
    out->bNumConfigurations = 1;
    if (!strcmp(scenario(), "invalid_descriptor")) out->bLength = 17;
    event("descriptor", "\"device\":%u", device->index);
    return 0;
}
int LIBUSB_CALL libusb_open(libusb_device *device, libusb_device_handle **out) {
    require(device == &fixture_devices[0] && out);
    *out = calloc(1, sizeof(**out));
    if (!*out) return LIBUSB_ERROR_NO_MEM;
    (*out)->magic = 0x5a555348u;
    (*out)->device = device->index;
    handles++;
    event("open", "\"bus\":7,\"address\":9,\"vendor_id\":4660,\"product_id\":43981");
    return 0;
}
static void valid_handle(libusb_device_handle *handle, int claimed) {
    require(handle && handle->magic == 0x5a555348u && handles == 1);
    if (claimed) require(handle->claimed);
}
void LIBUSB_CALL libusb_close(libusb_device_handle *handle) {
    valid_handle(handle, 0);
    event("close", NULL);
    handle->magic = 0;
    handles--;
    free(handle);
}
int LIBUSB_CALL libusb_claim_interface(libusb_device_handle *handle, int interface) {
    valid_handle(handle, 0);
    require(interface == 2 && !handle->claimed);
    event("claim", "\"interface\":%d", interface);
    if (!strcmp(scenario(), "busy")) return LIBUSB_ERROR_BUSY;
    handle->claimed = 1;
    return 0;
}
int LIBUSB_CALL libusb_release_interface(libusb_device_handle *handle, int interface) {
    valid_handle(handle, 1);
    require(interface == 2);
    event("release", "\"interface\":%d", interface);
    handle->claimed = 0;
    return !strcmp(scenario(), "release_failure") ? LIBUSB_ERROR_IO : 0;
}
static void transfer_event(const char *name, unsigned endpoint, unsigned request,
                           unsigned value, unsigned index, unsigned length,
                           unsigned timeout, int count, int code,
                           const unsigned char *data) {
    fprintf(events, "{\"event\":\"%s\",\"endpoint\":%u,\"request\":%u,"
            "\"value\":%u,\"index\":%u,\"length\":%u,\"timeout\":%u,"
            "\"transferred\":%d,\"code\":%d,\"data_hex\":\"",
            name, endpoint, request, value, index, length, timeout, count, code);
    for (unsigned i = 0; i < length && i < 256; i++) fprintf(events, "%02x", data[i]);
    fputs("\"}\n", events);
    fflush(events);
}
static int transfer(libusb_device_handle *handle, unsigned char endpoint,
                    unsigned char *data, int length, int *transferred,
                    unsigned timeout, int interrupt) {
    valid_handle(handle, 1);
    require(length >= 0 && length <= 65536 && transferred && timeout > 0);
    int input = (endpoint & LIBUSB_ENDPOINT_IN) != 0;
    const char *name = interrupt ? (input ? "interrupt_read" : "interrupt_write")
                                 : (input ? "bulk_read" : "bulk_write");
    int count = length;
    int code = 0;
    if (input) {
        for (int i = 0; i < length; i++) data[i] = (unsigned char)(interrupt ? 255 - i : i);
        if (!strcmp(scenario(), "read_timeout")) {
            count = length < 17 ? length : 17;
            code = LIBUSB_ERROR_TIMEOUT;
        } else if (!strcmp(scenario(), "invalid_count")) {
            count = length + 1;
        }
    } else if ((!interrupt && !strcmp(scenario(), "bulk_short"))
               || (interrupt && !strcmp(scenario(), "interrupt_short"))) {
        count = length / 2;
    } else if (!interrupt && !strcmp(scenario(), "bulk_timeout")) {
        count = length / 2;
        code = LIBUSB_ERROR_TIMEOUT;
    }
    *transferred = count;
    transfer_event(name, endpoint, 0, 0, 0, (unsigned)length, timeout, count, code, data);
    return code;
}
int LIBUSB_CALL libusb_bulk_transfer(libusb_device_handle *handle,
                                    unsigned char endpoint, unsigned char *data,
                                    int length, int *transferred, unsigned timeout) {
    return transfer(handle, endpoint, data, length, transferred, timeout, 0);
}
int LIBUSB_CALL libusb_interrupt_transfer(libusb_device_handle *handle,
                                         unsigned char endpoint, unsigned char *data,
                                         int length, int *transferred, unsigned timeout) {
    return transfer(handle, endpoint, data, length, transferred, timeout, 1);
}
int LIBUSB_CALL libusb_control_transfer(libusb_device_handle *handle,
                                       uint8_t request_type, uint8_t request,
                                       uint16_t value, uint16_t index,
                                       unsigned char *data, uint16_t length,
                                       unsigned timeout) {
    valid_handle(handle, 1);
    require(timeout > 0);
    int input = (request_type & LIBUSB_ENDPOINT_IN) != 0;
    int count = length;
    int code = 0;
    if (input) {
        for (unsigned i = 0; i < length; i++) data[i] = (unsigned char)(0x20u + i);
    } else if (!strcmp(scenario(), "control_short")) {
        count = length / 2;
    }
    if (!strcmp(scenario(), "control_timeout")) code = LIBUSB_ERROR_TIMEOUT;
    transfer_event(input ? "control_read" : "control_write", request_type,
                   request, value, index, length, timeout, count, code, data);
    return code ? code : count;
}
/* Any future automatic detach/configuration/reset becomes an observable failure. */
int LIBUSB_CALL libusb_detach_kernel_driver(libusb_device_handle *h, int interface) {
    (void)h; (void)interface; event("forbidden_detach", NULL); return LIBUSB_ERROR_NOT_SUPPORTED;
}
int LIBUSB_CALL libusb_set_auto_detach_kernel_driver(libusb_device_handle *h, int enable) {
    (void)h; (void)enable; event("forbidden_auto_detach", NULL); return LIBUSB_ERROR_NOT_SUPPORTED;
}
int LIBUSB_CALL libusb_set_configuration(libusb_device_handle *h, int configuration) {
    (void)h; (void)configuration; event("forbidden_configuration", NULL); return LIBUSB_ERROR_NOT_SUPPORTED;
}
int LIBUSB_CALL libusb_set_interface_alt_setting(libusb_device_handle *h, int interface, int alternate) {
    (void)h; (void)interface; (void)alternate;
    event("forbidden_alt_setting", NULL); return LIBUSB_ERROR_NOT_SUPPORTED;
}
int LIBUSB_CALL libusb_reset_device(libusb_device_handle *h) {
    (void)h; event("forbidden_reset", NULL); return LIBUSB_ERROR_NOT_SUPPORTED;
}
"""


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def compile_fixture(output):
    directory = output / "fixture"
    directory.mkdir(parents=True, exist_ok=True)
    os.chmod(directory, 0o700)
    source = directory / "libusb_fixture.c"
    library = directory / "libusb-1.0.so.0"
    source.write_text(FIXTURE_C, encoding="utf-8")
    os.chmod(source, 0o600)
    completed = subprocess.run(
        ["cc", "-std=c11", "-fPIC", "-shared", "-Wall", "-Wextra", "-Werror", "-O0",
         "-Wl,-soname,libusb-1.0.so.0", str(source), "-o", str(library)],
        capture_output=True, text=True, timeout=30,
    )
    require(completed.returncode == 0, "compile fake libusb: " + completed.stderr[:1800])
    os.chmod(library, 0o700)
    return directory, library


class Acceptance:
    def __init__(self, args, directory, library):
        self.binary = Path(args.binary).resolve()
        mode = self.binary.stat().st_mode
        require(stat.S_ISREG(mode) and not mode & (stat.S_ISUID | stat.S_ISGID),
                "test requires a regular unprivileged CLI binary")
        self.binary_hash = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.output = Path(args.output).resolve()
        self.directory = directory
        self.proven = False
        self.counter = 0
        self.results = []
        self.report = {
            "schema": 1, "platform": "linux", "backend": "synthetic-libusb-c-abi",
            "binary_sha256": self.binary_hash,
            "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
            "physical_devices_opened": False,
            "library_path_scope": "CLI child processes only",
            "tests": self.results,
        }

    @staticmethod
    def opened(vendor=0x1234, product=0xabcd, address=9):
        return (f"usb.open(bus=7, address={address}, vendor_id={vendor}, "
                f"product_id={product}, interface=2); ")

    @staticmethod
    def values(result):
        return [item["value"] for item in result.get("results", []) if "value" in item]

    @staticmethod
    def read_values(result):
        return [value for value in Acceptance.values(result) if isinstance(value, dict)
                and {"bytes", "base64", "text", "timed_out"}.issubset(value)]

    @staticmethod
    def success(code, result):
        require(code == 0 and result.get("status") != "failed",
                "expected successful fixture execution: " + json.dumps(result)[:1200])

    @staticmethod
    def failed(code, result):
        require(code != 0 or result.get("status") == "failed",
                "incomplete USB operation was reported as successful")

    def environment(self, scenario):
        self.counter += 1
        path = self.output / f"trace-{self.counter:03d}.jsonl"
        path.write_text("", encoding="utf-8")
        os.chmod(path, 0o600)
        environment = os.environ.copy()
        environment.pop("LD_PRELOAD", None)
        environment.pop("LD_AUDIT", None)
        environment["LD_LIBRARY_PATH"] = str(self.directory)
        environment["ZUSE_USB_FIXTURE"] = "1"
        environment["ZUSE_USB_FIXTURE_MODE"] = scenario
        environment["ZUSE_USB_FIXTURE_LOG"] = str(path)
        return environment, path

    def execute(self, program, scenario="success", allow_unloaded=False, probe=False):
        require(self.proven or probe, "refusing transfers before fake-library selection proof")
        require(hashlib.sha256(self.binary.read_bytes()).hexdigest() == self.binary_hash,
                "CLI changed since the fake-library selection probe")
        environment, path = self.environment(scenario)
        completed = subprocess.run([str(self.binary), "eval", program],
                                   capture_output=True, text=True, timeout=6, env=environment)
        trace = [json.loads(line) for line in path.read_text().splitlines()]
        require(not any(row["event"] == "violation" for row in trace),
                "C fixture detected an ABI or native lifetime violation")
        require(not any(row["event"].startswith("forbidden_") for row in trace),
                "adapter tried automatic detach, configuration, alternate-setting, or reset")
        if not allow_unloaded:
            require(bool(trace) and trace[0] == {
                "event": "load", "fixture": "zuse-synthetic-libusb-v1"
            }, "fake libusb was not selected; refusing further transfer tests")
            require(trace[-1]["event"] == "unload", "native library was not unloaded after cleanup")
        error_stream = not completed.stdout.strip() and completed.returncode != 0
        try:
            result = json.loads(completed.stderr if error_stream else completed.stdout)
        except ValueError as error:
            raise AssertionError("CLI did not return fixture JSON: " + completed.stderr[:400]) from error
        require(isinstance(result, dict), "CLI response is not an object")
        if error_stream:
            require(isinstance(result.get("error"), str),
                    "nonzero CLI stderr was not a structured validation error")
        return completed.returncode, result, trace

    @staticmethod
    def cleanup(trace, claimed=True):
        names = [row["event"] for row in trace]
        suffix = ["release", "close", "exit", "unload"] if claimed else ["close", "exit", "unload"]
        require(names[-len(suffix):] == suffix, "native cleanup order is incorrect")
        require(names.count("close") == 1 and names.count("exit") == 1
                and names.count("release") == int(claimed),
                "native handle/interface/context was closed more than once or leaked")

    def prove_fixture(self):
        code, result, trace = self.execute("usb.devices()", probe=True)
        self.success(code, result)
        expected = [
            {"bus": 7, "address": 9, "vendor_id": 4660, "product_id": 43981,
             "class": 0, "configurations": 1},
            {"bus": 7, "address": 10, "vendor_id": 39321, "product_id": 1,
             "class": 0, "configurations": 1},
        ]
        require(self.values(result) == [expected],
                "enumeration did not identify exactly the two synthetic USB devices")
        names = [row["event"] for row in trace]
        require("open" not in names and "claim" not in names, "enumeration opened a USB device")
        require(names[-3:] == ["free_list", "exit", "unload"],
                "enumeration cleanup order is incorrect")
        self.proven = True
        return {"devices": 2, "handles_opened": 0, "fixture_selected": True}

    def identity(self):
        for vendor, product, address in [(0x1235, 0xabcd, 9), (0x1234, 0xabce, 9),
                                         (0x1234, 0xabcd, 11)]:
            code, result, trace = self.execute(self.opened(vendor, product, address) + "usb.status()")
            self.failed(code, result)
            names = [row["event"] for row in trace]
            require("open" not in names and "claim" not in names,
                    "USB handle was opened before exact bus/address/VID/PID matching")
            require(names[-3:] == ["free_list", "exit", "unload"],
                    "rejected selection did not free inventory and context")
        return {"mismatched_vendor_product_address_cases": 3, "handles_opened": 0}

    def busy(self):
        code, result, trace = self.execute(self.opened() + "usb.status()", "busy")
        self.failed(code, result)
        require("busy" in json.dumps(result).lower(), "busy claim was not reported")
        self.cleanup(trace, claimed=False)
        return {"automatic_detach": False, "released_unclaimed_interface": False}

    def transfers(self):
        data = bytes(range(256))
        reverse = data[::-1]
        control = bytes(range(32))
        encoded = lambda value: json.dumps(base64.b64encode(value).decode("ascii"))
        program = (
            self.opened() + "usb.status(); "
            + f"usb.bulk_write(endpoint=1, base64={encoded(data)}, timeout=73); "
            + "usb.bulk_read(endpoint=129, length=256, timeout=73); "
            + f"usb.interrupt_write(endpoint=2, base64={encoded(reverse)}, timeout=73); "
            + "usb.interrupt_read(endpoint=130, length=256, timeout=73); "
            + f"usb.control_write(request_type=64, request=90, value=4660, index=513, "
              f"base64={encoded(control)}, timeout=73); "
            + "usb.control_read(request_type=192, request=91, value=4661, index=514, length=32, timeout=73); "
            + "usb.close()"
        )
        code, result, trace = self.execute(program)
        self.success(code, result)
        reads = self.read_values(result)
        expected = [data, reverse, bytes(range(0x20, 0x40))]
        require(len(reads) == 3, "missing bulk, interrupt, or control read result")
        for value, payload in zip(reads, expected):
            require(value["bytes"] == len(payload)
                    and base64.b64decode(value["base64"], validate=True) == payload
                    and value["timed_out"] is False, "native read payload/count/timeout mismatch")
        require(reads[0]["text"] is None and reads[1]["text"] is None
                and reads[2]["text"] == expected[2].decode(),
                "binary/UTF-8 read text representation is incorrect")
        status = [value for value in self.values(result)
                  if isinstance(value, dict) and "kernel_driver_detached" in value]
        require(bool(status) and all(value["kernel_driver_detached"] is False for value in status),
                "status did not preserve the no-detach contract")
        operations = [row for row in trace if row["event"] in
                      {"bulk_write", "bulk_read", "interrupt_write", "interrupt_read",
                       "control_write", "control_read"}]
        require([row["event"] for row in operations] ==
                ["bulk_write", "bulk_read", "interrupt_write", "interrupt_read",
                 "control_write", "control_read"], "transfer order or operation dispatch is incorrect")
        require([row["endpoint"] for row in operations] == [1, 129, 2, 130, 64, 192]
                and all(row["timeout"] == 73 for row in operations),
                "FFI endpoint/direction/timeout arguments changed")
        require(operations[0]["data_hex"] == data.hex()
                and operations[2]["data_hex"] == reverse.hex()
                and operations[4]["data_hex"] == control.hex(),
                "C ABI did not receive the complete write buffers")
        require([(row["request"], row["value"], row["index"]) for row in operations[-2:]]
                == [(90, 4660, 513), (91, 4661, 514)],
                "control request/value/index ABI changed")
        self.cleanup(trace)
        return {"bulk_bytes_each_direction": 256, "interrupt_bytes_each_direction": 256,
                "control_bytes_each_direction": 32, "argument_abi_verified": True}

    def partial_writes(self):
        payload = base64.b64encode(bytes(range(256))).decode()
        cases = [
            ("bulk_short", f'usb.bulk_write(endpoint=1, base64="{payload}", timeout=73)', "bulk_write"),
            ("bulk_timeout", f'usb.bulk_write(endpoint=1, base64="{payload}", timeout=73)', "bulk_write"),
            ("interrupt_short", f'usb.interrupt_write(endpoint=2, base64="{payload}", timeout=73)', "interrupt_write"),
            ("control_short", f'usb.control_write(request_type=64, request=90, base64="{payload}", timeout=73)', "control_write"),
        ]
        for scenario, operation, name in cases:
            code, result, trace = self.execute(
                self.opened() + operation + "; usb.control_read(request_type=192, request=99, length=1, timeout=73)",
                scenario,
            )
            self.failed(code, result)
            text = json.dumps(result)
            require("bytes_written=128" in text and "not retried" in text,
                    "partial write error omitted its actual byte count or no-retry status")
            transfers = [row for row in trace if row["event"] in
                         {"bulk_write", "interrupt_write", "control_write", "control_read"}]
            require(len(transfers) == 1 and transfers[0]["event"] == name,
                    "partial write was retried or a later step executed")
            self.cleanup(trace)
        return {"short_or_timed_out_write_cases": len(cases),
                "transfer_calls_per_failure": 1, "later_steps_executed": 0}

    def partial_read_timeout(self):
        code, result, trace = self.execute(
            self.opened() + "usb.bulk_read(endpoint=129, length=256, timeout=73); usb.close()",
            "read_timeout",
        )
        self.success(code, result)
        values = self.read_values(result)
        require(len(values) == 1 and values[0]["bytes"] == 17 and values[0]["timed_out"] is True
                and base64.b64decode(values[0]["base64"], validate=True) == bytes(range(17)),
                "timeout discarded partial bytes or reported false completion")
        require(sum(row["event"] == "bulk_read" for row in trace) == 1,
                "partial read timeout unexpectedly retried")
        self.cleanup(trace)
        return {"partial_bytes": 17, "timed_out": True, "transfer_calls": 1}

    def invalid_count(self):
        code, result, trace = self.execute(
            self.opened() + "usb.bulk_read(endpoint=129, length=16, timeout=73); "
            "usb.control_read(request_type=192, request=99, length=1, timeout=73)",
            "invalid_count",
        )
        self.failed(code, result)
        require("invalid byte count" in json.dumps(result), "invalid native byte count was accepted")
        require(not any(row["event"] == "control_read" for row in trace),
                "execution continued after an impossible FFI count")
        self.cleanup(trace)
        return {"impossible_count_rejected": True, "later_step_stopped": True}

    def control_timeout(self):
        for operation in [
            "usb.control_read(request_type=192, request=90, length=16, timeout=73)",
            'usb.control_write(request_type=64, request=90, base64="AQID", timeout=73)',
        ]:
            code, result, trace = self.execute(
                self.opened() + operation + "; usb.bulk_read(endpoint=129, length=1, timeout=73)",
                "control_timeout",
            )
            self.failed(code, result)
            require("completion is unknown" in json.dumps(result),
                    "control timeout claimed a known transfer completion")
            require(not any(row["event"] == "bulk_read" for row in trace),
                    "execution continued after uncertain control completion")
            self.cleanup(trace)
        return {"read_and_write_timeout_cases": 2, "completion_reported_unknown": True}

    def release_failure(self):
        code, result, trace = self.execute(self.opened() + "usb.close()", "release_failure")
        self.failed(code, result)
        require("release USB interface" in json.dumps(result), "release failure was hidden")
        self.cleanup(trace)
        return {"release_failure_reported": True, "handle_context_and_library_closed": True}

    def implicit_drop(self):
        code, result, trace = self.execute(self.opened() + "usb.status()")
        self.success(code, result)
        self.cleanup(trace)
        return {"explicit_close": False, "release_close_exit_unload": True}

    def preflight(self):
        suffixes = [
            "usb.bulk_read(endpoint=1, length=1, timeout=10)",
            'usb.bulk_write(endpoint=129, base64="AA==", timeout=10)',
            "usb.interrupt_read(endpoint=128, length=1, timeout=10)",
            "usb.bulk_read(endpoint=129, length=0, timeout=10)",
            "usb.bulk_read(endpoint=129, length=65537, timeout=10)",
            "usb.bulk_read(endpoint=129, length=1, timeout=0)",
            "usb.control_read(request_type=0, request=90, length=1, timeout=10)",
            'usb.control_write(request_type=128, request=90, base64="AA==", timeout=10)',
            "usb.control_read(request_type=192, request=90, length=65536, timeout=10)",
            'usb.bulk_write(endpoint=1, base64="AA?=", timeout=10)',
            'usb.bulk_write(endpoint=1, base64="AB==", timeout=10)',
        ]
        for index, suffix in enumerate(suffixes):
            program = (self.opened()
                       + 'usb.bulk_write(endpoint=1, base64="AQID", timeout=10); ' + suffix)
            source = self.output / f"preflight-{index}.zuse"
            try:
                source.write_text(program + "\n", encoding="utf-8")
                os.chmod(source, 0o600)
                environment, path = self.environment("success")
                checked = subprocess.run([str(self.binary), "check", str(source)],
                                         capture_output=True, text=True, timeout=5, env=environment)
                require(checked.returncode != 0 and path.read_text() == "",
                        "check accepted invalid arguments or loaded a native library")
                code, result, trace = self.execute(program, allow_unloaded=True)
                self.failed(code, result)
                require(not trace, "whole-program rejection happened after a native USB effect")
            finally:
                source.unlink(missing_ok=True)
        return {"invalid_cases": len(suffixes), "libraries_loaded": 0, "prior_transfers": 0}

    def record(self, name, function):
        try:
            item = {"name": name, "status": "passed", "detail": function()}
        except Exception as error:
            item = {"name": name, "status": "failed", "error": str(error)[:1800]}
        self.results.append(item)
        print(json.dumps(item), flush=True)
        return item["status"] == "passed"

    def run(self):
        try:
            if self.record("enumeration_only_proves_fake_library_selection", self.prove_fixture):
                for name, function in [
                    ("bus_address_and_vid_pid_locked_before_open", self.identity),
                    ("busy_interface_does_not_detach", self.busy),
                    ("bulk_interrupt_control_ffi_payloads", self.transfers),
                    ("partial_writes_not_retried_and_stop_following_steps", self.partial_writes),
                    ("read_timeout_preserves_partial_bytes", self.partial_read_timeout),
                    ("invalid_native_count_is_rejected", self.invalid_count),
                    ("control_timeout_completion_unknown", self.control_timeout),
                    ("release_failure_still_closes_handle_context_library", self.release_failure),
                    ("implicit_drop_releases_all_resources", self.implicit_drop),
                    ("whole_program_preflight_has_no_prior_effects", self.preflight),
                ]:
                    self.record(name, function)
        finally:
            self.report["fixture_selection_proven"] = self.proven
            self.report["passed"] = sum(row["status"] == "passed" for row in self.results)
            self.report["failed"] = sum(row["status"] == "failed" for row in self.results)
            path = self.output / "report.json"
            path.write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")
            os.chmod(path, 0o600)
        return int(bool(self.report["failed"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="newly built zuse CLI")
    parser.add_argument("--output", default=str(ROOT / "target/native-usb"))
    parser.add_argument("--compile-only", action="store_true", help="build fixture without loading it")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this fixture uses Linux child-process dynamic-library selection")
    if not args.compile_only and not args.binary:
        parser.error("--binary is required unless --compile-only is selected")
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    os.chmod(output, 0o700)
    directory, library = compile_fixture(output)
    if args.compile_only:
        print(json.dumps({"fixture_compiled": True, "fixture_loaded": False,
                          "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest()}))
        return 0
    return Acceptance(args, directory, library).run()


if __name__ == "__main__":
    raise SystemExit(main())
