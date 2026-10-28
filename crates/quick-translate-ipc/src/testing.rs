//! Test support: a private `dbus-daemon --session` so tests never touch the user's bus.

use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

const CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// A private session bus listening inside a caller-owned directory. Killed on drop.
pub struct PrivateBus {
    child: Child,
    address: String,
}

impl PrivateBus {
    pub fn start(dir: &Path) -> io::Result<Self> {
        let socket = dir.join("bus");
        // No service directories: nothing on this bus may D-Bus-activate the user's real portal,
        // keyring or other session services.
        let config = dir.join("bus.conf");
        std::fs::write(&config, CONFIG)?;
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut line = String::new();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("dbus-daemon stdout missing"))?;
        BufReader::new(stdout).read_line(&mut line)?;
        let address = line.trim().to_owned();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("dbus-daemon printed no address"));
        }
        Ok(Self { child, address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
