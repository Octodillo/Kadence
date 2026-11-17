# Testing

```sh
cargo nextest run        # or: cargo test
```

CI runs the same tests; see `.github/workflows/`. Tests that need
something a bare machine lacks (a JACK server, third-party plugins, a
display) skip themselves and say so, unless a switch below makes the
missing piece an error.

## Shared helpers

`crates/daw-test-support` is a dev-dependency of the other crates. Use it
rather than writing the check again in a test:

- `no_alloc::assert_no_alloc("process", || ...)` runs real-time code with
  the allocator forbidden and fails the test if it allocates. The test
  binary installs the checker with `daw_test_support::install_checker!();`.
  Calling the `assert_no_alloc` crate directly only prints a warning in this
  workspace, so a test doing that cannot fail.
- `golden::check(dir, name, &channels, how)` compares audio with a file in
  the crate's `tests/golden/`.
- `lv2::find_bundles(search_paths, &["mda.lv2"])` finds LV2 test plugins or
  tells the test to skip.

Allocations inside a backend cycle are counted by
`daw_backend::RtAllocCounter` instead. A test that drives a backend installs
it as the global allocator and compares
`BackendStats::rt_alloc_violations` with `RtAllocCounter::CLEAN`.

## Environment switches

All start with `KADENCE_`.

| Variable | Effect |
| --- | --- |
| `KADENCE_UPDATE_GOLDEN` | Rewrite every golden audio file instead of comparing. Set it after an intended change to the audio, and check the result before committing. |
| `KADENCE_REQUIRE_LV2_PLUGINS` | Fail, rather than skip, tests that need mda-lv2 or x42-plugins. CI sets it. |
| `KADENCE_VST3_AGAIN` | Path of a built `again.vst3` from the VST3 SDK, for the tests that load a real SDK plugin. |
| `KADENCE_VST3_REQUIRE_SDK_PLUGINS` | Fail, rather than skip, those tests when the bundle is missing. The VST3 workflow sets it. |
| `KADENCE_TEST_PHYSICAL_PORTS=1` | Says the JACK server's physical ports reach no hardware (`jackd -d dummy`), so the auto-connect test may link them. |
| `KADENCE_SMOKE_OUT` | Directory in which the song smoke test keeps its project and render. |
| `KADENCE_AUDIO=stub` | Run the app without an audio server (UI tests and screenshots). |

The JACK tests are `#[ignore]`d; run them against a server with
`cargo nextest run -p daw-backend -p daw-session --run-ignored only --test-threads 1`,
or `ci/pipewire-jack-tests.sh` for a private PipeWire.
