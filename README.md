<div align="center">

# Graffito Editor

**A native, data-driven level editor for _Super Mario Sunshine_.**

[![CI](https://github.com/ryanbevins/Graffito-Editor/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/ryanbevins/Graffito-Editor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/JhPr3fWuy)

[Join the Graffito community on Discord](https://discord.gg/JhPr3fWuy) ·
[Project format](docs/project-format.md) ·
[Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md)

</div>

> [!WARNING]
> Graffito is an experimental development preview. There are no official
> binaries or public releases yet, and project formats and workflows may change
> before the first release. Keep backups and manually verify built stages in
> Dolphin before relying on them.

Graffito Editor is a Rust-native scene editor, asset pipeline, and managed build
system for _Super Mario Sunshine_. The current source can inspect retail stages,
create source-free custom stages, author game-backed content, build a protected
runnable copy of the game, and launch the open stage directly in Dolphin.

The editor works from a user-supplied, legally obtained game extraction. It does
not include Nintendo assets, and it never edits the extracted base game.
Projects, generated content, and runnable builds live in separate
editor-managed locations.

Format, scene, and rendering behavior are grounded in the
[Super Mario Sunshine decompilation project](https://github.com/doldecomp/sms)
and Nintendo's JSystem/GX behavior. Graffito favors typed, data-derived
authoring over hardcoded object lists or blind binary patching.

## AI-assisted development

This project is developed with the assistance of AI coding tools for research,
implementation, tests, documentation, and iteration. AI output is not treated
as authoritative: format and behavior decisions are checked against the SMS
decompilation and JSystem/GX behavior, and the maintainer remains responsible
for the project's design and published code.

## What Graffito can do today

| Area | Current source-tree support |
| --- | --- |
| **Projects and stages** | Create, open, and reopen `.sms` projects; browse retail stages in localized area folders; import legacy project folders; and create minimal source-free stages with project-owned runtime mappings. |
| **Unified Content Browser** | Search, filter, sort, favorite, and preview project stages/models alongside read-only game stages, objects, skyboxes, music, sounds, and raw game files. Grid/list views, breadcrumbs, history, and contextual actions share one browser. |
| **Scene authoring** | Place safe cataloged actors, enemies, NPCs, map objects, and Mario from typed retail-backed templates. Edit transforms with viewport gizmos, snapping, and typed inspector controls; duplicate, delete, undo, and redo while dependency records and required resources are carried with the object. |
| **Models, terrain, and collision** | Import rigid `.gltf`/`.glb` geometry into native `.smsmodel` assets, edit supported GX material and collision settings, and export models as separate runtime objects, replacement terrain, skyboxes, or constrained decomp-verified stock replacements. |
| **Routes** | Inspect and author real Sunshine rail graphs in the viewport. Create, duplicate, rename, assign, connect, split, reverse, and disconnect routes; edit one-way or bidirectional links; and bake Bezier handles into runtime nodes. |
| **NPC dialogue** | Edit resolved dialogue for talk-capable placed actors using retail BMG/SPC routing. Graffito supports per-instance copy-on-write edits, confirmed shared edits, text, known controls, choices, page breaks, voice selection, balloons, and generated talk routes. |
| **Goop** | Inspect retail pollution layers, generate playable floor layers and depth data from the final terrain, select retail-derived styles and behaviors, paint or erase in the viewport, use connected fill, and rebuild stale resources after terrain changes. Retail wall and wave layers remain read-only. |
| **Sky, lighting, and audio** | Apply complete retail skybox bundles or authored skybox models; edit stage lights and ambient colors; assign stage music; inspect point, rail, and volume audio helpers; and preview supported JAudio music and sounds directly from the selected game data. |
| **Viewport** | Preview a growing subset of J3D/GX rendering through `wgpu`, including BMD/BDL models, BMT/BTI materials and textures, supported animation formats, particles, water, goop, grass, wires, collision, and many placed actors. The editor includes game-engine-style selection, views, overlays, gizmos, and camera controls. |
| **Build and play** | Save editable drafts even when validation issues remain. **Build Game** validates the stage and creates an independent runnable `run-root`; **Launch in Editor** embeds Dolphin on Windows, while **Launch in Dolphin** opens it externally. Both can direct-boot the open stage without modifying the base extraction. |

The companion `sms-cli` package exposes the same lower-level pipeline for
inspection and automation: model import/compilation, stage creation and
upgrades, schema generation, asset discovery, validation, source-free rebuilds,
route-corpus verification, project/stage export, and Dolphin launch.

```powershell
cargo run --locked -p sms-cli -- --help
```

## How projects and builds work

A Graffito project is identified by a small, human-readable `.sms` descriptor.
It points to three deliberately separate locations:

```text
My Project.sms          Project identity and paths
My Project.smsdata/     Editable scene overlays and authored content
My Project.smsbuild/    Protected build output
  run-root/             Complete runnable game directory
```

The workflow is intentionally explicit:

1. Create or open a project and select a legally obtained extracted game root.
2. Open a retail stage or create a new source-free stage.
3. Author content through the Content Browser, viewport, outliner, and inspector.
4. Save the project at any time, including while fixing validation errors.
5. Use **Build Game** when the stage is ready for export validation.
6. Test the managed `run-root` through **Launch in Editor** or
   **Launch in Dolphin**.

Saving records editable project state; it does not produce a standalone mod.
Building creates a complete local copy containing user-owned game data, so the
managed output must not be committed or redistributed. Build ownership markers,
path-overlap checks, atomic file replacement, and rollback protect both the
project and its read-only base.

For the descriptor schema, source-free stage layout, and managed-build details,
see the [project format documentation](docs/project-format.md).

## Current limitations

- Graffito is pre-1.0 software with no compatibility guarantee or supported
  installer.
- Windows 10/11 is the primary desktop target. Core crates and desktop
  compilation receive Linux CI coverage, but embedded **Launch in Editor** is
  Windows-only.
- The viewport is an expanding J3D/GX approximation, not a full emulator.
  Unsupported renderer state, animation details, and actor behavior may be
  absent or visually different from the game.
- Model import currently targets rigid/static geometry. Skins, skeletal
  animation, and morph targets are rejected; metallic/roughness, normal, AO,
  and emissive inputs remain diagnostics rather than complete GX mappings.
- Object placement is limited to classes with safe typed templates and
  dependency closure. Graffito does not guess arbitrary factories, stock
  replacements, runtime-linked fields, or service objects.
- Dialogue authoring follows resolved talk routes; it is not a general event,
  cutscene, or SPC scripting system. Routing and presentation conditions remain
  read-only.
- Audio authoring selects and retargets supported retail music and sound data.
  Custom audio import and complete JAudio emulation are not implemented.
- Goop authoring currently edits floor layers. Retail wall and wave layers are
  preserved but read-only.
- Automated tests prove parsing, compilation, round trips, and build
  invariants—not final gameplay or visual behavior. Runtime changes still need
  manual Dolphin verification.

## Build from source

### Requirements

- Git and Rustup; the repository pins Rust **1.95.0** with Clippy and rustfmt
- Windows 10 or 11 for the primary supported desktop workflow
- A current Vulkan, DirectX 12, or OpenGL-capable GPU and driver
- A legally obtained, extracted copy of _Super Mario Sunshine_
- A local checkout of the SMS decompilation project for the complete
  decomp-derived schema and metadata workflow
- Dolphin for playtesting, and optionally `nodtool` for CLI-based disc extraction

Clone and run the normal development build:

```powershell
git clone https://github.com/ryanbevins/Graffito-Editor.git graffito-editor
cd graffito-editor
cargo run --locked --profile fast-release -p graffito-editor
```

Build without launching:

```powershell
cargo build --locked --profile fast-release -p graffito-editor
```

The executable is written to
`target\fast-release\graffito-editor.exe`. The `fast-release` profile uses Thin
LTO and incremental compilation for practical iteration.

For a fully optimized local distributable build:

```powershell
cargo build --locked --release -p graffito-editor
```

The fat-LTO release executable is written to
`target\release\graffito-editor.exe`. No official binaries are currently
published by this repository.

To reopen a project descriptor directly:

```powershell
cargo run --locked --profile fast-release -p graffito-editor -- `
  "C:\Mods\My Project.sms"
```

## Development and regression testing

Run the complete code-only repository gate:

```powershell
cargo regression --code-only
```

With an unmodified extracted US game, include the retail archive census:

```powershell
cargo regression --base-root "C:\Games\SunshineUSExport"
```

The full gate checks generated glTF fixtures, formatting, strict Clippy,
workspace tests, a release build, and source-free byte-identical rebuilds across
all 108 US retail stage archives. Retail assets are read from the supplied path
and are never copied into the repository.

The individual CI commands are:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --release -p graffito-editor
```

Automated success should always be reported separately from manual editor and
Dolphin verification.

## Workspace

| Path / package | Responsibility |
| --- | --- |
| `apps/sms-editor` / `graffito-editor` | Desktop UI, viewport, authoring tools, managed builds, and Dolphin integration |
| `apps/sms-cli` / `sms-cli` | Inspection, conversion, validation, export, and automation commands |
| `apps/xtask` / `sms-xtask` | Repository regression and generated-fixture tasks |
| `crates/sms-authoring` | Secure glTF ingestion, native model/collision authoring, and scene-instance merging |
| `crates/sms-formats` | Checked big-endian readers and semantic writers for SMS and GameCube formats |
| `crates/sms-schema` | Registries and metadata generated from the SMS decompilation source |
| `crates/sms-scene` | Editable stage documents, object/route/goop/dialogue authoring, persistence, validation, and export |
| `crates/sms-render` | Renderer-facing scene, camera, selection, and viewport support types |

## Community and contributing

Questions, development discussion, testing feedback, and project updates are
welcome in the [Graffito Discord community](https://discord.gg/JhPr3fWuy).

Before contributing, read [CONTRIBUTING.md](CONTRIBUTING.md) and run the
repository checks. Do not commit extracted game files, retail assets, disc
images, managed game trees, generated projects containing copyrighted data, or
caches.

Please report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

## Credits

Special thanks to the developers and contributors of the
[Super Mario Sunshine decompilation project](https://github.com/doldecomp/sms).
Their research and documentation make Graffito's format, scene, rendering, and
runtime work possible.

## Legal

Graffito Editor is an unofficial fan-made development project. It is not
affiliated with or endorsed by Nintendo. _Super Mario Sunshine_ and related
names are trademarks of their respective owners. Users must provide their own
legally obtained game data.

Licensed under the [MIT License](LICENSE).
