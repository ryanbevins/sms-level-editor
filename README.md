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

- Create and reopen named `.sms` projects from a launch hub with a persistent
  recent-project list and native file/folder choosers
- Browse and select placement objects through an outliner
- Place schema-discovered objects and duplicate or delete existing objects
- Edit translation, rotation, and scale through the inspector and viewport
  gizmos, with snapping support
- Import rigid `.gltf` and `.glb` models into native project-owned model assets,
  edit their GX materials and collision settings, and place typed instances
- Use undo and redo for the currently supported object operations
- Inspect raw and decoded object parameters
- Run basic document validation and review load or validation issues in the UI

General parameter editing is not implemented yet; the inspector currently
displays most decoded parameters rather than writing them back. Imported rigid
model authoring is available, but general editing of retail terrain topology,
textures, materials, animation, and other retail asset content remains
incomplete.

### Development and diagnostic tooling

The `sms-cli` package currently provides commands to:

- extract a user-owned disc image through an external `nodtool` executable;
- generate decomp-derived schema data;
- list stages, assets, and placement objects;
- extract individual files from mounted archives;
- print stage and renderer-preview diagnostics;
- validate a parsed stage document;
- run a strict source-free stage-archive rebuild audit to an external path;
- apply a saved object overlay to a new rebuilt stage archive outside the base tree;
- import glTF/GLB models, compile native model assets to standalone BMD/COL,
  and create blank-stage archives;
- import a stage into a standalone typed document and rebuild it without the
  retail archive;
- save an editor-project overlay; and
- launch Dolphin, optionally with an isolated user directory.

Run `cargo run -p sms-cli -- --help` to see the current command list and
arguments.

`create-blank-stage` generates small source-free proxy BMDs for the required
coin, bottle, NormalBlock, and JuiceBlock manager closure unless an explicit
`--proxy-asset` is supplied. It rejects a Yaz0/RARC payload above the editor's
12 MiB blank-stage safety budget before writing output, leaving headroom in
Sunshine's 24 MiB MEM1.

## Projects, Model Export, and Managed Builds

The desktop editor uses a versioned, human-readable `project-name.sms` file as
the identity of a project. It records the project name, extracted base-game
root, managed project-data and build locations, last opened stage, schema
source, and optional Dolphin launch paths. The launch hub stores only a
recent-project index and reopens these descriptors; it does not duplicate
project content.

Each `.sms` descriptor points to a separate managed data folder containing:

- `sms-project.toml`;
- JSON scene overlays under `files/editor/stages/`; and
- native model assets and managed blobs under `Content/`.

The CLI's `export-project` command continues to operate directly on this managed
data folder. Existing folder-only projects can be wrapped in a `.sms` descriptor
through **Import Legacy Folder** without moving or rewriting their overlay data.
See [the project format specification](docs/project-format.md) for the version 1
schema and path-resolution rules.

This output records the editor's current object representation and typed archive
edits. The stage's semantic archive is freshly imported from the configured base
root when the stage opens; it is not cached in the project. Saving a project does
**not** rewrite retail `scene.bin`, patch a game image, or produce files that
Dolphin can run as a mod. Building is a separate, explicit action.

### Placed-model export modes

The recommended **Separate runtime object** mode keeps the retail terrain BMD
untouched. Each distinct model asset is compiled to its own
`mapobj/sms_<asset-uuid>/default.bmd` resource inside the stage RARC. The build
also adds a matching `ObjChara` record to `map/tables.bin` and one transformed
`SmJ3DAct` actor per placed instance to `map/scene.bin`. If `map/tables.bin` is
absent, the editor creates a typed one. Enabled static collision is transformed
and appended to `map/map.col`; it is world collision rather than moving
per-actor collision, and existing retail collision groups remain intact. The
exporter expands the typed `Map` triangle and grid-list capacities by the exact
authored footprint so retail runtime and moving collision keep their original
headroom, and rejects merged COL indices beyond Sunshine's signed 16-bit limit.
Standalone actors are validated with `SmJ3DAct`'s exact `0x00240000` loader
flags and a conservative 12 MiB BMD / 8 MiB TEX1 safety budget for Sunshine's
24 MiB MEM1. The compiler keeps every source-free texture in the editable
asset but emits only textures referenced by GX material slots.

**Bake as map terrain** is an explicit destructive mode. It replaces
`map/map/map.bmd` with the selected authored terrain instances instead of
creating standalone actors. Enabled collision is still appended to the world
COL. Use this only when replacing the stage terrain model is intentional.

**Replace verified stock MapObjBase** is a separate constrained workflow, not
an arbitrary-name actor path. It requires an exact decomp-derived stock resource
slot whose compiled model fallback, loader flags, collision resources, and
vertex limits are compatible with the authored asset. Shared/global resource
conflicts and unsupported slot layouts are rejected rather than guessed. The
slot's exact decomp-derived `TMapObjData::unk8` manager must already exist in
the open scene, and slots with compiled `mHold` model/joint or `mMove`
BCK/joint dependencies are excluded because replacing only their BMD/COL would
leave those dependencies unsatisfied.

### Build Game and Launch in Dolphin

**Build Game** saves the project, rebuilds the stage from semantic documents,
and prepares `<managed-build-root>/run-root/` as a complete runnable extracted
game directory. Every base file is copied independently, then the rebuilt stage
is atomically installed at its exact game-relative path.
The managed build root defaults to a `.smsbuild` sibling of the `.sms`
descriptor and is protected by a project-identity marker.

**Launch in Dolphin** performs that same freshness pass, resolves the open
archive's runtime area and scenario from the staged game's own
`files/data/stageArc.bin`, and atomically patches the managed copy at
`run-root/sys/main.dol`. The patcher
recognizes the game's post-logo transition by PowerPC behavior instead of a
regional address or known executable hash, so retail, source-built, and modded
executables retain their own code and stage mappings. Dolphin runs the launch
DOL with its normal user profile by default, preserving the user's controller
configuration, or with the project-configured Dolphin user directory when one
is set. It enters the open scene without file select, Delfino Plaza, or scenario selection. An
automatic movie is bypassed for that initial transition only; later
transitions keep normal game behavior.

When one archive has several runtime contexts, the first entry in that game's
`stageArc.bin` is used deterministically and the complete match count is
reported in the console. Subsequent builds reuse byte-identical independent
copies; changed files and the target-specific launch executable are replaced
atomically.

Both workflows reject build locations that overlap the extracted base game or
managed project data. The extracted base and its original `main.dol` remain
read-only.

The experimental `rebuild-stage` CLI command audits the binary authoring
pipeline. It imports every stage resource into typed documents, discards the
source buffers, regenerates the child files plus RARC/Yaz0 layers, and writes
only after a byte-identical second rebuild. It refuses output inside the
extracted base tree and rejects unsupported resource kinds instead of copying
payloads through.

Managed builds use the semantic archive imported when the stage was opened.
Typed transform, deletion, duplicate, resource, model, collision, and complete
JDrama insertion edits are applied before every resource and container layer is
rebuilt. Source-less palette objects and unmodeled parameter changes are
rejected instead of producing incomplete placement streams. Model geometry is
canonically relaid out when compiled. Version 3 editor projects persist typed
edits, while the semantic baseline is freshly imported when the stage opens;
the build uses that in-memory import and never rereads the retail archive path.

For a detached workflow, `import-stage-document` first proves an exact rebuild
and then creates a standalone typed JSON document whose RARC payload slots are
required to be empty. `export-stage-document` rebuilds and reparses that document
without reading the retail archive again. Loader-ignored layout values that vary
between otherwise equivalent files are represented as bounded typed
reconstruction metadata, never as an original file buffer or opaque child
payload.

"No cached bytes" does not mean "no imported data": geometry, collision,
textures, object records, compression choices, and every other varying authored
value must exist in the semantic document to reproduce the file. The strict
contract is that output bytes come from typed fields or deterministic writers;
no writer can fall back to a retained source file.

## Regression Testing

Run the complete local regression gate from the workspace root:

```powershell
cargo regression
```

This runs formatting, Clippy, every workspace test, a release build, and the
source-free byte-identical rebuild census across all 108 US retail stages,
including `test11.szs`. On Windows it prefers
`%USERPROFILE%\Downloads\SunshineUSExport` when that folder exists, then falls
back to `SMS_BASE_ROOT`. A different unmodified US extraction can be selected
explicitly with `cargo regression --base-root <path>`.

CI and machines without retail data can run `cargo regression --code-only`.
Retail assets are never copied into the repository or build output.

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

Launch the development UI at the recent-project hub:

```powershell
cargo run --release -p sms-editor
```

Pass a `.sms` descriptor to reopen it directly (the stored last stage is opened
automatically):

```powershell
cargo run --release -p sms-editor -- "C:\Mods\Isle Delfino.sms"
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
| `sms-formats` | Checked preview readers plus strict source-free semantic readers/writers for supported SMS/GameCube formats |
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
