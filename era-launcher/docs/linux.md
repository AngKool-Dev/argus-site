# Linux support (EraLauncher v0.1.12+)

EraLauncher runs on Linux starting with v0.1.12. This page covers the bits
that differ from the Windows build.

## TL;DR

```bash
# Install Temurin 21 (or any Java 21+)
sudo apt install temurin-21-jre

# Download era-launcher-x86_64-linux.tar.gz from the release page
tar -xzf era-launcher-x86_64-linux.tar.gz
./era-launcher
```

## Data directory

EraLauncher stores config, instances, and managed runtimes under the
XDG standard:

```
~/.local/share/EraLauncher/
├── config/
├── instances/<uuid>/{mods,resourcepacks,shaderpacks,saves,game,logs,backups,...}
├── cache/
└── runtimes/java21/
```

All paths use forward slashes; the launcher rewrites Windows-style paths
on Windows and passes through POSIX paths on Linux.

## Java

- **Auto-detection** globs `/usr/lib/jvm/*/bin/java`, `/usr/lib64/jvm/*/bin/java`,
  the Adoptium Temurin install root under `~/.local/share/EraLauncher/runtimes/`,
  and `$JAVA_HOME`/`$PATH` if set.
- **Provisioning**: when the launcher needs Java but can't find it, the
  **installer** downloads the matching `.tar.gz` from
  `api.adoptium.net` and unpacks it into
  `~/.local/share/EraLauncher/runtimes/java21/`. The archive's
  top-level directory is stripped so `bin/java` lands in the right place.
- **Verification**: the launcher runs `bin/java -version` and parses the
  major version. Temurin, Zulu, Liberica, and the system OpenJDK all use
  the same version-string format so the parser handles every common
  distro package.

## Self-update

The auto-updater writes a POSIX shell helper `era-launcher.update.sh`
next to the launcher binary (chmod 0755). On update:

1. The launcher downloads the new binary to `era-launcher.new` in the
   same directory.
2. The launcher spawns `setsid -f sh -c "nohup ./era-launcher.update.sh &"`,
   then exits.
3. The helper polls for the running launcher process to disappear
   (`pgrep -x era-launcher`, up to 60 × 1 s), copies the new binary in
   place, deletes the staging file, and relaunches the new launcher.
4. The helper writes either `applied` or `timeout`/`failed` to
   `era-launcher.update-result` so the next launcher run can surface
   failures via the status bar.

The helper uses the same `.update-result` marker file contract as the
Windows `.bat` so the launcher's startup cleanup code is shared between
platforms.

## What's not supported on Linux

- **Microsoft / Xbox authentication.** Mojang blocks third-party MSA flows.
  Use offline mode (the launcher ships with an offline-account picker).
- **Forge loader.** Forge requires platform-specific native helpers that
  the v0.1.12 release does not yet have. Fabric and Quilt are fully
  supported.
- **`.bat` updater.** Windows-only. POSIX uses `.update.sh` instead.

## Building from source

```bash
# Install build deps
sudo apt install build-essential librust-openssl-dev libssl-dev pkg-config

# Build
cd era-launcher
cargo build --release

# Run
./target/release/era-launcher
```

## Reporting issues

- App crashes: `~/.local/share/EraLauncher/instances/<id>/game/` (Java
  `hs_err_pid*.log`) — open them from the **CRASHES** section in the
  TUI. Press `c` to copy to clipboard (`xclip`/`xsel` required), `d` to
  delete, `o` to open the folder.
- Launcher logs: press `F1` to open the LOGS view (or `Ctrl+L` for the
  command prompt and type `log`).
- Bug reports: include `era-launcher --version`, your distro, and the
  output of `java -version`.