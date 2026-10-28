//! System light/dark preference from the XDG Settings portal (`org.freedesktop.appearance`
//! `color-scheme`), with live updates.
//!
//! Use a connection that makes no other portal calls: a portal call before
//! `Registry.Register` would tie that connection to a guessed app ID.

use std::time::Duration;

use ashpd::desktop::settings::{ColorScheme as PortalScheme, Settings};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{MatchRule, MessageStream};

const READ_TIMEOUT: Duration = Duration::from_secs(2);
const SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// The preferred color scheme. Without a portal value it is [`ColorScheme::NoPreference`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    NoPreference,
    PreferDark,
    PreferLight,
}

impl ColorScheme {
    pub fn is_dark(self) -> bool {
        self == Self::PreferDark
    }
}

impl From<PortalScheme> for ColorScheme {
    fn from(value: PortalScheme) -> Self {
        match value {
            PortalScheme::PreferDark => Self::PreferDark,
            PortalScheme::PreferLight => Self::PreferLight,
            PortalScheme::NoPreference => Self::NoPreference,
        }
    }
}

/// The current scheme and a stream of later changes. Never fails: without a portal the scheme
/// is [`ColorScheme::NoPreference`] and the stream never yields.
pub async fn watch(
    connection: &zbus::Connection,
) -> (ColorScheme, BoxStream<'static, ColorScheme>) {
    let setup = async {
        // Subscribe before reading so a change in between is not lost.
        let changes = changes(connection).await?;
        let settings = Settings::with_connection(connection.clone())
            .await
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        let current = settings
            .color_scheme()
            .await
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok::<_, zbus::Error>((ColorScheme::from(current), changes))
    };
    match tokio::time::timeout(READ_TIMEOUT, setup).await {
        Ok(Ok((current, changes))) => {
            tracing::debug!(?current, "portal color scheme");
            (current, changes)
        }
        Ok(Err(err)) => {
            tracing::debug!("no portal color scheme, using light: {err}");
            (ColorScheme::NoPreference, stream::pending().boxed())
        }
        Err(_) => {
            tracing::debug!("portal color scheme read timed out, using light");
            (ColorScheme::NoPreference, stream::pending().boxed())
        }
    }
}

async fn changes(connection: &zbus::Connection) -> zbus::Result<BoxStream<'static, ColorScheme>> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(SETTINGS_INTERFACE)?
        .member("SettingChanged")?
        .path(PORTAL_PATH)?
        .arg(0, NAMESPACE)?
        .arg(1, KEY)?
        .build();
    let messages = MessageStream::for_match_rule(rule, connection, Some(8)).await?;
    Ok(messages
        .filter_map(|message| async move {
            let message = message.ok()?;
            let (namespace, key, value): (String, String, OwnedValue) =
                message.body().deserialize().ok()?;
            (namespace == NAMESPACE && key == KEY).then(|| scheme_from_value(&value))?
        })
        .boxed())
}

/// The portal sends `u32` 0 (no preference), 1 (dark) or 2 (light), possibly wrapped in a
/// second variant.
fn scheme_from_value(value: &Value<'_>) -> Option<ColorScheme> {
    match value {
        Value::Value(inner) => scheme_from_value(inner),
        Value::U32(1) => Some(ColorScheme::PreferDark),
        Value::U32(2) => Some(ColorScheme::PreferLight),
        Value::U32(_) => Some(ColorScheme::NoPreference),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_prefer_dark_is_dark() {
        assert!(ColorScheme::PreferDark.is_dark());
        assert!(!ColorScheme::PreferLight.is_dark());
        assert!(!ColorScheme::NoPreference.is_dark());
        assert_eq!(
            ColorScheme::from(PortalScheme::PreferDark),
            ColorScheme::PreferDark
        );
    }

    #[test]
    fn signal_values() {
        assert_eq!(
            scheme_from_value(&Value::U32(1)),
            Some(ColorScheme::PreferDark)
        );
        let wrapped = Value::Value(Box::new(Value::U32(2)));
        assert_eq!(scheme_from_value(&wrapped), Some(ColorScheme::PreferLight));
        assert_eq!(
            scheme_from_value(&Value::U32(0)),
            Some(ColorScheme::NoPreference)
        );
        assert_eq!(scheme_from_value(&Value::from("dark")), None);
    }
}
