# ARM64 Rust executables

These are release builds of the five crate-dependency-free Rust programs.
They contain no kernel module or Device Tree overlay. All five were built from
clean source commit `925d083e4969d4f19acf79fc3cdb13fda4cab512` on `ubuntu4`,
Ubuntu 26.04.1 LTS ARM64, with Rust/Cargo 1.98.1 and glibc 2.43.

| File | Purpose | Maximum required glibc |
| --- | --- | --- |
| `controller` | FIFO-sized controller for the direct userspace demonstration | `GLIBC_2.34` |
| `controller-long` | Long-message controller for the kernel target driver | `GLIBC_2.34` |
| `target` | Direct `/dev/mem` FIFO-sized target demonstration | `GLIBC_2.34` |
| `target-driver` | Kernel lifecycle, READY echo and receive-only diagnostics | `GLIBC_2.39` |
| `virtual-display` | SSD1306/SH1106 SDL viewer with default GPIO5/GPIO26 outputs | `GLIBC_2.39` |

The executables are compatible with Raspberry Pi OS Debian 13 using glibc 2.41.
Verify them before use:

```sh
(cd prebuilt/aarch64 && sha256sum -c SHA256SUMS)
```

On a capable ARM64 machine, rebuild with `cargo build --release --locked --bins`.
Build the C kernel module locally on each target with `make -C kernel`; kernel
modules and overlays are not distributed as prebuilts. Build Rust artifacts on
Ubuntu4 rather than the memory-constrained Raspberry Pi 1 and 2 lab hosts,
then publish the binaries, checksums and provenance through the canonical Git
repository and deploy through Git.
