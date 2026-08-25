# Installation, migration, rollback, and removal

## Install from a package manager

Homebrew and Nix are the preferred installation paths for a local machine.

The Formula is maintained in the `sorafujitani/homebrew-tap` repository:

```sh
brew tap sorafujitani/tap
brew install zshctl
```

The Nix flake provides the Rust package and Zsh integration. `fzf` and `ghq`
remain separate packages so an existing Home Manager or Nix profile can provide
them without file collisions:

```sh
nix profile add github:sorafujitani/zshctl#zshctl
```

For a Nix installation, source the loader from the default profile:

```zsh
source "$HOME/.nix-profile/share/zshctl/zshctl.zsh"
zshctl-bind-default-keys
```

## Install from a release

Download `scripts/install.sh` from the same tagged source revision, inspect it,
then run it with the tag, for example `sh install.sh v0.1.0`. Add this loader:

```zsh
source "$HOME/.local/lib/zshctl/zshctl.zsh"
```

zshctl configuration uses `ZSHCTL_*` environment variables, `zshctl-*` widgets,
and declarative YAML exclusively. No JavaScript runtime is required or started.

The client performs exactly one serialized daemon-start retry. For environments
where a background process cannot be started, set `ZSHCTL_DIRECT_FALLBACK=1`
to execute feature requests in the short-lived CLI process after that retry
fails. This degraded mode does not support daemon
lifecycle operations or cross-request caches and prints a diagnostic on stderr.

## Adopt zshctl

1. Create YAML configuration under `~/.config/zshctl` or set
   `ZSHCTL_HOME` explicitly.
2. Source the zshctl loader shown above and bind the desired `zshctl-*` widgets.
3. Run `zshctl server status` and open a nested shell. Both must report the same
   daemon PID.
4. Exercise snippet, completion, placeholder, ghq, and history bindings.

History starts in zshctl's SQLite database. NDJSON can use stdin/stdout, while
the compatible file-oriented formats use `--in` and `--out`:

```sh
zshctl history export > zshctl-history.jsonl
zshctl history import < zshctl-history.jsonl
zshctl history export --format zsh --out zsh-history.txt
zshctl history import --format zsh --in zsh-history.txt --dedupe strict
zshctl history export --format atuin-json --out atuin.jsonl
```

Imports validate the complete input before opening a transaction. Existing
databases are never silently replaced. `--dry-run` validates without inserting;
`--dedupe strict` and `--dedupe loose` skip an already-present record ID. Fish
is an export format in v1; importing Fish YAML is intentionally unsupported.

## Upgrade and rollback

The installer fully extracts and verifies a versioned release directory before
atomically switching the `current` symlink. An interrupted download or extract
therefore leaves the active version untouched. After an upgrade, restart and
inspect the build identity with `zshctl server status`. To roll back, run
`scripts/rollback.sh vPREVIOUS`, then `zshctl server restart`. Installed release
directories are retained until removal; configuration and history are never
part of the switch. Protocol incompatibility is reported before feature work
runs.

## Remove

Run `scripts/uninstall.sh`. It stops the daemon and removes installed binaries
and shell files. Configuration and history are preserved deliberately. Remove
those user-owned files separately only after making any required backup.

## Optional dependencies

`fzf` is required for interactive selection and `ghq` for repository selection.

zshctl does not register or replace `fzf-tab-complete`. fzf-tab may continue to
own normal Zsh completion, while zshctl owns only keys explicitly bound to its
widgets. If both are wanted on Tab, choose the binding explicitly instead of
depending on plugin load order.
