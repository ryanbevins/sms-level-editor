# SMS Editor Project Format

> [!WARNING]
> This format is experimental and may change before the first public release.

An SMS Editor project is identified by one UTF-8 TOML descriptor with the
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

[launch]
dolphin_executable = "C:\\Tools\\Dolphin\\Dolphin.exe"
game_image = "D:\\Games\\Super Mario Sunshine.rvz"
dolphin_user_directory = "C:\\DolphinProfiles\\SMS-Modding"
```

Required fields are:

- `format_version`: currently `1`;
- `kind`: always `sms-editor-project`;
- `name`: the user-facing project name;
- `project_id`: a stable identity generated when the descriptor is created;
- `created_with`: the editor version that created the descriptor;
- `base_game_root`: the extracted game directory, which remains read-only; and
- `project_data_root`: the directory containing editor-owned overlay data.

`managed_build_root`, `schema_source_root`, `last_stage`, and every value under
`launch` are optional. The editor updates `last_stage` after a stage opens so
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

## Managed build tree

Saving a project updates only its managed data; it does not create a playable
mod. **Build Game** and **Launch in Dolphin** use the separate project-owned
managed build root:

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
extracted game, preserving the
stage archive's exact game-relative path. Every run-root file has independent
file identity; byte-identical copies are reused on later builds. The rebuilt
stage is installed atomically. **Launch in Dolphin** performs the same build,
resolves the open archive through the staged game's own `stageArc.bin`, and
atomically patches the managed `sys/main.dol` copy. Its behavior-based PowerPC
patch boots the resolved area and scenario directly while preserving the
executable's regional or modded code. Keeping the launch executable at that
exact path lets Dolphin mount the surrounding extracted game directory. Dolphin
uses its normal user profile by default, preserving the user's controller
configuration. If `launch.dolphin_user_directory` is set, Dolphin uses that
profile instead. The extracted base is never opened for modification; the next managed build refreshes the copy
from its configured base executable before preparing another launch.

Managed **Launch in Dolphin** requires `launch.dolphin_executable`. The optional
`launch.dolphin_user_directory` applies to both managed and legacy launches.
`launch.game_image` belongs only to the legacy external Dolphin launch action.

## Recent projects

The launch hub keeps a separate application-level index of up to 12 recent
`.sms` descriptor paths. On Windows it is stored at
`%APPDATA%\SMS Editor\recent-projects.toml`. This index is not part of a project
and can be deleted without losing project data.

## Legacy folders

**Import Legacy Folder** reads an existing `sms-project.toml`, creates a new
`.sms` descriptor, and points `project_data_root` at the existing folder. The
overlay files are not moved or rewritten during import.
