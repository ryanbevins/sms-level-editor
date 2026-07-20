use super::*;

pub(super) fn default_base_root() -> String {
    if let Ok(path) = std::env::var("SMS_BASE_ROOT") {
        if PathBuf::from(&path).exists() {
            return path;
        }
    }

    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let candidate = PathBuf::from(user_profile)
            .join("Downloads")
            .join("SunshineJPExtract");
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    String::new()
}

pub(super) fn default_repo_root() -> String {
    if let Ok(path) = std::env::var("SMS_REPO_ROOT") {
        if sms_repo_marker_exists(&PathBuf::from(&path)) {
            return path;
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        if let Some(root) = find_sms_repo_root(&current_dir) {
            return root;
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            if let Some(root) = find_sms_repo_root(parent) {
                return root;
            }
        }
    }

    "..".to_string()
}

pub(super) fn find_sms_repo_root(start: &std::path::Path) -> Option<String> {
    start
        .ancestors()
        .find(|candidate| sms_repo_marker_exists(candidate))
        .map(|candidate| candidate.to_string_lossy().to_string())
}

pub(super) fn sms_repo_marker_exists(path: &std::path::Path) -> bool {
    path.join("src")
        .join("System")
        .join("MarNameRefGen.cpp")
        .exists()
}

#[derive(Debug, Default)]
pub(super) struct EditorStartupArgs {
    pub(super) project_file: Option<PathBuf>,
    pub(super) repo_root: Option<String>,
    pub(super) base_root: Option<String>,
    pub(super) stage_id: Option<String>,
    pub(super) focus_object: Option<String>,
    pub(super) camera_focus: Option<[f32; 3]>,
    pub(super) camera_distance: Option<f32>,
    pub(super) camera_yaw: Option<f32>,
    pub(super) camera_pitch: Option<f32>,
}

pub(super) fn editor_startup_args() -> EditorStartupArgs {
    editor_startup_args_from(std::env::args().skip(1))
}

fn editor_startup_args_from(args: impl IntoIterator<Item = String>) -> EditorStartupArgs {
    let mut parsed = EditorStartupArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => parsed.project_file = args.next().map(PathBuf::from),
            "--repo-root" => parsed.repo_root = args.next(),
            "--base-root" => parsed.base_root = args.next(),
            "--stage" | "--stage-id" => parsed.stage_id = args.next(),
            "--focus-object" => parsed.focus_object = args.next(),
            "--camera-focus" => {
                if let Some(value) = args.next() {
                    parsed.camera_focus = parse_vec3_arg(&value).or_else(|| {
                        let x = value.parse().ok()?;
                        let y = args.next()?.parse().ok()?;
                        let z = args.next()?.parse().ok()?;
                        Some([x, y, z])
                    });
                }
            }
            "--camera-distance" => {
                parsed.camera_distance = args.next().and_then(|value| value.parse().ok())
            }
            "--camera-yaw" => parsed.camera_yaw = args.next().and_then(|value| value.parse().ok()),
            "--camera-pitch" => {
                parsed.camera_pitch = args.next().and_then(|value| value.parse().ok())
            }
            _ if !arg.starts_with('-')
                && std::path::Path::new(&arg)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("sms")) =>
            {
                parsed.project_file = Some(PathBuf::from(arg));
            }
            _ => {}
        }
    }

    parsed
}

pub(super) fn parse_vec3_arg(value: &str) -> Option<[f32; 3]> {
    let mut parts = value.split(',').map(str::trim);
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let z = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some([x, y, z])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_accepts_a_positional_sms_project() {
        let parsed = editor_startup_args_from([r"C:\Mods\Isle Delfino.sms".to_string()]);
        assert_eq!(
            parsed.project_file,
            Some(PathBuf::from(r"C:\Mods\Isle Delfino.sms"))
        );
    }

    #[test]
    fn explicit_project_flag_accepts_a_mixed_case_extension() {
        let parsed =
            editor_startup_args_from(["--project".to_string(), r"C:\Mods\Bianco.SMS".to_string()]);
        assert_eq!(
            parsed.project_file,
            Some(PathBuf::from(r"C:\Mods\Bianco.SMS"))
        );
    }
}
