# Delicium

[English](README.md) · [简体中文](README.zh-CN.md) · [Latine](README.la.md)

*Delicium* is Latin for "darling, pet, beloved" — the singular of *deliciae*. The word
already carries both a single pet and a whole desktop of them, which is the whole idea.

Delicium is a Windows desktop-character runtime. A transparent, always-on-top overlay
runs animated characters in Delicium's own formats — 2D parametric models and 3D skeletal
models — without a general-purpose game engine. Rendering is wgpu, the window layer is
winit, the control panel is egui, and the simulation runs on bevy_ecs.

Windows first. Performance and visual quality come before compatibility with existing
model formats: Delicium does not import GLB or other external formats.

## What it does today

- **Transparent overlay.** Only the character's outline accepts the mouse; empty scene
  areas pass clicks through to whatever is underneath. The control window always stays
  interactive.
- **Two model families.** 2D sprite models with parametric deformation (rotation and
  warp), textures and polygon masks. 3D skeletal models with animation, GPU skinning,
  base PBR material (roughness/metallic), directional light and depth.
- **Fixed-step simulation.** 60 Hz with up to four catch-up substeps per frame, and
  interpolation on the render side.
- **Up to 64 characters**, with nearest-neighbour social responses inside a 2.5-unit
  radius, including 2D/3D cross-type pairs.
- **Autonomous wandering.** Characters walk horizontally inside the visible area of the
  scene, on an idle/walk state machine; `--no-wander` turns it off.
- **Click reaction.** Press and release in place (under 4 px) makes the character turn
  toward the click and do a short startled hop. The hop is a presentation offset: the
  simulated position does not move.
- **Tray icon.** Left click shows or hides the control window; right click opens a menu.
  Closing the control window hides it to the tray.
- **Persistent configuration** in `%APPDATA%\Delicium\config.json`: instances, positions,
  the hit-region and wandering preferences, the interface language, the default character and
  the chat endpoint are restored on the next launch.
- **Model library.** No character is wired into the engine. Five bundled models are addressed
  by id (`bangboo-eous`, `puppet2d`, `robot3d`, and textured variants), and any
  `*.dmodel.json` dropped into `%APPDATA%\Delicium\models\` becomes a library entry. `--pet
  ID|PATH` starts one of them; the controls list the library and add instances from it.
- **Two interface languages.** Simplified Chinese and English, chosen in the settings page or
  following the Windows UI language.
- **Conversation panel.** Each character has an in-memory conversation in the control window.
  Without a configured endpoint the pet answers with local lines; with an OpenAI-compatible
  endpoint configured it sends the persona, the recent turns and the new message, and shows the
  reply. Character-to-character social lines land in the same log.
- **A bundled Bangboo pet** (Eous) with `--bangboo` (an alias for `--pet bangboo-eous`);
  a second one is added from the model library in the control window. Self-made example
  assets are included.

## Install

Download `delicium-launcher.exe` from the
[latest release](https://github.com/Praefectura/Delicium/releases/latest), put it anywhere
and run it. On first start it copies itself into `%LOCALAPPDATA%\Delicium\`, so the download
location does not matter and the original file can be deleted afterwards. The launcher then
fetches the engine, verifies its SHA-256 and starts the pet; there is nothing else to
install.

## Build and run

Requires a Rust MSVC toolchain on Windows and a Visual Studio build environment.

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

## The launcher

`delicium-launcher.exe` is the stable entry point. On first start it copies itself into
`%LOCALAPPDATA%\Delicium\`, then checks GitHub Releases for a newer build, installs it if
there is one, and starts the pet. A double click goes straight to the pet — no window, no
interruption. Settings open on demand from the tray menu (`--settings` opens them directly).

The settings page carries the hit-region and wandering toggles, the interface language, the
default character (a library id) and the conversation endpoint (base URL, model, API key).
It writes only the fields it owns, through a temporary file and an atomic rename, so unknown
fields and the engine's saved scene survive; the running pet polls the file and applies
preference changes live.

The layout is `%LOCALAPPDATA%\Delicium\` with `delicium-launcher.exe`, `state.json`,
`launcher.json`, `logs\`, and one `app\<version>\delicium.exe` per installed version.
Each version gets its own directory, so an update never overwrites the build that is
currently running. The launcher replaces its own image with `ReplaceFileW`, which Windows
allows while the file is executing.

Only one engine version is kept: a new version is recorded as pending, and once it has run
long enough to count as healthy the version it replaced is deleted along with its directory.
If the new version crashes early three times it is discarded and the version that was
already running stays. A directory without a valid `.install.json` marker is never deleted,
and neither are your own models in `%APPDATA%\Delicium\models\`.

Updates are read from a fixed address
(`.../releases/latest/download/latest.json`) and verified with SHA-256 over TLS. The
manifest never supplies a URL: asset addresses are derived from the repository and the
release tag, so a compromised manifest cannot redirect a download. A failed or interrupted
update installs nothing and the running version keeps starting.

## Command line

The engine (`delicium.exe`) takes `--model`, `--pet`, `--bangboo`, `--overlay-size`,
`--exit-after`, `--no-wander`, `--backend`, the `--bench` family, and `--smoke-test`.
The launcher takes `--settings`, `--no-update`, `--engine PATH`, and forwards everything
after `--` to the engine. Full tables: [docs/USAGE.md](docs/USAGE.md).

## Testing

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
```

Two `GetGuiResources` leak tests read process-wide GDI counters, so they are `#[ignore]`d
and must run in a dedicated process invocation without other tests alongside:

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib -- --ignored --test-threads=1 repeated_builds_do_not_grow_the_process_gdi_object_count repeated_chunked_builds_do_not_grow_the_process_gdi_object_count
```

Windows GUI acceptance scripts live in `tests/` and each starts its own scene, drives
real input, and writes logs and screenshots to a fresh `.artifacts/` directory.

## Model formats

`delicium.model` JSON, extension `.dmodel.json`, strictly self-contained — no file paths,
URLs, scripts or external resources. Version `0.1` is the original colour path; `0.2`
adds embedded PNGs, UVs, base materials and optional action bindings. Unknown versions
and unknown fields are rejected. The full specification is in
[docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md) (Chinese); the English README gives only the
summary above.

## Status

Pre-release, single platform, two tagged releases (`v0.1.0`, `v0.1.1`). What has actually
been run:

- **Offline gate.** `fmt --check`, `clippy --all-targets -D warnings`, 274 library tests
  (19 ignored by design), seven non-GPU integration targets, eight hardware-GPU tests, and a
  release build: 390 passed, 0 failed. Both `GetGuiResources` leak tests pass in a dedicated
  process. Eight Windows GUI acceptance scripts pass. The launcher/settings handshake
  was proved with a cross-process probe, and version retention was exercised end to end on a
  synthetic installation root (16 assertions, including a second start).
- **Chat transport.** The conversation panel and its OpenAI-compatible client were exercised
  against a loopback HTTP fixture owned by the test, including refusal of redirects and
  error statuses. **No real third-party LLM service has been contacted**, and the optional
  API key is stored in plain text in `%APPDATA%\Delicium\config.json`.
- **Release pipeline.** Tags `v0.1.0` and `v0.1.1` each triggered
  `.github/workflows/release.yml` on `windows-latest`; both runs finished green and published
  three assets: `delicium-launcher.exe`, `delicium.exe` and `latest.json`. An anonymous
  request to `releases/latest/download/latest.json` returns the manifest, which redirects to
  `objects.githubusercontent.com` and is followed by the client's WinHTTP redirect policy.
- **Install from the published Release.** The `v0.1.0` launcher downloaded from the Release,
  pointed at an empty installation root, fetched the manifest and the engine over the
  network, verified SHA-256, installed `app/0.1.0/delicium.exe`, and ran it to a clean exit.
  The on-disk engine hash matches the manifest. A second launch reported
  `Up to date (0.1.0)` and did not reinstall.
- **Still outstanding.** That end-to-end run happened on the development machine with an
  isolated `DELICIUM_INSTALL_ROOT`, not on a pristine machine. The `v0.1.1` auto-update was
  released and verified by anonymous download, but the same end-to-end run has not been
  repeated against it: on the link available while writing this, GitHub release assets
  transferred at 50-70 KB/s, which does not finish a 25 MB engine in a reasonable time.
  Self-replacement of the launcher image has not been exercised by a release that raises
  `min_launcher_version`, because no such release exists yet. Dual-monitor acceptance is
  untested.

Treat the updater as implemented and exercised end to end once, not as battle-tested.

## What is not here yet

No GLB or other external format import; no visual editor; no IK, physics or morph targets;
wandering is horizontal only, with no vertical movement or obstacle avoidance; the only
network feature is the optional, user-configured chat endpoint — there is no store, no
account system, no payment and no SQLite in the runtime. Materials are basic GGX/Schlick
without IBL, shadows, normal maps or mipmaps. Non-Windows platforms are out of scope for the
first stage. See [docs/ROADMAP.md](docs/ROADMAP.md) for the current status list.

## Licence

MIT, © 2026 Praefectura — see [LICENSE](LICENSE). The bundled `bangboo-eous.dmodel.json`
is adapted from OSad's *Bangboo 01 - Eous*, whose original page declares CC BY 4.0;
attribution and the list of modifications are in
[assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md). Character and
trademark rights do not transfer with that model licence. This project is not affiliated
with the rights holders.

---

## Documentation

| File | Contents | Language |
|---|---|---|
| [docs/USAGE.md](docs/USAGE.md) | Build, run, every command-line flag, control window, overlay and multi-monitor behaviour | 中文 |
| [docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md) | Complete `delicium.model` specification, budgets, texture rules | 中文 |
| [docs/VERIFICATION.md](docs/VERIFICATION.md) | Test procedures and every dated acceptance record, including failed gates | 中文 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Current status, what is out of scope, licence table | 中文 |
| [assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md) | Eous asset provenance, CC BY 4.0 attribution, modifications | 中文 |

The documentation files are Chinese because they were written during development and are
moved verbatim so that no measured number is retyped. Contributions that add English or
Latin versions of them are welcome.

Delicium's measured numbers in `docs/VERIFICATION.md` are per-run results from the day
they were recorded. A later improvement does not turn a failed gate into a pass, and the
file keeps both.
