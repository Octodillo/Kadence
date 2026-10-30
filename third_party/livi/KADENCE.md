# livi, vendored for Kadence

This is livi 0.9.0 from crates.io (upstream: https://github.com/wmedrano/livi-rs),
MIT licensed (see `LICENSE`). The workspace uses it through `[patch.crates-io]`
in the top-level `Cargo.toml`.

Changes from upstream, each with a regression test in this crate:

- `Instance::restore_state` takes the instance's worker lock, so the plugin's
  state `restore()` never runs while the worker thread is inside its `work()`
  (LV2 puts `restore()` in the Instantiation threading class). Test:
  `plugin::tests::restore_waits_for_running_work`, with the
  `tests/fixtures/worker_probe.c` plugin. Kadence issue #51.
- Control ports with the `lv2:sampleRate` property get their default, minimum
  and maximum multiplied by the sample rate in instances, so writes are no
  longer clamped to the declared fractions. `Port` has a new `sample_rate`
  field and a `Port::scaled` method; `Plugin::ports` still reports the
  declared values. Test: `plugin::tests::sample_rate_ports_scale_with_the_rate`.
  Kadence issue #51.

The tests need a C compiler, the LV2 headers and the LV2 example plugins
(Ubuntu: `lv2-dev lv2-examples mda-lv2`):

    cargo test --manifest-path third_party/livi/Cargo.toml
