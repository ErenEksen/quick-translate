//! Runtime capabilities detected at daemon start (design D2).
//!
//! Every platform decision is driven by these fields. `compositor` is used only to pick
//! which binding snippet to show in settings, never for behavior.

use std::fmt;

/// Protocol and portal versions detected at runtime. `None` means not advertised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `zwlr_layer_shell_v1` version.
    pub layer_shell: Option<u32>,
    /// `ext_data_control_manager_v1` version.
    pub ext_data_control: Option<u32>,
    /// `zwlr_data_control_manager_v1` version. Only version 2 or later supports PRIMARY.
    pub wlr_data_control: Option<u32>,
    /// `xdg_activation_v1` version.
    pub xdg_activation: Option<u32>,
    /// `wp_fractional_scale_manager_v1` version.
    pub fractional_scale: Option<u32>,
    /// `wp_viewporter` version.
    pub viewporter: Option<u32>,
    /// `org.freedesktop.portal.GlobalShortcuts` interface version.
    pub portal_global_shortcuts: Option<u32>,
    /// Detected compositor, for snippet selection only.
    pub compositor: Compositor,
}

/// The data-control protocol usable for a background PRIMARY read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataControl {
    Ext(u32),
    Wlr(u32),
}

impl Capabilities {
    /// The preferred usable data-control protocol: ext-data-control-v1, else wlr v2 or later.
    pub fn data_control(&self) -> Option<DataControl> {
        match (self.ext_data_control, self.wlr_data_control) {
            (Some(v), _) => Some(DataControl::Ext(v)),
            (None, Some(v)) if v >= 2 => Some(DataControl::Wlr(v)),
            _ => None,
        }
    }

    /// Whether the popup uses the layer shell (`true`) or the window shell (`false`).
    pub fn has_layer_shell(&self) -> bool {
        self.layer_shell.is_some()
    }
}

/// The running compositor or desktop, detected from environment variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Compositor {
    Kde,
    Gnome,
    Hyprland,
    Sway,
    Niri,
    River,
    Labwc,
    Wayfire,
    Cosmic,
    /// Anything else. Holds `XDG_CURRENT_DESKTOP` if it was set.
    Other(Option<String>),
    #[default]
    Unknown,
}

impl Compositor {
    /// Detect from `XDG_CURRENT_DESKTOP`, `HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`, `NIRI_SOCKET`, etc.
    pub fn detect() -> Self {
        Self::detect_from(|key| std::env::var(key).ok())
    }

    /// Testable detection with an injected environment lookup.
    ///
    /// Sockets set by the compositor itself win over `XDG_CURRENT_DESKTOP`, which users often
    /// override for portal selection. Weaker hints are consulted last.
    pub fn detect_from(env: impl Fn(&str) -> Option<String>) -> Self {
        let set = |key: &str| env(key).is_some_and(|v| !v.trim().is_empty());

        if set("HYPRLAND_INSTANCE_SIGNATURE") {
            return Self::Hyprland;
        }
        if set("NIRI_SOCKET") {
            return Self::Niri;
        }
        if set("SWAYSOCK") {
            return Self::Sway;
        }

        let desktop = env("XDG_CURRENT_DESKTOP")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if let Some(found) = desktop
            .as_deref()
            .and_then(|d| d.split(':').find_map(Self::from_desktop_name))
        {
            return found;
        }

        if set("WAYFIRE_SOCKET") {
            Self::Wayfire
        } else if set("LABWC_PID") {
            Self::Labwc
        } else if set("KDE_FULL_SESSION") {
            Self::Kde
        } else if let Some(desktop) = desktop {
            Self::Other(Some(desktop))
        } else {
            Self::Unknown
        }
    }

    fn from_desktop_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Some(match name.as_str() {
            "kde" | "plasma" => Self::Kde,
            "gnome" | "gnome-classic" | "gnome-flashback" => Self::Gnome,
            "hyprland" => Self::Hyprland,
            "sway" => Self::Sway,
            "niri" => Self::Niri,
            "river" => Self::River,
            "labwc" => Self::Labwc,
            "wayfire" => Self::Wayfire,
            "cosmic" => Self::Cosmic,
            _ => return None,
        })
    }
}

impl fmt::Display for Compositor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Kde => "KDE Plasma",
            Self::Gnome => "GNOME",
            Self::Hyprland => "Hyprland",
            Self::Sway => "Sway",
            Self::Niri => "niri",
            Self::River => "river",
            Self::Labwc => "labwc",
            Self::Wayfire => "Wayfire",
            Self::Cosmic => "COSMIC",
            Self::Other(Some(desktop)) => return write!(f, "other ({desktop})"),
            Self::Other(None) => "other",
            Self::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

struct Version(Option<u32>);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "yes (v{v})"),
            None => f.write_str("no"),
        }
    }
}

/// Multi-line human-readable report used by `quick-translate --status` and settings Diagnostics.
///
/// The output has no trailing newline.
impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let primary = match self.data_control() {
            Some(DataControl::Ext(v)) => format!("yes (ext-data-control v{v})"),
            Some(DataControl::Wlr(v)) => format!("yes (wlr-data-control v{v})"),
            None if self.wlr_data_control.is_some() => {
                "no (wlr-data-control v1 lacks PRIMARY)".to_owned()
            }
            None => "no".to_owned(),
        };
        let popup = if self.has_layer_shell() {
            "layer-shell"
        } else {
            "xdg-toplevel window"
        };
        let rows: [(&str, String); 10] = [
            (
                "Layer shell (zwlr_layer_shell_v1)",
                Version(self.layer_shell).to_string(),
            ),
            (
                "Data control (ext_data_control_manager_v1)",
                Version(self.ext_data_control).to_string(),
            ),
            (
                "Data control (zwlr_data_control_manager_v1)",
                Version(self.wlr_data_control).to_string(),
            ),
            (
                "XDG activation (xdg_activation_v1)",
                Version(self.xdg_activation).to_string(),
            ),
            (
                "Fractional scale (wp_fractional_scale_manager_v1)",
                Version(self.fractional_scale).to_string(),
            ),
            (
                "Viewporter (wp_viewporter)",
                Version(self.viewporter).to_string(),
            ),
            (
                "Portal GlobalShortcuts",
                Version(self.portal_global_shortcuts).to_string(),
            ),
            ("Background PRIMARY read", primary),
            ("Popup surface", popup.to_owned()),
            ("Compositor", self.compositor.to_string()),
        ];
        let width = rows
            .iter()
            .map(|(label, _)| label.len() + 1)
            .max()
            .unwrap_or(0);
        for (i, (label, value)) in rows.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{:<width$} {value}", format!("{label}:"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn data_control_prefers_ext() {
        let caps = Capabilities {
            ext_data_control: Some(1),
            wlr_data_control: Some(2),
            ..Default::default()
        };
        assert_eq!(caps.data_control(), Some(DataControl::Ext(1)));
    }

    #[test]
    fn data_control_requires_wlr_v2() {
        let mut caps = Capabilities {
            wlr_data_control: Some(1),
            ..Default::default()
        };
        assert_eq!(caps.data_control(), None);
        caps.wlr_data_control = Some(2);
        assert_eq!(caps.data_control(), Some(DataControl::Wlr(2)));
        caps.wlr_data_control = None;
        assert_eq!(caps.data_control(), None);
    }

    #[test]
    fn detect_compositor() {
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "KDE")])),
            Compositor::Kde
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")])),
            Compositor::Gnome
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "labwc:wlroots")])),
            Compositor::Labwc
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "COSMIC")])),
            Compositor::Cosmic
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "river")])),
            Compositor::River
        );
        assert_eq!(
            Compositor::detect_from(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc")])),
            Compositor::Hyprland
        );
        assert_eq!(
            Compositor::detect_from(env(&[("SWAYSOCK", "/run/user/1000/sway-ipc.sock")])),
            Compositor::Sway
        );
        // niri users often set XDG_CURRENT_DESKTOP=GNOME for portals; the socket wins.
        assert_eq!(
            Compositor::detect_from(env(&[
                ("XDG_CURRENT_DESKTOP", "GNOME"),
                ("NIRI_SOCKET", "/run/user/1000/niri.sock"),
            ])),
            Compositor::Niri
        );
        assert_eq!(
            Compositor::detect_from(env(&[("WAYFIRE_SOCKET", "/tmp/wayfire")])),
            Compositor::Wayfire
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", "X-Cinnamon")])),
            Compositor::Other(Some("X-Cinnamon".into()))
        );
        assert_eq!(
            Compositor::detect_from(env(&[("XDG_CURRENT_DESKTOP", ""), ("SWAYSOCK", "")])),
            Compositor::Unknown
        );
        assert_eq!(Compositor::detect_from(env(&[])), Compositor::Unknown);
    }

    #[test]
    fn compositor_display() {
        assert_eq!(Compositor::Kde.to_string(), "KDE Plasma");
        assert_eq!(Compositor::Niri.to_string(), "niri");
        assert_eq!(
            Compositor::Other(Some("X-Cinnamon".into())).to_string(),
            "other (X-Cinnamon)"
        );
        assert_eq!(Compositor::Other(None).to_string(), "other");
        assert_eq!(Compositor::Unknown.to_string(), "unknown");
    }

    #[test]
    fn display_full_report() {
        let caps = Capabilities {
            layer_shell: Some(4),
            ext_data_control: Some(1),
            wlr_data_control: Some(2),
            xdg_activation: Some(1),
            fractional_scale: Some(1),
            viewporter: Some(1),
            portal_global_shortcuts: Some(2),
            compositor: Compositor::Kde,
        };
        let expected = "\
Layer shell (zwlr_layer_shell_v1):                 yes (v4)
Data control (ext_data_control_manager_v1):        yes (v1)
Data control (zwlr_data_control_manager_v1):       yes (v2)
XDG activation (xdg_activation_v1):                yes (v1)
Fractional scale (wp_fractional_scale_manager_v1): yes (v1)
Viewporter (wp_viewporter):                        yes (v1)
Portal GlobalShortcuts:                            yes (v2)
Background PRIMARY read:                           yes (ext-data-control v1)
Popup surface:                                     layer-shell
Compositor:                                        KDE Plasma";
        assert_eq!(caps.to_string(), expected);
    }

    #[test]
    fn display_minimal_report() {
        let caps = Capabilities {
            wlr_data_control: Some(1),
            compositor: Compositor::Gnome,
            ..Default::default()
        };
        let report = caps.to_string();
        let lines: Vec<_> = report.lines().collect();
        assert_eq!(lines.len(), 10);
        assert!(lines[0].ends_with(" no"));
        assert!(lines[2].ends_with(" yes (v1)"));
        assert!(lines[6].starts_with("Portal GlobalShortcuts:") && lines[6].ends_with(" no"));
        assert!(lines[7].ends_with(" no (wlr-data-control v1 lacks PRIMARY)"));
        assert!(lines[8].ends_with(" xdg-toplevel window"));
        assert!(lines[9].ends_with(" GNOME"));
        assert!(!report.ends_with('\n'));
    }
}
