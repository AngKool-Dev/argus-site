# Platform Audit — Windows-specific call sites in EraLauncher

This document tracks every Windows-only assumption in the codebase and the
chosen `#[cfg(windows)]` / `#[cfg(not(windows))]` strategy for cross-platform
support. Updated as part of v0.1.12 (Linux support).

| File | Line(s) | Symbol | Status | Strategy |
| --- | --- | --- | --- | --- |
| `src/argus/render.rs` | 34-69 | `set_buffer_size` | guarded | `#[cfg(windows)]` real, `#[cfg(not(windows))]` no-op |
| `src/argus/render.rs` | 79-119 | `disable_maximize` | guarded | `#[cfg(windows)]` real, `#[cfg(not(windows))]` no-op |
| `src/argus/app.rs` | 2718-2760 | `win_console::hide/show` | already done | real impl on `cfg(windows)`, no-op otherwise |
| `src/argus/app.rs` | 2762-2795 | `open_in_file_manager` | already done | Windows `explorer`/`cmd`, POSIX `xdg-open`/`open` |
| `src/argus/update.rs` | 211-264 | `create_update_helper` (.bat) | replaced | Branched on `cfg(windows)`; POSIX uses `era-launcher.update.sh` |
| `src/argus/update.rs` | new | `create_update_helper` (.sh) | new | `setsid`/`nohup`/`pgrep`, same `.update-result` marker |
| `src/installer.rs` | 72-102 | `adoptium_jre_url` | extended | Added `linux-aarch64`/`linux-x64`, `.tar.gz` archive |
| `src/installer.rs` | 391-403 | `verify_java` | already POSIX-aware | uses `bin/java` on Linux, `bin/javaw.exe` on Windows |
| `src/minecraft/java.rs` | 157-191 | `candidate_paths` | extended | Linux: glob `/usr/lib/jvm/*/bin/java`, `/usr/lib64/jvm/*/bin/java`, `~/.local/share/EraLauncher/runtimes/java*/bin/java`, `PATH` probe |
| `src/minecraft/java.rs` | 193-210 | `managed_candidate_paths` | Linux-aware | Uses `~/.local/share/EraLauncher/runtimes/...` everywhere (XDG-compatible) |
| `src/launch.rs` / `src/launch/*` | n/a | child process spawn | already portable | `std::process::Command` |

## Decision log

- **`win_console` and `open_in_file_manager`**: kept as-is. They already have
  working POSIX fallbacks.
- **Console buffer resize (`set_buffer_size`)**: this is a Windows console
  window quirk — POSIX TTYs do not have a separate buffer. No-op on Linux.
- **Disable maximize (`disable_maximize`)**: same — only meaningful on
  Windows console windows. No-op on Linux.
- **`.bat` update helper**: kept verbatim for Windows users. Linux gets a
  POSIX shell helper (`era-launcher.update.sh`) with `setsid`/`nohup`/`pgrep`
  and the same `.update-result` marker contract so `app.rs` startup cleanup
  works on both platforms.
- **Adoptium URL on Linux**: now uses the `feature_releases` API which
  already returns the correct `.tar.gz` URL for `os=linux`. `verify_java`
  already runs the binary so detection works without further changes.
- **Java detection on Linux**: globs the standard locations Debian/Ubuntu
  and Fedora use (`/usr/lib/jvm`, `/usr/lib64/jvm`) plus `~/.local/share`
  for the managed runtimes. PATH probing was already in the codebase so
  `apt install temurin-21-jre` "just works".

## Out of scope

- Console-hide behaviour on Windows (`win_console::hide/show`) — kept on
  Windows, no-op elsewhere. The `enter_game_mode` function calls into it
  unconditionally but the no-op branch compiles out cleanly.
- `crossterm_winapi` / `winapi` dependency — already scoped to
  `[target.'cfg(windows)'.dependencies]`.