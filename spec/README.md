# zshctl interface specification

`manifest.json` inventories zshctl' public CLI modes, Zsh widgets, functions,
environment settings, and declarative YAML configuration fields. Its entries
are exercised by Rust unit tests and the isolated integration scripts under
`tests/`.

The Git completion catalog is a checked-in zshctl asset. Changes to that catalog
must be reviewed with the feature tests and must not silently alter the public
configuration contract.
