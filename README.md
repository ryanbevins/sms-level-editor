# SMS Level Editor

> [!WARNING]
> **This project is experimental, unfinished, and not ready for use.** There is
> no public release yet. Until a release is published, do not use this editor
> for active or production mods: its workflows, project format, parsers, and
> renderer can change without notice, and saved projects are not playable mods.

This repository is public only so people can follow development, inspect the
implementation, and contribute to the project. Its presence on GitHub should
not be treated as an end-user release, a compatibility promise, or an indication
that the editor is currently suitable for mod creation.

SMS Level Editor is a Rust-native research and development project for browsing,
previewing, and eventually editing stages from *Super Mario Sunshine*. It uses
`egui` for the desktop interface and `wgpu` for the viewport. Format and
rendering behavior is developed against the
[SMS decompilation project](https://github.com/doldecomp/sms) and Nintendo's
JSystem/GX behavior.

The editor reads data from a user-supplied, legally obtained extracted copy of
the game. It does not include Nintendo assets, and it is designed to keep the
extracted base game read-only.

## AI-Assisted Development

This project is developed with the assistance of AI coding tools. They are used
to support research, draft and revise code, write tests and documentation, and
accelerate iteration.

AI-generated output is not treated as authoritative. Formats and behavior are
checked against the SMS decompilation and Nintendo's JSystem/GX behavior, and
changes are held to the same testing and repository standards as any other
contribution. The maintainer remains responsible for the project's design,
technical decisions, and published code.

## Current Development State

The following functionality exists in the current source tree. All of it should
still be considered incomplete and experimental.

### Stage and asset inspection

- Discover stage archives under an extracted game root and browse them by stage
- Decompress Yaz0 data and mount RARC archives without extracting them in place
- Scan loose and archived stage assets, including models, textures, collision,
  animations, particles, messages, and placement data
- Parse retail JDrama placement records from `map/scene.bin` into a scene
  hierarchy with transforms and available parameters
- Generate object, parameter, asset, NPC, enemy, boss, and particle metadata
  from a local SMS decompilation checkout

### Experimental viewport

- Preview J3D BMD/BDL models, BMT material tables, and BTI textures
- Render an expanding subset of GX/J3D material behavior, including TEV,
  lighting, culling, depth, alpha comparison, blending, and texture matrices
- Preview map geometry, collision, water, pollution/goop, grass, wires, and many
  placed objects, NPCs, enemies, and bosses
- Preview supported BCK skeletal, BTK texture-SRT, BTP texture-pattern, and JPA
  particle animation data
- Preview supported asset-driven level transformations and effects
- Navigate with UE5-style eased fly movement, orbit, pan, focus, dolly, and
  right-mouse + wheel camera-speed controls
- Toggle lit, collision, and object views along with grid, collision, object
  bounds, water, goop, and effect visibility

Viewport output is not yet a complete or authoritative reproduction of the
game. Unsupported material state, animation behavior, effects, or actor logic
may be missing or visually incorrect.

### Scene editing prototype

- Browse and select placement objects through an outliner
- Place schema-discovered objects and duplicate or delete existing objects
- Edit translation, rotation, and scale through the inspector and viewport
  gizmos, with snapping support
- Use undo and redo for the currently supported object operations
- Inspect raw and decoded object parameters
- Run basic document validation and review load or validation issues in the UI

General parameter editing is not implemented yet; the inspector currently
displays most decoded parameters rather than writing them back. Terrain, model
topology, textures, materials, and other retail asset content also cannot be
authored in the editor.

### Development and diagnostic tooling

The `sms-cli` package currently provides commands to:

- extract a user-owned disc image through an external `nodtool` executable;
- generate decomp-derived schema data;
- list stages, assets, and placement objects;
- extract individual files from mounted archives;
- print stage and renderer-preview diagnostics;
- validate a parsed stage document;
- save an editor-project overlay; and
- launch Dolphin, optionally with an isolated user directory.

Run `cargo run -p sms-cli -- --help` to see the current command list and
arguments.

## Saved Projects Are Not Mods

The current **Save Project** action and the CLI's `export-project` command write
an editor-only project folder containing:

- `sms-project.toml`; and
- a JSON scene overlay under `files/editor/stages/`.

This output records the editor's current object representation. It does **not**
rewrite retail `scene.bin`, repack a RARC stage archive, patch a game image, or
produce files that Dolphin can run as a mod. The Dolphin launch helper only
starts the user-supplied game and does not apply the editor overlay.

Project output is deliberately required to live outside the extracted base game
directory. Even with that safeguard, the project format is unstable and may
change or be replaced before the first release.

## Building the Development Snapshot

Building from source is intended for contributors and people following
development. It is not an installation path for a supported editor release.

Current development requirements are:

- Windows 10 or 11 as the primary development target;
- Rust 1.95 or newer;
- a modern Vulkan, DirectX 12, or OpenGL-capable GPU;
- a user-supplied extracted copy of *Super Mario Sunshine* for stage data; and
- a local checkout of the SMS decompilation project for schema generation.

Dolphin and `nodtool` are optional and are only needed for their corresponding
development commands.

```powershell
git clone https://github.com/ryanbevins/sms-level-editor.git
cd sms-level-editor
cargo build --release -p sms-editor
```

The executable is written to `target\release\sms-editor.exe`.

Launch the development UI with its project and stage controls:

```powershell
cargo run --release -p sms-editor
```

Development and diagnostic sessions can also prefill an extracted root, decomp
root, and stage:

```powershell
cargo run --release -p sms-editor -- `
  --repo-root C:\path\to\sms-decomp `
  --base-root C:\path\to\extracted-game `
  --stage dolpic0
```

## Workspace Layout

| Package | Current responsibility |
| --- | --- |
| `sms-editor` | Desktop UI, object interactions, preview preparation, and GPU viewport |
| `sms-cli` | Extraction helpers, inspection, validation, diagnostics, project export, and Dolphin launch |
| `sms-formats` | Checked readers for SMS/GameCube formats and preservation of source bytes |
| `sms-schema` | Object and preview metadata generated from the SMS decompilation source |
| `sms-scene` | Parsed stage documents, supported object edits, validation, and editor-project persistence |
| `sms-render` | Renderer-facing scene, camera, selection, and viewport support types |

## Contributing

Development checks used by the repository are:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p sms-editor
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow. Do not
commit extracted game files, retail assets, disc images, generated editor
projects, caches, or other copyrighted game data.

## Release Status

No release has been published and no current project files or workflows are
guaranteed to remain compatible. A future release will define the first
supported installation and modding workflow. Until then, this repository should
be treated strictly as a development preview.

## Credits and Thanks

Special thanks to the developers and contributors of the
[Super Mario Sunshine decompilation project](https://github.com/doldecomp/sms).
Their painstaking research and documentation make this editor's work toward
accurate game formats, scene behavior, and rendering semantics possible.

## Legal

This is an unofficial fan-made development project. It is not affiliated with
or endorsed by Nintendo. *Super Mario Sunshine* and related names are trademarks
of their respective owners. Users must provide their own legally obtained game
data.

Licensed under the [MIT License](LICENSE).
