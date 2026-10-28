//! Secret Service storage against `gnome-keyring-daemon` on a PRIVATE D-Bus bus.
//!
//! Every test starts its own `dbus-daemon` with a config that has no service activation, so
//! nothing ever reaches the user's real session bus or keyring. Keyring files live in a
//! temporary `XDG_DATA_HOME`. Tests are skipped when the binaries are not installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use oo7::zbus;
use quick_translate_secrets::{APPLICATION, BackendId, SecretStore};

const PASSWORD: &str = "test-password";

struct PrivateBus {
    dir: tempfile::TempDir,
    address: String,
    bus: Child,
    keyring: Option<Child>,
}

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

impl PrivateBus {
    /// Starts a private bus, or returns `None` (test skipped) without `dbus-daemon`.
    fn start() -> Option<Self> {
        if !have("dbus-daemon") {
            eprintln!("dbus-daemon not installed, skipping");
            return None;
        }
        // Short path: unix socket names are limited to 108 bytes.
        let dir = tempfile::Builder::new()
            .prefix("qts")
            .tempdir_in("/tmp")
            .unwrap();
        for sub in ["home", "data", "config", "cache", "run"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        set_mode_700(&dir.path().join("run"));
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.path().display()
            ),
        )
        .unwrap();
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .args(["--nofork", "--print-address"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.path().join("home"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        assert!(address.starts_with("unix:"), "bad address {address:?}");
        assert!(
            address.contains(dir.path().to_str().unwrap()),
            "bus must be private"
        );
        Some(Self {
            dir,
            address,
            bus,
            keyring: None,
        })
    }

    /// Starts a bus with `gnome-keyring-daemon` providing an unlocked `login` collection.
    async fn with_keyring() -> Option<Self> {
        if !have("gnome-keyring-daemon") {
            eprintln!("gnome-keyring-daemon not installed, skipping");
            return None;
        }
        let mut bus = Self::start()?;
        bus.start_keyring().await;
        Some(bus)
    }

    async fn start_keyring(&mut self) {
        let d = self.dir.path();
        let mut child = Command::new("gnome-keyring-daemon")
            .args(["--foreground", "--components=secrets", "--unlock"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", d.join("home"))
            .env("XDG_DATA_HOME", d.join("data"))
            .env("XDG_CONFIG_HOME", d.join("config"))
            .env("XDG_CACHE_HOME", d.join("cache"))
            .env("XDG_RUNTIME_DIR", d.join("run"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(PASSWORD.as_bytes()).unwrap();
        drop(stdin);
        self.keyring = Some(child);
        self.wait_for_name("org.freedesktop.secrets", true).await;
    }

    async fn stop_keyring(&mut self) {
        if let Some(mut k) = self.keyring.take() {
            k.kill().ok();
            k.wait().ok();
        }
        self.wait_for_name("org.freedesktop.secrets", false).await;
    }

    async fn connection(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    async fn wait_for_name(&self, name: &str, owned: bool) {
        let cnx = self.connection().await;
        let dbus = zbus::fdo::DBusProxy::new(&cnx).await.unwrap();
        let name = zbus::names::BusName::try_from(name).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while dbus.name_has_owner(name.clone()).await.unwrap() != owned {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {name} owned={owned}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn files(&self) -> Vec<PathBuf> {
        fn walk(p: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.is_file() {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.dir.path(), &mut out);
        out
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        for child in self
            .keyring
            .iter_mut()
            .chain(std::iter::once(&mut self.bus))
        {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

fn set_mode_700(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn gnome_keyring_round_trip_with_attributes() {
    let Some(bus) = PrivateBus::with_keyring().await else {
        return;
    };
    let store = SecretStore::connect_to(&bus.address).await;
    assert!(store.is_persistent());

    assert_eq!(store.get(BackendId::Deepl).await, Ok(None));
    store.set(BackendId::Deepl, "deepl-key-1:fx").await.unwrap();
    store.set(BackendId::Openai, "sk-openai-1").await.unwrap();
    assert_eq!(
        store.get(BackendId::Deepl).await,
        Ok(Some("deepl-key-1:fx".into()))
    );
    store.set(BackendId::Deepl, "deepl-key-2:fx").await.unwrap();
    assert_eq!(
        store.get(BackendId::Deepl).await,
        Ok(Some("deepl-key-2:fx".into()))
    );

    // Check the stored items directly with the documented attributes.
    let service = oo7::dbus::Service::new_with_connection(&bus.connection().await)
        .await
        .unwrap();
    let collection = service.default_collection().await.unwrap();
    for (backend, secret) in [("deepl", "deepl-key-2:fx"), ("openai", "sk-openai-1")] {
        let items = collection
            .search_items(&[("application", APPLICATION), ("backend", backend)])
            .await
            .unwrap();
        assert_eq!(
            items.len(),
            1,
            "exactly one {backend} item (replaced, not duplicated)"
        );
        assert_eq!(items[0].secret().await.unwrap(), oo7::Secret::text(secret));
        let attrs = items[0].attributes().await.unwrap();
        assert_eq!(
            attrs.get("application").map(String::as_str),
            Some(APPLICATION)
        );
    }

    store.delete(BackendId::Deepl).await.unwrap();
    assert_eq!(store.get(BackendId::Deepl).await, Ok(None));
    assert_eq!(
        store.get(BackendId::Openai).await,
        Ok(Some("sk-openai-1".into()))
    );
}

#[tokio::test]
async fn gnome_keyring_persists_across_restarts_and_never_in_plaintext() {
    let Some(mut bus) = PrivateBus::with_keyring().await else {
        return;
    };
    let secret = "persist-me-0123456789:fx";
    SecretStore::connect_to(&bus.address)
        .await
        .set(BackendId::Deepl, secret)
        .await
        .unwrap();

    // A new store (daemon restart) and a restarted keyring daemon both still have it.
    assert_eq!(
        SecretStore::connect_to(&bus.address)
            .await
            .get(BackendId::Deepl)
            .await,
        Ok(Some(secret.into()))
    );
    bus.stop_keyring().await;
    bus.start_keyring().await;
    let store = SecretStore::connect_to(&bus.address).await;
    assert!(store.is_persistent());
    assert_eq!(store.get(BackendId::Deepl).await, Ok(Some(secret.into())));

    let files = bus.files();
    assert!(
        files.iter().any(|f| f.ends_with("login.keyring")),
        "{files:?}"
    );
    for f in files {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "plaintext key found in {}",
            f.display()
        );
    }
}

#[tokio::test]
async fn no_provider_falls_back_to_session_only() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let store = SecretStore::connect_to(&bus.address).await;
    assert!(!store.is_persistent());

    store.set(BackendId::Openai, "sk-session").await.unwrap();
    assert_eq!(
        store.get(BackendId::Openai).await,
        Ok(Some("sk-session".into()))
    );
    drop(store);

    // After a restart the key has to be entered again, and nothing was written to disk.
    let store = SecretStore::connect_to(&bus.address).await;
    assert_eq!(store.get(BackendId::Openai).await, Ok(None));
    assert!(
        bus.files().iter().all(|f| f.ends_with("bus.conf")),
        "{:?}",
        bus.files()
    );
}

#[tokio::test]
async fn unreachable_bus_falls_back_to_session_only() {
    let store = SecretStore::connect_to("unix:path=/nonexistent/qt-test-bus").await;
    assert!(!store.is_persistent());
    store.set(BackendId::Deepl, "k").await.unwrap();
    assert_eq!(store.get(BackendId::Deepl).await, Ok(Some("k".into())));
}

#[tokio::test]
async fn session_override_shadows_keyring() {
    let Some(bus) = PrivateBus::with_keyring().await else {
        return;
    };
    let store = SecretStore::connect_to(&bus.address).await;
    store.set(BackendId::Deepl, "stored").await.unwrap();
    store.set_for_session(BackendId::Deepl, "temporary");
    assert_eq!(
        store.get(BackendId::Deepl).await,
        Ok(Some("temporary".into()))
    );
    assert_eq!(
        SecretStore::connect_to(&bus.address)
            .await
            .get(BackendId::Deepl)
            .await,
        Ok(Some("stored".into()))
    );
}

/// `SecretStore::connect()` reads `DBUS_SESSION_BUS_ADDRESS`. The variable is set only for a
/// child test process, never for this process.
#[tokio::test]
async fn connect_uses_session_bus_address() {
    let Some(bus) = PrivateBus::with_keyring().await else {
        return;
    };
    let empty = PrivateBus::start().unwrap();
    for (address, expect) in [(&bus.address, "persistent"), (&empty.address, "session")] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_connect_from_env",
                "--ignored",
                "--nocapture",
            ])
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .env("QT_SECRETS_CHILD", expect)
            .status()
            .unwrap();
        assert!(status.success(), "child expecting {expect} failed");
    }
    let service = oo7::dbus::Service::new_with_connection(&bus.connection().await)
        .await
        .unwrap();
    let items = service
        .default_collection()
        .await
        .unwrap()
        .search_items(&[("application", APPLICATION), ("backend", "openai")])
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
#[ignore = "helper run by connect_uses_session_bus_address"]
async fn child_connect_from_env() {
    let Ok(expect) = std::env::var("QT_SECRETS_CHILD") else {
        return;
    };
    // Refuse to run against anything but a private test bus.
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    assert!(address.starts_with("unix:path=/tmp/qts"), "{address}");
    let store = SecretStore::connect().await;
    assert_eq!(store.is_persistent(), expect == "persistent");
    store.set(BackendId::Openai, "from-env").await.unwrap();
    assert_eq!(
        store.get(BackendId::Openai).await,
        Ok(Some("from-env".into()))
    );
}
