# Kadence

Kadence is a digital audio workstation for KDE Plasma, written in Rust.
Its app ID is `io.github.octodillo.daw`.

- Real-time engine in plain Rust crates with no UI toolkit dependency.
- Plasma front end in Qt 6 and Kirigami through CXX-Qt, following the KDE HIG.
- Audio and MIDI through PipeWire's JACK interface (or JACK2).
- CLAP, LV2 and VST3 plugin hosting.

Kadence 0.1 can record, arrange, edit, mix and export a song. Start with
the [user guide](docs/user-guide.md). Packages for Arch, Fedora and Flatpak
are attached to each [release](https://github.com/Octodillo/Kadence/releases);
[packaging/README.md](packaging/README.md) covers building them, and
[docs/releasing.md](docs/releasing.md) making a release.

## Layout

Every crate lives under `crates/` and is picked up by the workspace
automatically. `crates/daw-core` holds the contract types the other crates
build against: time (`SampleTime`, `TickTime`, `TempoMap`), events
(`Event`, `EventList`), the graph `Node` trait, and the plugin traits
(`PluginInstance`, `PluginFormat`). Only the UI crates may depend on Qt.

## Building

```sh
cargo build
cargo nextest run   # or: cargo test
```

CI runs `cargo fmt --check`, `cargo clippy -D warnings` and the tests on
every pull request.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
