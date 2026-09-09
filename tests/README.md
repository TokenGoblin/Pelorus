# tests/

Test suites that are not unit tests of a single crate. Each directory names the
phase that creates it; empty ones are scaffolding, not neglect.

| Directory | Contents | Owning phase |
|---|---|---|
| `wpt/` | Vendored WPT subset, runner, testdriver shim | 12 |
| `compat/` | The forty-site suite and its recorded traffic archives | 0 (list), 8+ (runner) |
| `sandbox/` | Syscall denial assertions and escape attempts | 2, hardened 17 |
| `spoof/` | Address-bar and IDN spoofing corpus | 18 |
| `brand/` | The brand-leak gate | 0 |
| `fuzz/` | cargo-fuzz targets, one per parser AND per IPC message | 1 onwards |

The testdriver shim in `wpt/` and the local CA trust anchor the `compat/`
replay needs are both test-only capabilities. They live behind
`#[cfg(feature = "testing")]` and CI scans release artifacts for their symbols
(§14.4). An automation surface that reaches a user is a critical vulnerability,
not a stray flag.
