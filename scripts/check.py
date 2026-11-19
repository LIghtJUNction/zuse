#!/usr/bin/env python3
"""Local acceptance checks. No hosted workflow is triggered."""
import subprocess

def run(*command):
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)

def main():
    run("cargo", "fmt", "--all", "--", "--check")
    run("cargo", "test", "--locked")
    run("cargo", "check", "--locked", "--no-default-features", "--lib")
    targets = subprocess.check_output(["rustup", "target", "list", "--installed"], text=True)
    if "wasm32-unknown-unknown" in targets.splitlines():
        run("cargo", "check", "--locked", "--no-default-features", "--lib", "--target", "wasm32-unknown-unknown")
    run("cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings")

if __name__ == "__main__":
    main()
