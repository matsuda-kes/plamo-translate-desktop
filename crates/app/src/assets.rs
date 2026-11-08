//! Icon assets: GPUI Kit's default component icons plus the extra Lucide icons the app uses.
//!
//! Without a registered asset source, GPUI renders every SVG icon as nothing, including the
//! title bar's window controls.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(ExtraIcons, [ArrowLeftRight, CircleArrowUp]);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::IconName;

    use super::AppAssets;

    #[test]
    fn serves_default_and_extra_icons() {
        for icon in [
            IconName::WindowClose,
            IconName::Copy,
            IconName::ArrowLeftRight,
        ] {
            let path = icon.path();
            assert!(
                AppAssets.load(&path).unwrap().is_some(),
                "missing icon asset {path}"
            );
        }
    }
}
