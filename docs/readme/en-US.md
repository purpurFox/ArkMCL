# ArkMCL

Rust service + CLI + Flutter frontend.

## Layout

```text
.
├── Cargo.toml          # Rust workspace
├── crates/
│   ├── core/           # Domain logic (platform-agnostic, no UI dependencies)
│   ├── l10n/           # Locale catalogs (zh-CN, en-US)
│   ├── platform/       # Path resolution and Unix JSON-RPC transport
│   ├── storage/        # Instance repository, filesystem, and symlinks
│   ├── cli/            # CLI, binary name arkmcl
│   └── service/        # Daemon, binary name arkmcl-service
├── apps/
│   └── gui/            # Flutter app (Linux only for now)
└── docs/
```

## Requirements

- Rust stable ≥ 1.85 (workspace uses edition 2024)
- Flutter stable 3.47+ (Dart 3.13+)
- Linux desktop builds require clang, cmake, ninja, and pkg-config

## Quick start

### CLI

```bash
cargo run -p arkmcl-cli -- version            # ArkMCL 0.1.0
cargo run -p arkmcl-cli -- info               # JSON: name, version, locale, socket
cargo run -p arkmcl-cli -- --lang en-US help  # English help
```

Exit codes: `0` on success, `2` on usage errors, `1` on runtime failures.
Results are written to stdout; diagnostics and errors to stderr.

### Service

```bash
# Runs in the foreground; production should use systemd or a package manager
cargo run -p arkmcl-service -- --lang en-US

# In another terminal
cargo run -p arkmcl-cli -- --lang en-US service status
cargo run -p arkmcl-cli -- --lang en-US service stop

# Instances
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --create --name "My Instance" --version 1.20.4
cargo run -p arkmcl-cli -- --lang en-US instance fabric-demo --create --name "Fabric Demo" --version 1.20.4 --loader fabric --loader-version 0.15.11
cargo run -p arkmcl-cli -- --lang en-US instance           # print global defaults
cargo run -p arkmcl-cli -- --lang en-US instance --list
cargo run -p arkmcl-cli -- --lang en-US instance my-instance          # print instance info (same as --info)
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --info
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --default
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --memory 4096 --jvm-args "-Xmx4G -Dfoo=bar"
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --version 1.20.4
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --loader fabric --loader-version 0.15.11
cargo run -p arkmcl-cli -- --lang en-US instance my-instance --loader none
cargo run -p arkmcl-cli -- --lang en-US instance --group
cargo run -p arkmcl-cli -- --lang en-US launch my-instance --prepare
cargo run -p arkmcl-cli -- --lang en-US launch my-instance --start
cargo run -p arkmcl-cli -- --lang en-US launch my-instance          # attach the console (same as --terminal)
cargo run -p arkmcl-cli -- --lang en-US launch my-instance --terminal
cargo run -p arkmcl-cli -- --lang en-US launch --list
cargo run -p arkmcl-cli -- --lang en-US launch my-instance --stop

# Accounts
cargo run -p arkmcl-cli -- --lang en-US account --list
cargo run -p arkmcl-cli -- --lang en-US account --refresh
cargo run -p arkmcl-cli -- --lang en-US account Player --add
cargo run -p arkmcl-cli -- --lang en-US account Ext --add --server https://example.com/auth
cargo run -p arkmcl-cli -- --lang en-US account --add   # Microsoft device-code login
cargo run -p arkmcl-cli -- --lang en-US account Player --info
cargo run -p arkmcl-cli -- --lang en-US account Player --default
cargo run -p arkmcl-cli -- --lang en-US account Player --remove

# Resources
cargo run -p arkmcl-cli -- --lang en-US resource --search sodium --type mods --list-limit 5
cargo run -p arkmcl-cli -- --lang en-US resource --download sodium --download-to-instance my-instance --filter-version 1.20.4 --filter-loader fabric
cargo run -p arkmcl-cli -- --lang en-US resource --list-remote --filter-vanilla stable --list-limit 5
cargo run -p arkmcl-cli -- --lang en-US resource --list-remote --filter-loader fabric --filter-version 1.20.4 --list-limit 5
cargo run -p arkmcl-cli -- --lang en-US resource --list-remote --filter-loader forge --filter-version 1.20.4
cargo run -p arkmcl-cli -- --lang en-US resource --list-remote --filter-loader neoforge --filter-version 1.20.4
```

- The socket defaults to `$XDG_RUNTIME_DIR/com.purft.arkmcl/arkmcl.sock`.
- If `XDG_RUNTIME_DIR` is missing, it falls back to
  `$XDG_STATE_HOME/com.purft.arkmcl/run/arkmcl.sock`.
- Current RPC methods: `server.info`, `server.stop`, `instance.*`, `launch.*`, `account.add/info/list/default/remove`.
- Protocol handshake: `server.info` returns `protocol` and `capabilities`; the CLI checks the major version on first connection and refuses incompatible services.

### Shell completion

```bash
# bash
arkmcl completion bash > ~/.local/share/bash-completion/completions/arkmcl

# zsh
arkmcl completion zsh > "${fpath[1]}/_arkmcl"

# fish
arkmcl completion fish > ~/.config/fish/completions/arkmcl.fish
```

Completion covers commands, flags, instance IDs/names, account names, and groups; dynamic values come from `arkmcl complete <topic>` and stay silent when the service is not running.

### Localization

- Supported locales: `zh-CN` and `en-US`.
- Precedence: `--lang` > `ARKMCL_LANG` > `LC_ALL` > `LC_MESSAGES` > `LANG`.
- Unsupported tags fall back to `en-US` with a warning on stderr.
- Catalogs live in `crates/l10n/locales/` and are embedded at build time.

### GUI

```bash
cd apps/gui
flutter run -d linux
```

Only Linux desktop is currently adapted and verified. `apps/gui/web/` is
generated by the template but is out of scope.

Linux configuration:

- Application ID (GTK/DBus): `com.purft.arkmcl` (`apps/gui/linux/CMakeLists.txt`)
- Window title: `ArkMCL`; default 1280×720, minimum 720×480
  (`apps/gui/linux/runner/my_application.cc`)

## Checks

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace

# Flutter (from apps/gui)
dart format --output=none --set-exit-if-changed .
flutter analyze
flutter test
```

## Status

Implemented:

- Rust workspace: `arkmcl-core`, `arkmcl-l10n`, `arkmcl-platform`, `arkmcl-storage`, `arkmcl-cli`, `arkmcl-service`
- CLI: `account`, `instance`, `launch`, `resource` with grouped lists, wildcards, batch confirmation, and `--json`, bilingual; auto-starts `arkmcl-service` when it is not running (`ARKMCL_NO_AUTOSTART=1` disables this; `service status`/`stop` never start it); help entries are sorted alphabetically
- Service: Unix socket + JSON-RPC with info, stop, instance CRUD, config updates, and lock checks
- Path resolution: XDG defaults, explicit environment variables, portable mode (disabled by default)
- Instance repository: JSON storage, ID allocation, config validation, atomic writes
- Time statistics: instance created/last launched/run seconds and account created/last used/run seconds, used by list sorting
- Symlinks: default links for saves, resource packs, shaders; existing paths are not overwritten; per-file mod links
- Instance lock: `.arkmcl/lock` with PID + token, stale locks reclaimed, deletion blocked while running
- Symlink probe: creates a temporary link in the instance directory; sharing is disabled when unsupported
- Metadata models and cache: `Manifest`, `Version`, `AssetIndex`, all stored under `assets/indexes/` with fallback reads for installer output
- Metadata fetching: HTTPS only, 30s timeout, 16 MiB limit; mutable metadata uses a 24h TTL with conditional requests, immutable metadata is cached permanently; network failures fall back to stale cache; `SHA-1` verification
- Fabric support: loader version list, profile fetch, and base-version merge; the client jar still points at the base version
- Forge/NeoForge: download and run the official installer; requires Java and network; artifacts land in `$MINECRAFT_HOME/versions` and `libraries`; installation is direct with no sandbox rollback
- Fabric component: `instance <id> --create --loader fabric` writes the component into `pack.json` (no library download)
- Content downloads: client jar, libraries (including Fabric Maven URLs), natives jars, and asset objects; 4 concurrent by default (1..=8), resumable, backoff retries, and progress notifications; `SHA-1` verified
- Resource downloads: mod / resourcepack / shader / datapack / world / plugin can target an instance or the global roots; modpack is global-only. Download targets live under `$MINECRAFT_HOME`; user-shared saves stay in `$ARKMCL_DATA_HOME/saves`
- CurseForge: `--api curseforge` supports search, version lookup, and downloads; requires `CURSEFORGE_API_KEY`
- Servers: `--type server`; `fabric`, `forge`, `neoforge`, and `purpur` are supported, vanilla server is used when no loader is set; Fabric downloads the official server launcher jar, Forge/NeoForge run `--installServer`; start, console, and stop are supported
- Instance defaults: id/name are `<loader>-<version>`, memory 4096 (client >= 512, server >= 256), JVM args default to Aikar's flags, group defaults to `<loader>`; `--create-defaults` controls config copying, `--move` changes the instance id
- mrpack import: `instance --import <pack.mrpack|https://...>` parses `modrinth.index.json`, extracts overrides, filters by side, downloads files, and creates an instance with the correct loader; remote sources must use HTTPS; caches are cleaned after success or failure
- Resource dependencies: Modrinth `required` dependencies are resolved recursively; `incompatible` conflicts are reported; results include `resolved`
- `--autoremove`: lists global mods/resourcepacks/shaderpacks/datapacks not referenced by any instance; `--yes` moves them to trash
- System keyring: prefers `secret-tool` and falls back to `tokens.json`
- Global defaults: stored in the `instance` field of `settings.json`; without a selector, `--memory|--version|--jvm-args|--group|--type|--loader|--create-defaults` read or write them; with a selector and no value they print the instance's current setting, while `--type` and `--create-defaults` apply to the global default only
- Natives extraction: honors `extract.exclude`, uses ZIP `enclosed_name` to block path traversal
- Launch preparation: `launch <id> --prepare` fetches metadata, merges loaders, downloads dependencies, extracts natives, and returns a launch plan
- Launch execution: `launch <id> --start`, `launch <id> --terminal`, `launch --list`, `launch <id> --stop`; the console forwards stdout/stderr and accepts stdin, exit codes and crash reports are stored on the instance, and logs go to `logs/latest.log`
- Java discovery: `JAVA*_HOME` (for example `JAVA8_HOME`, `JAVA17_HOME`), `JDK_HOME`, `PATH`, `~/.jdks`, and system JVM directories; parses major/arch/vendor and prefers an exact match, otherwise the lowest version that satisfies the requirement
- Accounts: offline (MD5 offline UUID), external auth servers, and Microsoft device-code login; `accounts.json` stores metadata only, tokens live in service memory
- Default instance: set with `instance <id> --default`; `instance --info`, `launch --start`, `launch --prepare`, and `launch --terminal` use it when the id is omitted
- Instance account: optional and not written by default; launch picks explicit account > instance account > default account
- Launch plan: platform rules, classpath, natives, JVM/game arguments, and placeholder substitution (pure logic, no process spawn)
- Unit tests plus CLI and service integration tests

Not implemented:

- Quilt component installation and version sources
- Java auto-download (Q-004 only prompts the user)
- Mirror/download-source switching (Q-006)
- Concurrent launch limit (Q-009)
- Premium-account gate for offline/external login (D-011)
- world installation
- Importing an existing instance directory (non-mrpack)
- Servers: world management and default server.properties generation
- GUI ↔ service communication; the UI still says "backend: not connected"
- Local `--list` scans global directories only and lists instances under modpacks; it does not recurse into instance mods/resourcepacks
- Package-manager distribution (Q-012)
- macOS / Windows support; Web is unverified

## License

This project is licensed under **GPL-3.0-or-later** (GNU General Public License v3.0 or later). See the root `LICENSE` file for the full text.

- No Minecraft game assets are included; you still need a legitimate copy of Minecraft to use this project.
- `Minecraft` is a trademark of Mojang Studios / Microsoft; this license grants no trademark rights.
- This project is not affiliated with Mojang Studios, Microsoft, or the Purpur project.

## Boundaries

- `arkmcl-core` does not depend on the CLI, GUI, operating system, or a
  specific transport.
- `arkmcl-l10n` only owns locale catalogs and formatting.
- `arkmcl-platform` owns paths and Unix IPC details.
- The CLI talks to the service over JSON-RPC and does not duplicate business rules.
