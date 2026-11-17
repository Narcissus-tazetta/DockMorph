use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// アイコンはバイナリに埋め込む。.app の Resources に置くと、バンドルを作らない cargo run で表示が崩れるため
pub struct Assets;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "add", "sync", "play_arrow", "folder_open", "undo", "check_circle", "warning", "info", "close", "apps", "image",
    "download", "settings_backup_restore", "settings", "expand_more",
);

pub const MENUBAR_ICON: &[u8] = include_bytes!("../assets/menubar.svg");

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

pub fn icon(name: &str) -> SharedString {
    format!("icons/{name}.svg").into()
}
