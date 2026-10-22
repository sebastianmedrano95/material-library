# Security policy

## Scope

Material Library is a local application. Its desktop HTTP server binds to the loopback interface (`127.0.0.1`) and uses a per-process token for API requests. The MCP mode communicates over stdio and does not start the desktop HTTP server.

The scanner recursively reads directory entries and supported filenames under the root supplied by the user. It does not follow directory symlinks and does not open image data during indexing. The desktop interface streams selected source images to the browser; cloud-sync providers may hydrate online-only files. MCP results include absolute file paths from the scanned library.

## Reporting a vulnerability

Please do not include private texture paths, credentials, or personal data in a public issue. Use GitHub's private vulnerability reporting for this repository when enabled. If private reporting is unavailable, contact the maintainers privately before publishing details.
