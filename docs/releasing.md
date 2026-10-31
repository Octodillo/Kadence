# Releasing Kadence

`.github/workflows/release.yml` builds every release artifact from a tag and
attaches them to a **draft** GitHub release. Nothing is published until a
maintainer publishes the draft by hand.

## Before tagging

The version lives in several files, and `ci/check-release-version.sh`
fails the release if any of them disagrees with the tag:

- `Cargo.toml` (`[workspace.package] version`) and `Cargo.lock`
  (run `cargo update --workspace` after changing it)
- `CMakeLists.txt` (`project(kadence VERSION …)`)
- `packaging/arch/release/PKGBUILD` (`pkgver`; reset `pkgrel` to 1)
- `packaging/fedora/kadence.spec` (`Version`, and a new `%changelog` entry)
- `data/io.github.octodillo.daw.metainfo.xml` (a new `<release>` first in
  `<releases>`, with the release date and a short description)
- `docs/releases/VERSION.md`, the release notes, used as the release's text

The metainfo screenshots point at `data/screenshots/` at the release tag,
so refresh them first if the UI changed. They are taken by the app itself,
installed or with the icon on `XDG_DATA_DIRS` so the sidebar shows it:

```sh
for scheme in BreezeLight BreezeDark; do
    KADENCE_AUDIO=stub QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QT_SCALE_FACTOR=2 \
        kadence --color-scheme "$scheme" --screenshot-dir "shots/$scheme"
done
```

Then copy `arrange-automation` (as `arrange`), `edit`, `mix-device` (as `mix`)
and `export` from each
folder to `data/screenshots/NAME-light.png` and `NAME-dark.png`.

## Trying a release

Push a tag with a suffix, such as `v0.1.0-test.1`, on the commit to try.
The workflow runs the same way and makes a draft marked as a prerelease.
Delete the draft and the tag afterwards:

```sh
gh release delete v0.1.0-test.1 --yes
git push origin :refs/tags/v0.1.0-test.1
```

## Releasing

1. Merge the version changes to `main` and wait for CI to pass.
2. Tag the merge commit and push the tag:
   `git tag -a v0.1.0 -m 'Kadence 0.1.0' && git push origin v0.1.0`.
3. The Release workflow builds the source tarball, the Arch package, the
   Fedora RPMs and the Flatpak bundle, installs and smoke-tests each, and
   makes the draft with all of them and `SHA256SUMS`. It never touches a
   release that is already published. If a job failed for a reason outside
   the code (a mirror or runner outage), run the workflow again from the
   Actions tab with the same tag: it replaces any draft for that tag with a
   new one. If the
   code needs a fix, merge it, delete the draft, move the tag to the new
   commit (`git tag -fa v0.1.0 … && git push -f origin v0.1.0`; only while
   nothing is published) and let the workflow run again.
4. Check the draft: the notes, every file, and install at least the Flatpak.
5. Publish the draft on GitHub.

After publishing, the attached `kadence-VERSION-PKGBUILD` (renamed to
`PKGBUILD`) can go to the AUR as `kadence`; its checksum already matches
the attached tarball.
