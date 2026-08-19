# Contributing

Thank you for helping improve the Raspberry Pi I²C target project.

The kernel driver is experimental and operates directly on a lightly documented
hardware peripheral. Discuss changes to FIFO servicing, interrupt behavior,
pin ownership, the UAPI, or lifecycle semantics in an issue before implementing
them. Small focused fixes and documentation corrections can go directly to a
pull request.

## Pull requests

Keep changes focused and include:

1. the controller and target models involved;
2. kernel, electrical, timing, and compatibility implications;
3. automated tests plus relevant hardware counters or scope evidence; and
4. documentation updates for externally visible behavior.

Run the Rust checks from CI and rebuild the kernel module on each affected
target model. Never distribute a kernel module built for another machine's
kernel.

By contributing, you agree that Rust and documentation contributions are
licensed under either MIT or Apache-2.0, at your option. Kernel contributions
retain the SPDX license of their source file.
