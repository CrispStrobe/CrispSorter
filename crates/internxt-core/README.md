# internxt-core-rust

Unofficial Rust engine for [Internxt Drive](https://internxt.com): authentication,
end-to-end crypto, the Drive REST API, and fully streaming network transfers.

> Not affiliated with or endorsed by Internxt.

> Written mostly by [Claude Code](https://claude.com/claude-code), porting the
> behaviour of Internxt's official Node/TypeScript packages.

This crate is the protocol-agnostic core used by
[`internxt-cli-rust`](https://github.com/Bebbssos/internxt-cli-rust). It has no
terminal, clap, or filesystem-credential dependencies, so it works equally under a CLI,
a WebDAV/FUSE server, or a GUI. Progress reporting, 2FA, browser-open, and
refresh-warning are injected as closures/traits by the caller.

## Status

Early development. The library surface is not yet stable — expect breaking changes
between `0.x` releases.

## Features

- `fs` *(default)* — native filesystem + runtime-bound transfer helpers
  (path-based upload, multipart upload, `create_folder_with_retry`). Pulls in
  `tokio::fs` / `tokio::spawn` / `tokio::time`. Disable to build only the
  reader/writer surface (crypto, api, network, and the generic streaming
  `upload_stream_to_network` / `download_file_to_writer`).
- `thumbnails` *(default)* — image thumbnail generation (decode/resize/encode a
  300×300 PNG preview). Pulls in `image`.

## Crypto compatibility

Crypto is byte-for-byte compatible with the official Node implementation, checked
against reference test vectors (`cargo test`, no network).

## License

[MIT](LICENSE).

