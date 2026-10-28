//! Popup theme from the system color scheme (spec translation-popup, "Theme follows system
//! preference"): dark for `prefer-dark`, light otherwise, including without a portal value.

use iced::Theme;
use quick_translate_platform::appearance::ColorScheme;

pub fn for_scheme(scheme: ColorScheme) -> Theme {
    if scheme.is_dark() {
        Theme::Dark
    } else {
        Theme::Light
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_unless_dark_is_preferred() {
        assert_eq!(for_scheme(ColorScheme::PreferDark), Theme::Dark);
        assert_eq!(for_scheme(ColorScheme::PreferLight), Theme::Light);
        assert_eq!(for_scheme(ColorScheme::NoPreference), Theme::Light);
    }
}
