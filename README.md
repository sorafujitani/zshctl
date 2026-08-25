# zshctl

zshctl is a Rust-native workflow toolkit for Zsh. It provides snippets,
context-aware completion, repository navigation, and durable Smart History
through a fast per-user daemon.

## Quick start with Homebrew

Install the tagged stable release from the maintained tap:

```sh
brew install sorafujitani/tap/zshctl
```

Add the loader to `~/.zshrc`:

```zsh
source "$(brew --prefix)/share/zshctl/zshctl.zsh"
zshctl-bind-default-keys
```

Open a new shell and verify the daemon:

```sh
zshctl server status
```

The Formula installs `zshctl`, `zshctld`, the Zsh integration, `fzf`, and `ghq`.

## What it provides

- YAML snippets with placeholders and automatic first-word expansion
- Context-aware completion backed by commands and built-in Git sources
- `ghq` repository selection
- SQLite command history with scopes, redaction, import, and export
- A versioned local protocol and one deterministic per-user daemon
- Native binaries with no JavaScript runtime dependency

## Configuration

Create `~/.config/zshctl/config.yml`. For example:

```yaml
snippets:
  - name: git status
    keyword: gs
    snippet: git status

completions:
  - name: project tasks
    patterns:
      - "^just $"
    sourceCommand: "just --summary"
```

Configuration is merged from `~/.config/zshctl`, project `.zshctl`
directories, and paths selected with `ZSHCTL_HOME` or `ZSHCTL_CONFIG`.

The default bindings are:

| Key | Action |
| --- | --- |
| Space | Expand an automatic snippet, otherwise insert a space |
| Enter | Expand an automatic snippet, then accept the line |
| Tab | Open context-aware completion |
| Ctrl-R | Open Smart History |
| Ctrl-X Ctrl-S | Select and insert a snippet |
| Ctrl-X Ctrl-G | Select and enter a `ghq` repository |

Call `zshctl-bind-default-keys` only if these bindings are wanted. Individual
`zshctl-*` widgets can be bound separately.

## Other installation methods

### Nix

```sh
nix profile add github:sorafujitani/zshctl#zshctl
```

```zsh
source "$HOME/.nix-profile/share/zshctl/zshctl.zsh"
zshctl-bind-default-keys
```

The flake exposes `.#zshctl`; `.#zshctl-core` is an alias. Install `fzf` and
`ghq` separately when they are not already provided by the system or Home
Manager.

### Tagged binary release

Every `vMAJOR.MINOR.PATCH` GitHub Release contains checksummed archives for
supported macOS and Linux targets. The versioned installer and rollback flow
are documented in the [migration guide](docs/migration.md).

### Build from source

```sh
cargo build --release --bins
source "$PWD/zshctl.zsh"
zshctl-bind-default-keys
```

The minimum supported Rust version is 1.85. Zsh 5.8 or newer is the v1 shell
target.

## Operations

```sh
zshctl server start
zshctl server status
zshctl server restart
zshctl server stop
```

The socket is placed under `$ZSHCTL_RUNTIME_DIR`, then
`$XDG_RUNTIME_DIR/zshctl`, or a protected `/tmp/zshctl-UID` directory. History
is stored under the standard user data directory and is preserved on uninstall.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --locked --release --bins
```

Architecture and public behavior are documented in
[docs/architecture.md](docs/architecture.md) and
[spec/manifest.json](spec/manifest.json). Release maintainers should follow
[docs/releasing.md](docs/releasing.md); installation, migration, rollback, and
removal are covered by [docs/migration.md](docs/migration.md).
