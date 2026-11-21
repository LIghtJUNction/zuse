#!/usr/bin/env python3
"""Preflight bundled examples with an explicit installed Zuse executable."""
import argparse
from pathlib import Path
import subprocess

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="zuse")
    parser.add_argument("--execute-fixtures", action="store_true",
                        help="also run owned headless-browser fixtures and the local timer/import example")
    args = parser.parse_args()
    assets = Path(__file__).resolve().parents[1] / "assets"
    runnable = ("observe-and-time.zuse", "appearance-click.zuse", "import-and-eval.zuse")
    for name in (*runnable, "functions.zuse", "wechat.zuse"):
        source = assets / name
        subprocess.run([args.binary, "check", str(source)], cwd=assets, check=True)
        if args.execute_fixtures and name in runnable:
            subprocess.run([args.binary, "run", str(source)], cwd=assets, check=True, timeout=20)
    print("Bundled examples passed preflight.")

if __name__ == "__main__":
    main()
