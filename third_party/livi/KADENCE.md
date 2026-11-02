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
- Dropping a `Features` stops its worker thread and joins it; upstream
  detached it and let it notice on its next 100 ms tick. Detaching a thread
  while it exits is a use-after-free in glibc's `pthread_detach` (2.39, Ubuntu
  24.04), which crashed these tests with SIGSEGV now and then, since many
  short-lived `Features` are built. The thread now parks between runs, so the
  join wakes it and returns at once. Test:
  `features::tests::dropping_features_joins_the_worker_thread`. Kadence
  issue #152.
- lilv parses Turtle with a blank node prefix that `lilv_world_blank_node_prefix`
  formats into one static buffer for every world (0.24.22, still so in 0.28).
  Two worlds parsing at once on different threads can mix up their blank
  nodes, and a plugin then loses its ports ("missing port N/M"). A
  process-wide lock (`parse_lock` in `lib.rs`) is now held while a `World` is
  built, which also reads every plugin's data files then (lilv otherwise
  reads them on the first query), and by the new `World::load_resource`.
  Callers must load data through livi, not through `World::raw`. Test:
  `tests::worlds_built_on_several_threads_read_every_port` (it failed about
  one run in six without the lock). Kadence issue #156.

The tests need a C compiler, the LV2 headers and the LV2 example plugins
(Ubuntu: `lv2-dev lv2-examples mda-lv2`):

    cargo test --manifest-path third_party/livi/Cargo.toml
