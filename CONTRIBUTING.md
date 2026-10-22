# Contributing

Contributions are welcome. Please open an issue to discuss larger changes before submitting a pull request.

## Development setup

- Install the stable Rust toolchain with `rustup`.
- Run `cargo fmt --check` and `cargo check --all-targets` before opening a pull request.
- Keep the dry-run scanner filename-only. Do not read or decode image contents during indexing.
- Keep MCP tools explicit about filesystem access and avoid adding write or delete operations without a clear user-directed need.
- Do not commit local paths, sample libraries, generated thumbnails, credentials, or personal asset metadata.

## Pull requests

Describe the user-facing change, the platforms affected, and how you verified it. Keep documentation and UI copy in English to match the project language.
