> **Archived: Glass v1 (Zed fork).** This repository preserves the original Glass codebase and its history. Development has moved to a complete v2 rebuild. Visit [Glass](https://app.glassapp.dev/marketing) for the current app. Releases and the historical README below belong to v1.

# Glass

Glass is a browser, code editor, and terminal in one app. Instead of switching between separate applications, everything lives in the same environment. Anyone can use the browser — developers also get an editor and terminal alongside it.

> **Glass is a fork of [Zed](https://github.com/zed-industries/zed).** We actively sync with upstream every week. Glass would not be possible without the incredible work the Zed team continues to do.

Glass is in **active development**. macOS is currently the most mature platform, and Windows development is supported.

---

### What is Glass?

- **Browser** — A full browser. Browse, stream, work — you never have to touch the editor if you don't want to.
- **Code editor** — Inherited from Zed, with significant UI changes and native macOS components.
- **Terminal** — Built into the same environment.

For developers, everything is connected. Deep integration between coding, execution, and service management is the long-term vision.

### GPUI

Zed's UI framework, [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), lives in the same repository as Zed. We separated it into its own standalone repository at **[Glass-HQ/gpui](https://github.com/Glass-HQ/gpui)** and extended it with native iOS and macOS components, making it a framework that multiple apps can build on. We are also bringing iOS support to GPUI so that apps built with it can run everywhere.

### Local GPUI Development

Production builds, CI, and releases use the pinned `Glass-HQ/gpui` revision from [`Cargo.toml`](./Cargo.toml). They do not depend on a sibling checkout.

If you are changing Glass and GPUI together, use [`script/cargo-gpui-local`](./script/cargo-gpui-local) to opt into a local override:

```sh
script/cargo-gpui-local build -p zed
script/cargo-gpui-local test -p gpui_examples
```

On Windows, use [`script/cargo-gpui-local.ps1`](./script/cargo-gpui-local.ps1):

```powershell
powershell -ExecutionPolicy Bypass -File .\script\cargo-gpui-local.ps1 build -p zed
```

The script uses `GLASS_GPUI_PATH` when it is set, and otherwise defaults to `../gpui`.

To launch Glass itself against a local GPUI checkout on Windows, use [`script/setup-windows-dev.ps1`](./script/setup-windows-dev.ps1) with `-Run`. The script calls [`script/stage-windows-cef-runtime.ps1`](./script/stage-windows-cef-runtime.ps1) to stage the CEF runtime into a stable dev directory, builds the companion CLI, and launches `zed.exe` directly. For direct Cargo commands, use [`script/cargo-gpui-local.ps1`](./script/cargo-gpui-local.ps1).

---

### Building Glass

- [Building for macOS](./docs/src/development/macos.md)
- [Building for Windows](./docs/src/development/windows.md)

For Windows development, use [`script/setup-windows-dev.ps1`](./script/setup-windows-dev.ps1) to verify prerequisites, install `ninja` if needed, stage the CEF runtime, build the companion CLI, and launch Glass.

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for ways you can contribute to Glass.

### Licensing

Glass is licensed under the [GNU General Public License v3.0 or later](./LICENSE-GPL). This is the same license used by [Zed](https://github.com/zed-industries/zed), the project Glass is forked from.

A small number of utility crates are licensed under [Apache License 2.0](./LICENSE-APACHE). See individual crate `Cargo.toml` files for details.

#### Third-party dependency compliance

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

### Acknowledgments

Glass is built on top of [Zed](https://github.com/zed-industries/zed), created by **Zed Industries, Inc.** — the team behind [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter). Their work on the editor, GPUI, and the broader ecosystem made Glass possible.
