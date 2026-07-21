# Graffito-Editor Project Format

> [!WARNING]
> This format is experimental and may change before the first public release.

A Graffito-Editor project is identified by one UTF-8 TOML descriptor with the
`.sms` extension. The descriptor is intentionally small and contains no retail
game assets. It points to an extracted, read-only copy of *Super Mario
Sunshine* and to a separate managed project-data folder.

## Version 1

```toml
format_version = 1
kind = "sms-editor-project"
name = "Isle Delfino"
project_id = "019f..."
created_with = "0.1.0"
base_game_root = "C:\\Games\\SunshineJPExtract"
project_data_root = "Isle Delfino.smsdata"
managed_build_root = "Isle Delfino.smsbuild"
schema_source_root = "C:\\src\\sms"
last_stage = "dolpic0"

[stage_music.dolpic0]
bgm_id = 0x80010002
wave_scene_id = 0x202
secondary_bgm_id = 0x80010023
secondary_wave_scene_id = 0x204

[sound_assignments."map_static:SoundObjRiver"]
kind = "map_static"
source_name = "SoundObjRiver"
original_sound_id = 0x500F
sound_id = 0x5000

[launch]
dolphin_executable = "C:\\Tools\\Dolphin\\Dolphin.exe"
game_image = "D:\\Games\\Super Mario Sunshine.rvz"
dolphin_user_directory = "C:\\DolphinProfiles\\SMS-Modding"
```

Required fields are:

- `format_version`: currently `1`;
- `kind`: always `sms-editor-project` (retained as a compatibility identifier);
- `name`: the user-facing project name;
- `project_id`: a stable identity generated when the descriptor is created;
- `created_with`: the editor version that created the descriptor;
- `base_game_root`: the extracted game directory, which remains read-only; and
- `project_data_root`: the directory containing editor-owned overlay data.

`managed_build_root`, `schema_source_root`, `last_stage`, and every value under
`launch` are optional. `stage_music` is also optional and stores the selected
decomp-derived BGM, matching wave-scene identifier, and optional secondary
crossfade BGM plus its own wave-scene identifier by stage ID. `sound_assignments` stores named map-static or graph
emitter overrides; these are global runtime table bindings rather than
per-placement values. Music preview uses the actual JAudio resources in the
selected base game rather than storing substitute clips. The same entry shape
is used for retail and source-free custom stages. The editor updates `last_stage` after a stage opens so
reopening the project can restore the working context. When
`managed_build_root` is omitted, it defaults to a `.smsbuild` sibling of the
descriptor.

## Path resolution

`project_data_root` and `managed_build_root` may be absolute or relative. A
relative value is resolved against the directory containing the `.sms`
descriptor and cannot contain parent-directory traversal. New projects use a
sibling `<name>.smsdata` folder and, by default, a sibling `<name>.smsbuild`
folder. All other stored paths are explicit filesystem locations chosen through
native file or folder dialogs.

Project data and managed builds must not overlap each other or the extracted
base-game directory. The descriptor must also remain outside the managed build
root. The managed data folder retains the transactional `sms-project.toml`
manifest, `Content/` assets, and `files/` overlay tree, including its base-game
identity and lossless-save safeguards.

## Source-free authored stages

The experimental **Create New Stage** workflow operates inside an open project
through **File > New Stage...** or **+ New Stage** in the Stages content browser.
Creation accepts a unique stage ID and creates a minimal source-free scene with
an internal runtime placeholder terrain. It does not require a world model,
insert Mario, select a skybox, or select a retail lighting source.

The editor derives a project-owned `files/data/stageArc.bin` from the configured
release and adds a new archive mapping in an unused scenario of its reserved,
runtime-supported areas. Existing retail mappings and assets are never replaced.
A conflicting stage ID or an exhausted reserved scenario range is an error.

Stage content is authored through the normal scene workflow. The first
collision-bearing project model dragged into a new blank stage defaults to
**Bake as map terrain**, replacing only the internal placeholder during build.
The **Objects** content-browser tab exposes safe world-placement classes derived
from typed records in the configured retail stages. The user drags the typed
`Mario` class into the viewport for player placement; managed build and launch
remain blocked until it exists. Actors, enemies, NPCs, map objects, and other
cataloged placement classes use a deterministic retail-backed template for
their class, including its normal default parameter values. Canonical parameters
that do not drive dependency links or serialized stream layout remain editable
through validated typed inspector controls after placement. Linked names,
resource/character/rail selectors, and layout counts remain visible and explain
why they are read-only until their dependent records can be rebuilt safely.

Placing a catalog object carries its typed manager and character dependencies
into the scene, imports the discovered parameter, model, animation, collision,
and related resource closure, and merges any required named rail graph without
replacing unrelated graphs. Existing equivalent dependencies are reused.
Manager, director, and other service-only classes without a safe typed placement
template remain unavailable as direct authoring choices; required services are
installed automatically. The user assigns a placed model as the **Stage Skybox**
and edits ambient and light settings in-scene. These settings remain editable
after creation.
Only project-owned data and managed builds are changed; the extracted base game
remains read-only.

An authored stage adds these managed files:

```text
<project-data-root>/
  files/
    editor/
      stages/
        <stage-id>.stage.json
        <stage-id>.scene.json
    data/
      stageArc.bin
```

`<stage-id>.stage.json` is the source-free semantic baseline. It contains typed
stage data and deterministic reconstruction metadata rather than cached source
archive or container bytes. `<stage-id>.scene.json` is the normal editor overlay.
The overlay persists authored object prototypes, dependency records, resource
references, and canonical parameter edits rather than retaining the retail
archive as a fallback. When the project is reopened, the baseline is validated
and installed before the overlay. Managed export reconstructs object records and
their dependency/resource closure from this typed state. Unknown, ambiguous, or
noncanonical parameter edits are rejected. `files/data/stageArc.bin` is the
project-owned runtime table containing the new mapping; the configured release's
original table is not modified.

**Build Game** rebuilds the authored semantic stage as
`run-root/files/data/scene/<stage-id>.szs` and atomically overlays the
project-owned `stageArc.bin` into the managed release. **Launch in Editor** and
**Launch in Dolphin** perform the same build, resolve that archive against the
staged project-owned table, and patch the managed `sys/main.dol` for direct boot
into the allocated area and scenario. A missing mapping is rejected rather than
guessed.

These persistence and build paths remain experimental. Compilation, semantic
round trips, and automated build checks do not establish visual or in-game
runtime correctness; each authored stage still needs manual Dolphin verification.

## Managed build tree

Saving a project updates only its managed data; it does not create a playable
mod. **Build Game**, **Launch in Editor**, and **Launch in Dolphin** use the
separate project-owned managed build root:

```text
Isle Delfino.smsbuild/
  .smsbuild-owner.toml
  run-root/
    sys/main.dol
    files/...
```

The ownership marker binds the build root to the project identity. The editor
refuses an unowned or mismatched directory instead of taking it over.

**Build Game** reconciles `run-root/` as a complete runnable copy of the
extracted game, preserving every authored file's exact game-relative path.
Every run-root file has independent file identity; byte-identical copies are
reused on later builds. The rebuilt stage and project-owned file overlays,
including an authored stage's `files/data/stageArc.bin`, are installed
atomically. Saved stage-music choices are resolved through that staged table and
installed into the managed `sys/main.dol` as a runtime area/scenario dispatcher.
The dispatcher updates Sunshine's stage BGM, optional secondary crossfade BGM,
fade mode, and wave scene before audio
initialization, so the choices work when `run-root/` is booted normally in
Dolphin as well as through either editor launch button. Both launch buttons
perform the same build, resolve the open
archive through the resulting staged `stageArc.bin`, and atomically patch the
managed `sys/main.dol` copy. Its behavior-based PowerPC
patch suppresses the Nintendo-logo director, waits for the normal asynchronous
startup data load to finish, and then boots the resolved area and scenario
directly while preserving the executable's regional or modded code. Keeping
the launch executable at that exact path lets Dolphin mount the surrounding
extracted game directory. Dolphin uses its normal user profile by default,
preserving the user's controller configuration. If
`launch.dolphin_user_directory` is set, Dolphin uses that
profile instead. **Launch in Editor** reparents Dolphin's Windows render window
into the editor viewport until the user presses Stop. That mode temporarily
enables Dolphin background input and disables focus-loss pausing through
command-line overrides; it does not rewrite the saved Dolphin profile.
**Launch in Dolphin** leaves it as a normal external window. The extracted base is never
opened for modification; the next managed build refreshes the copy from its
configured base executable before preparing another launch.

Both managed launch actions require `launch.dolphin_executable`. The optional
`launch.dolphin_user_directory` applies to both managed and legacy launches.
`launch.game_image` belongs only to the legacy external Dolphin launch action.

## Recent projects

The launch hub keeps a separate application-level index of up to 12 recent
`.sms` descriptor paths. On Windows it is stored at
`%APPDATA%\Graffito-Editor\recent-projects.toml`. This index is not part of a project
and can be deleted without losing project data.

## Legacy folders

**Import Legacy Folder** reads an existing `sms-project.toml`, creates a new
`.sms` descriptor, and points `project_data_root` at the existing folder. The
overlay files are not moved or rewritten during import.
