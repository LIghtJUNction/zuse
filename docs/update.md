# Updating the CLI

`zuse update` updates the Cargo installation containing the executable you invoked. It uses the adjacent `.crates2.json` record to identify the package, source and installation options. The Cargo package must be `zuse-computer`, and that record must own only the `zuse` executable. A renamed executable, another package's binary, an inconsistent version record, or ambiguous ownership stops the operation.

```sh
zuse update --check
zuse update --dry-run
zuse update
```

`--check` and `--dry-run` make no installation calls. Both return the current version, latest stable version, update availability, installation identity and a literal command array when automatic updating is supported. `--dry-run` is useful for reviewing the preserved installation options. The two flags are mutually exclusive. `--read-only` permits either inspection form and rejects a normal update.

## Sources and paths

| Installation | Stable version source | Update behavior |
| --- | --- | --- |
| Cargo from crates.io | Official `zuse-computer` API record, matching repository and non-yanked version | `cargo install zuse-computer --registry crates-io --version =VERSION --locked --bin zuse --root ORIGINAL_ROOT`, with recorded features, profile and target |
| Cargo from the official Zuse Git repository | Public, non-prerelease GitHub release; corresponding tag must exist in that repository | Cargo installs the verified `vVERSION` tag into the same root, with recorded options |
| Explicit Cargo Git branch/revision, custom registry, multiple binaries, or inconsistent tracking | Manual source selection required | Returns an error and leaves the installation unchanged |
| System prefix or package-manager location | crates.io stable metadata for the version comparison | Returns a manual-update explanation; Arch/AUR users can use their normal `paru -Syu` or `yay -Syu` workflow |
| Portable archive, source build, `--no-track` or missing modern Cargo record | crates.io stable metadata for the version comparison | Returns the official release location and a manual-update explanation |

The executable path is resolved before selecting its root. A symlink to a system installation therefore remains a system installation. The tracking file must be a regular file within that root. No `--force`, `sudo`, global installation fallback, or automatic package-manager command is used.

The updater leaves Cargo and Git configuration in place, including source replacement, caches, proxy settings and credentials. The explicit `--root` always selects the running executable's root even if `CARGO_INSTALL_ROOT` names another directory. Existing Cargo custom branches and revisions require a manual decision before changing their source policy.

Version comparison uses numeric semantic-version components. A newer development version is retained; build metadata does not trigger a reinstall. A prerelease can advance to the stable release with the same numeric version. Stable targets use the project's `MAJOR.MINOR.PATCH` release format.

## Requirements and results

`curl` must be on PATH. Applying an update also requires Cargo; Git installations require `git` to verify the official tag. Missing tools, HTTP failures and timeouts return a nonzero exit status with a JSON error. No new Rust dependency is needed for the updater.

stdout contains one JSON receipt on a completed check, plan, update or unsupported automatic update. The `status` is `up_to_date`, `update_available`, `dry_run`, `updated` or `failed`. A successful `updated` receipt names the verified installed version and sets `update_available` to false. Progress and child diagnostics go to stderr. Other errors follow the existing CLI convention: a JSON error on stderr and a nonzero exit status.

Metadata requests allow HTTPS redirects only, have a 30-second transfer limit and a 128 KiB response limit. The owned process has a 35-second outer lookup deadline. Cargo installation has a 30-minute deadline and streams its progress to stderr. After Cargo exits successfully, the updater checks the new tracking record and executes that same installed path with `--version` before reporting success.

On supported Unix builds, Ctrl+C and SIGTERM use the existing CLI cancellation guard and stop the updater's owned child processes. Cargo may already have replaced the executable when cancellation or verification fails; inspect `zuse --version` before retrying. The updater does not claim a rollback after Cargo has completed installation. Windows retains the existing native signal behavior; actual Windows self-update behavior needs its own platform acceptance.

## Local verification

```sh
cargo test --locked --bin zuse
cargo build --locked --bin zuse
python3 tests/cli_update.py --binary target/debug/zuse -v
```

The Rust cases cover version ordering, release/package identity, tracked options, unknown and symlinked metadata, literal arguments, process failure, cancellation and deadlines. The CLI fixtures copy the binary into private temporary Cargo roots and provide fake `curl`, `git` and `cargo` executables. They exercise the real command parser and updater, including simulated replacement and installed-version readback. They make no public requests, perform no actual Cargo installation, and use no desktop input.

These private fixtures validate updater behavior. Actual registry installation into a user prefix and platform-specific executable replacement are separate acceptance steps.
