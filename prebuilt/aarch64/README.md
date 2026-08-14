# ARM64 Rust executables

These are convenience builds of the four dependency-free Rust programs. They
contain no kernel module or Device Tree overlay.

They were built from clean Git commit
`caaf31524ccbf6c8cf9ec0151c61a6eccce822d2` on a Raspberry Pi 4 running
Raspberry Pi OS (Debian 13, ARM64), using Rust/Cargo 1.97.1 and glibc 2.41.
Building on Raspberry Pi OS provides a more conservative glibc baseline than
the Ubuntu development machines.

| File | Purpose |
| --- | --- |
| `controller` | FIFO-sized controller for the direct userspace demonstration |
| `controller-long` | Long-message controller for the kernel target driver |
| `target` | Direct `/dev/mem` FIFO-sized target demonstration |
| `target-driver` | Kernel module/overlay lifecycle and example responder |

Verify the files before use:

```sh
(cd prebuilt/aarch64 && sha256sum -c SHA256SUMS)
```

GitHub `main` remains the source of truth. Ubuntu machines should normally build
the Rust programs locally with `cargo build --release --locked`. Every target
machine must build the C kernel module locally with `make -C kernel`.
