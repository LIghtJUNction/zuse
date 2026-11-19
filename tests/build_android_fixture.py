#!/usr/bin/env python3
"""Build the isolated Android fixture with SDK tools only; never contacts a device."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "tests" / "android-fixture"
OUTPUT = ROOT / "target" / "android-fixture"
PACKAGE = "best.lmm.zuse.fixture"


def run(argv: list[str | Path], *, capture: bool = False) -> str:
    command = [str(part) for part in argv]
    print("+ " + " ".join(command), flush=True)
    result = subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )
    return result.stdout if capture else ""


def executable(name: str) -> Path:
    found = shutil.which(name)
    if not found:
        raise RuntimeError(f"{name} is required on PATH")
    return Path(found)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path,
                        default=Path(os.environ.get("ANDROID_SDK_ROOT",
                                     os.environ.get("ANDROID_HOME", "/opt/android-sdk"))))
    parser.add_argument("--platform", default="android-35")
    parser.add_argument("--build-tools", default="35.0.0")
    args = parser.parse_args()
    sdk = args.sdk.resolve()
    platform = sdk / "platforms" / args.platform / "android.jar"
    tools = sdk / "build-tools" / args.build_tools
    aapt2 = tools / ("aapt2.exe" if os.name == "nt" else "aapt2")
    zipalign = tools / ("zipalign.exe" if os.name == "nt" else "zipalign")
    d8_jar = tools / "lib" / "d8.jar"
    signer_jar = tools / "lib" / "apksigner.jar"
    for path in (platform, aapt2, zipalign, d8_jar, signer_jar):
        if not path.is_file():
            raise RuntimeError(f"SDK tool or platform is missing: {path}")
    java, javac, keytool = (executable(name) for name in ("java", "javac", "keytool"))
    if (ROOT / ".git").exists():
        ignored = subprocess.run(
            ["git", "check-ignore", "-q", "target/android-fixture/debug.p12"], cwd=ROOT
        )
        if ignored.returncode != 0:
            raise RuntimeError("target/android-fixture must be ignored by Git before building")

    OUTPUT.mkdir(parents=True, exist_ok=True)
    if os.name == "posix":
        OUTPUT.chmod(0o700)
    classes, dex = OUTPUT / "classes", OUTPUT / "dex"
    for folder in (classes, dex):
        if folder.exists():
            shutil.rmtree(folder)
        folder.mkdir(mode=0o700)

    resource_apk = OUTPUT / "resources.apk"
    unsigned_apk = OUTPUT / "unsigned.apk"
    aligned_apk = OUTPUT / "aligned.apk"
    final_apk = OUTPUT / "zuse-fixture.apk"
    sources = sorted((SOURCE / "src").rglob("*.java"))
    if not sources:
        raise RuntimeError("fixture Java source is missing")

    run([aapt2, "link", "-o", resource_apk, "-I", platform,
         "--manifest", SOURCE / "AndroidManifest.xml",
         "--min-sdk-version", "26", "--target-sdk-version", "35",
         "--version-code", "1", "--version-name", "0.1.0"])
    run([javac, "-J-Xmx256m", "-encoding", "UTF-8", "--release", "8",
         "-classpath", platform, "-d", classes, *sources])
    class_files = sorted(classes.rglob("*.class"))
    # Invoke the SDK's D8 main directly to bound heap use on small developer hosts.
    run([java, "-Xms32m", "-Xmx384m", "-cp", d8_jar, "com.android.tools.r8.D8",
         "--min-api", "26", "--lib", platform, "--output", dex, *class_files])
    with zipfile.ZipFile(resource_apk) as resources, zipfile.ZipFile(
        unsigned_apk, "w", compression=zipfile.ZIP_DEFLATED
    ) as apk:
        for entry in resources.infolist():
            apk.writestr(entry, resources.read(entry.filename))
        for dex_file in sorted(dex.glob("*.dex")):
            apk.write(dex_file, dex_file.name)
    run([zipalign, "-f", "4", unsigned_apk, aligned_apk])

    # This disposable local signing identity is reused for reinstalling the same
    # fixture. Both the key and its password stay under the ignored target tree.
    keystore, password_file = OUTPUT / "debug.p12", OUTPUT / "debug.password"
    if keystore.exists() != password_file.exists():
        raise RuntimeError("incomplete fixture signing identity; remove only target/android-fixture/debug.*")
    if not keystore.exists():
        with password_file.open("x", encoding="ascii") as password:
            password.write(secrets.token_hex(24) + "\n")
        if os.name == "posix":
            password_file.chmod(0o600)
        run([keytool, "-J-Xmx128m", "-genkeypair", "-noprompt",
             "-keystore", keystore, "-storetype", "PKCS12",
             "-storepass:file", password_file,
             "-alias", "zuse-fixture", "-keyalg", "RSA", "-keysize", "2048",
             "-validity", "3650", "-dname", "CN=zuse fixture,OU=Tests,O=zuse"])
        if os.name == "posix":
            keystore.chmod(0o600)
    signer = [java, "-Xmx192m", "-jar", signer_jar]
    run([*signer, "sign", "--ks", keystore, "--ks-pass", "file:" + str(password_file),
         "--ks-key-alias", "zuse-fixture", "--v4-signing-enabled", "false",
         "--out", final_apk, aligned_apk])
    verification = run([*signer, "verify", "--verbose", final_apk], capture=True)
    print(verification, end="")
    permissions = run([aapt2, "dump", "permissions", final_apk], capture=True)
    print(permissions, end="")
    if "uses-permission:" in permissions:
        raise RuntimeError("the fixture unexpectedly requests Android permissions")
    badging = run([aapt2, "dump", "badging", final_apk], capture=True)
    print(badging, end="")
    if f"name='{PACKAGE}'" not in badging or "application-debuggable" not in badging:
        raise RuntimeError("fixture package/debuggable identity validation failed")
    with zipfile.ZipFile(final_apk) as apk:
        if "classes.dex" not in apk.namelist():
            raise RuntimeError("APK contains no compiled code")

    digest = hashlib.sha256(final_apk.read_bytes()).hexdigest()
    metadata = {
        "package": PACKAGE,
        "component": PACKAGE + "/.MainActivity",
        "apk": str(final_apk),
        "bytes": final_apk.stat().st_size,
        "sha256": digest,
        "minSdk": 26,
        "targetSdk": 35,
        "sdk": str(sdk),
        "platform": args.platform,
        "buildTools": args.build_tools,
        "permissions": [],
        "installed": False,
        "sources": {
            str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [SOURCE / "AndroidManifest.xml", *sources]
        },
    }
    (OUTPUT / "build.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    (OUTPUT / "SHA256SUMS").write_text(digest + "  " + final_apk.name + "\n", encoding="ascii")
    print(json.dumps(metadata, indent=2))
    print("Build complete. No device was contacted or APK installed.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"fixture build failed: {error}", file=sys.stderr)
        raise SystemExit(1)
