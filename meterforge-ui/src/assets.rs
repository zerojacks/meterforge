//! 本地静态资源:优先加载 meter-ui 内置资源,再回退到 gpui-component 资源。

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// 内置的自定义图标,路径需以 `icons/` 开头供 `Icon::path` 使用。
const LOCAL_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/refresh-cw.svg",
        include_bytes!("../assets/icons/refresh-cw.svg"),
    ),
    // 软件图标，同一份资源也用于 Windows 可执行文件图标（见 build.rs）
    // 及 Linux 应用图标（见 packaging/linux）。
    ("icon/app.png", include_bytes!("../assets/icon/app.png")),
];

#[derive(Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, data)) = LOCAL_ASSETS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(data)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut entries = gpui_component_assets::Assets.list(path)?;
        entries.extend(
            LOCAL_ASSETS
                .iter()
                .map(|(p, _)| SharedString::from(*p))
                .filter(|p| p.starts_with(path)),
        );
        Ok(entries)
    }
}
