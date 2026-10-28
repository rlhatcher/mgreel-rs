# Development

## Layout

The repo is a Cargo workspace with a Tauri app:

- `crates/mgreel-core`: the library everything else uses.
  - `reel`, `riff`: reels in memory, WAV and `cue ` chunk reading and
    writing, resampling, limits.
  - `edit`: splice edits with undo.
  - `slice`: auto-slicing by level and spectrum, and merging short splices.
  - `card`: the card's 32 slots, moves, swaps, backups and restores.
  - `names`: reel names in `mgreel.json`.
  - `history`: backups, and the copies taken when a card mounts.
  - `sets`: whole cards kept on the computer, and imported folders.
  - `marker_files`: markers from Audacity, REAPER, Ableton Live and WAV files.
  - `options`: `options.txt`, changing only values.
- `crates/mgreel-cli`: the `mgreel` command. The package name stays
  `mgreel-rs`, which is what installs it.
- `app/`: the desktop app. `app/src-tauri` is the Rust side, with a command
  for each thing the app does; `app/src` is the SvelteKit frontend.
- `Casks/`: the Homebrew cask, so the repo works as a tap.
- `docs/`: this guide, its screenshots, and the script that takes them.

## Run and test

You need Rust (stable) and Node 24.

```bash
cd app
npm install
npm run tauri dev
```

From the repo root:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

In `app/`:

```bash
npm run check    # type-check the frontend
npm run build    # build the frontend
```

`npm run tauri build -- --bundles app,dmg` builds the app and a DMG into
`target/release/bundle/`.

## Screenshots

The images in `docs/images/` come from the app's own pages, with a made-up
card in `docs/screenshots/mock.js` standing in for a real one. Start the
frontend with `npm run dev` in `app/`, then from the repo root:

```bash
uv run --no-project --with playwright python -I docs/screenshots/capture.py
```

## Releasing

1. Set the same version in `Cargo.toml` (workspace), `app/package.json` and
   `app/src-tauri/tauri.conf.json`, commit it and push it to `main`.
2. Tag that commit and push the tag:
   `git tag v1.2.0 && git push origin v1.2.0`. Only stable versions
   release; the workflow refuses a tag such as `v1.2.0-rc.1`.
3. The Release workflow builds the DMG on an Apple Silicon runner and attaches
   it to a new GitHub release. It fails if the tag doesn't match the version in
   `tauri.conf.json`. Then it commits the new version and sha256 to
   `Casks/morphagene-reels.rb` on `main`, so `brew upgrade` picks it up. A
   failed run can be rerun, and an older release never replaces a newer one
   in the cask.

The build job has read-only access and runs the npm packages and build
scripts; only the publish job can write, and it runs no third-party code.

## Signing

The app is ad-hoc signed (`signingIdentity: "-"` in `tauri.conf.json`), not
signed with an Apple Developer ID or notarized. The cask clears the quarantine
flag Homebrew sets with a declared `postflight_steps` step, which Homebrew
allows in third-party taps. Getting into the official `homebrew/cask` would
need a notarized build.
