use std::{borrow::Cow, collections::HashMap, ffi::OsStr, fs, io, path::{Component, Path, PathBuf}, sync::OnceLock};
use gpui_kit::{AssetSource, Result, SharedString};

static COLORS: OnceLock<HashMap<String, u32>> = OnceLock::new();

pub struct FileAssets {
    root: PathBuf,
}

impl AssetSource for FileAssets {
    fn load(&self, key: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if key.is_empty() || !Path::new(key).components().all(|part| matches!(part, Component::Normal(_))) {
            return Ok(None);
        }
        match fs::read(self.root.join(key)) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<SharedString>> {
        let mut paths = Vec::new();
        read_paths(&self.root, &self.root, &mut paths)?;
        Ok(paths.into_iter().filter(|path| path.starts_with(prefix)).map(Into::into).collect())
    }
}

fn read_paths(root: &Path, folder: &Path, paths: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            read_paths(root, &path, paths)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

pub fn initialize() -> Result<FileAssets> {
    let executable = std::env::current_exe()?;
    let folder = executable.parent().ok_or_else(|| io::Error::other("UTAchat installation path unavailable"))?;
    let raw = fs::read_to_string(folder.join("themes").join("dark.json"))?;
    #[derive(serde::Deserialize)]
    struct ThemeFile {
        colors: HashMap<String, String>,
    }
    let file: ThemeFile = serde_json::from_str(&raw)?;
    let mut colors = HashMap::new();
    for (key, value) in file.colors {
        let hex = value.strip_prefix('#').ok_or_else(|| io::Error::other(format!("Theme color {key} must start with #")))?;
        if hex.len() != 6 && hex.len() != 8 {
            return Err(io::Error::other(format!("Invalid theme color {key}")).into());
        }
        colors.insert(key, u32::from_str_radix(hex, 16)?);
    }
    for name in ["rail", "sidebar", "content", "header", "member_bg", "input_bg", "text", "muted", "subtle", "accent", "line", "danger_text", "auth_link", "server_inactive", "server_action_bg", "server_add", "selection", "link", "friend_accept", "separator", "profile_bar", "header_separator", "online", "reconnecting", "owner_avatar", "member_avatar", "join_button", "overlay", "toast_bg", "delete_action", "message_text", "avatar_0", "avatar_1", "avatar_2", "avatar_3", "avatar_4", "avatar_5", "avatar_6", "avatar_7"] {
        if !colors.contains_key(name) {
            return Err(io::Error::other(format!("Missing theme color {name}")).into());
        }
    }
    let assets = folder.join("assets");
    for name in ["icons/utachat-mark.svg", "icons/send.svg", "icons/check.svg"] {
        if !assets.join(name).is_file() {
            return Err(io::Error::other(format!("Missing application resource: assets/{name}")).into());
        }
    }
    if !folder.join("config").join("connection.json").is_file() {
        return Err(io::Error::other("Missing config/connection.json").into());
    }
    COLORS.set(colors).map_err(|_| io::Error::other("UTAchat resources initialized twice"))?;
    Ok(FileAssets { root: assets })
}

pub fn theme_color(name: &str) -> u32 {
    *COLORS.get().expect("UTAchat theme unavailable")
        .get(name).unwrap_or_else(|| panic!("Theme color missing: {name}"))
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
}

pub fn fatal(error: impl std::fmt::Display) -> ! {
    let message = format!("UTAchat could not load its installation files:\n\n{error}");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = OsStr::new(&message).encode_wide().chain(std::iter::once(0)).collect();
        let caption: Vec<u16> = OsStr::new("UTAchat").encode_wide().chain(std::iter::once(0)).collect();
        unsafe {
            MessageBoxW(std::ptr::null_mut(), wide.as_ptr(), caption.as_ptr(), 0x10);
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
    std::process::exit(1)
}
