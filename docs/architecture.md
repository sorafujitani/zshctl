# Architecture

zshctl separates its shell interface from process ownership:

- `zshctl-protocol` owns framed, versioned request and response types. It has no
  Zsh or daemon lifecycle dependency.
- `zshctl-core` owns deterministic buffer, cursor, snippet, completion, and
  preprompt transformations.
- `zshctl-config` discovers, validates, caches, and merges YAML configuration.
- `zshctl-history` owns the SQLite schema and transactional history operations.
- `zshctl-daemon` owns the per-user socket, caches, and sessions.
- `zshctl-cli` owns user-facing commands, stdout, stderr, and exit categories.

The daemon socket is deterministic for a user. Shell session IDs isolate
session-local state but never participate in daemon or socket ownership.

## Protocol policy

Protocol version 1 uses a four-byte big-endian length followed by JSON. Every
request and response contains the protocol version and request ID. Adding an
optional field or operation is additive. Removing or changing required fields,
framing, or existing semantics requires a protocol version bump. A daemon must
reject incompatible work before executing it.

Frames are limited to 1 MiB and concurrent connections are bounded. Invalid,
partial, or oversized frames close only that client connection.

## History durability

Smart History uses SQLite in WAL mode. The shared daemon serializes operations
on its connection, while SQLite's busy timeout coordinates external readers.
Schema changes run in one transaction, so a failed migration rolls back to the
prior readable schema. Imports
parse the complete input before their transaction; malformed input commits no
rows. `zshctl history integrity` exposes SQLite's integrity check. User data is
outside the versioned installation tree and is preserved by upgrade, rollback,
and uninstall.

## Runtime boundary

zshctl does not embed or spawn a JavaScript runtime. Configuration is
declarative YAML, and its parsing, validation, merge, fingerprinting, and cache
invalidation run inside the Rust daemon. Unknown completion fields are rejected
instead of being interpreted as executable hooks.

## Exit categories

- `0`: requested operation succeeded.
- `1`: validation, I/O, protocol, or internal failure; diagnostics are on stderr.
- `3`: `server status` reports a non-healthy state; status JSON remains on stdout.
