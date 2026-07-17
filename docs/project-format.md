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

`schema_source_root`, `last_stage`, and every value under `launch` are optional.
The editor updates `last_stage` after a stage opens so reopening the project can
restore the working context.

## Path resolution

`project_data_root` may be absolute or relative. A relative value is resolved
against the directory containing the `.sms` descriptor and cannot contain
parent-directory traversal. New projects use a sibling `<name>.smsdata` folder
by default. All other stored paths are explicit filesystem locations chosen
through native file or folder dialogs.

Project data must not overlap the extracted base-game directory. The managed
data folder retains the existing transactional `sms-project.toml` manifest and
`files/` overlay tree, including its base-game identity and lossless-save
safeguards.

## Recent projects

The launch hub keeps a separate application-level index of up to 12 recent
`.sms` descriptor paths. On Windows it is stored at
`%APPDATA%\SMS Editor\recent-projects.toml`. This index is not part of a project
and can be deleted without losing project data.

## Legacy folders

**Import Legacy Folder** reads an existing `sms-project.toml`, creates a new
`.sms` descriptor, and points `project_data_root` at the existing folder. The
overlay files are not moved or rewritten during import.
