//! The asset source the application registers: the component library's
//! default icon bundle, plus the icons KhonsuGraph draws beyond it and the
//! brand mark. As in DuckLocal, extra icons are listed one by one rather than
//! embedding the whole catalog.

use std::borrow::Cow;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    ExtraIcons,
    [Save, ListTree, Download, CircleStop, DatabasePlus, ChartNoAxesColumn, ChartPie, Code, MessageSquare, UserRound]
);

/// The brand mark, and the agents' marks drawn as their avatars (see
/// `ui::agent_avatar`), served at `agents/<name>.svg` as in DuckLocal.
const BRAND: &[(&str, &[u8])] = &[
    ("brand/logo.png", include_bytes!("../assets/logo-128.png")),
    ("agents/claude.svg", include_bytes!("../assets/agents/claude.svg")),
    ("agents/openai.svg", include_bytes!("../assets/agents/openai.svg")),
    ("agents/deepseek.svg", include_bytes!("../assets/agents/deepseek.svg")),
    ("agents/gemini.svg", include_bytes!("../assets/agents/gemini.svg")),
    ("agents/cursor.svg", include_bytes!("../assets/agents/cursor.svg")),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(
            BRAND
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(paths)
    }
}
