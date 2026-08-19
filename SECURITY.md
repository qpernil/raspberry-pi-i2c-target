# Security Policy

This repository contains an experimental out-of-tree Linux kernel driver. It is
not production-qualified and has not been validated on every Raspberry Pi,
kernel, bus topology, or electrical configuration.

## Reporting a vulnerability

Do not open a public issue for suspected memory-safety, privilege, kernel-crash,
or unintended-pin-ownership vulnerabilities. Use GitHub's private vulnerability
reporting feature for this repository. Include the board model, kernel version,
driver configuration, reproduction details, logs or counters, and any suggested
mitigation.

Ordinary protocol limitations, display glitches, and documented FIFO overruns
may be reported as normal issues when they do not expose sensitive information
or cross a privilege boundary.

## Security expectations

Load the module only on test systems where a kernel fault is acceptable. Build
it locally against the running kernel, verify the wired voltage and pin mapping,
and do not treat the example request/response protocol as authenticated or
integrity-protected.
