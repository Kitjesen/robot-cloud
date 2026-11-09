use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_owned()
}

pub fn binary(name: &str) -> Result<PathBuf> {
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target"));
    let path = target
        .join("debug")
        .join(format!("{name}{}", env::consts::EXE_SUFFIX));
    ensure!(
        path.is_file(),
        "missing {}; run cargo build --workspace first",
        path.display()
    );
    Ok(path)
}

fn executable(variable: &str, name: &str) -> Result<PathBuf> {
    let name = env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{name}{}", env::consts::EXE_SUFFIX)));
    if name.is_file() {
        return Ok(fs::canonicalize(name)?);
    }
    for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(&name);
        if candidate.is_file() {
            return Ok(fs::canonicalize(candidate)?);
        }
    }
    bail!("tool {} not found; set {variable}", name.display())
}

pub fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn apply_environment(command: &mut Command, values: &BTreeMap<String, String>) {
    command
        .env_remove("DOSO_GATEWAY_API_KEY")
        .env_remove("DOSO_REMOTE_SYNC_KEY")
        .env_remove("DOSO_CLOUD_ON")
        .env_remove("DOSO_REMOTE_ENABLED")
        .env_remove("DOSO_CLOUD_CHOICE")
        .env_remove("DOSO_VIDEO_CONFIG")
        .envs(values);
}

pub struct Process {
    child: Child,
    log: PathBuf,
}

impl Process {
    fn spawn(mut command: Command, log: PathBuf) -> Result<Self> {
        let output = fs::File::create(&log)?;
        command
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        Ok(Self {
            child: command.spawn().context("start lab subprocess")?,
            log,
        })
    }

    pub fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn check(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("lab subprocess exited ({status}): {}", self.logs());
        }
        Ok(())
    }

    pub async fn wait_success(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(
                    status.success(),
                    "lab subprocess failed ({status}): {}",
                    self.logs()
                );
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "lab subprocess did not stop: {}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().context("kill lab subprocess")?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait()?.is_none() {
                ensure!(
                    Instant::now() < deadline,
                    "lab subprocess cleanup timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}

pub struct Fixture {
    pub directory: PathBuf,
    pub processes: Vec<Process>,
}

impl Fixture {
    pub async fn new() -> Result<Self> {
        let directory = root()
            .join(".runtime")
            .join(format!("lab-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(directory.parent().unwrap())?;
        fs::create_dir(&directory)?;
        let mut fixture = Self {
            directory,
            processes: Vec::new(),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(windows)]
        {
            let user = format!("{}\\{}", env::var("USERDOMAIN")?, env::var("USERNAME")?);
            let mut command = Command::new("icacls.exe");
            command.arg(&fixture.directory).args([
                "/inheritance:r",
                "/grant:r",
                &format!("{user}:(OI)(CI)F"),
            ]);
            fixture.run(command, "permissions").await?;
        }
        Ok(fixture)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    fn mqtt_env(&self, port: u16) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("DOSO_MQTT_URL".into(), format!("mqtts://127.0.0.1:{port}")),
            (
                "DOSO_MQTT_CA".into(),
                self.path("ca.pem").to_string_lossy().into_owned(),
            ),
        ])
    }

    pub fn gateway_env(&self, broker_port: u16, port: u16, key: &str) -> BTreeMap<String, String> {
        let mut values = self.mqtt_env(broker_port);
        for (name, file) in [
            ("DOSO_MQTT_CERT", "doso-gateway.pem"),
            ("DOSO_MQTT_KEY", "doso-gateway.key"),
            ("DOSO_GATEWAY_SIGNING_KEY", "gateway-signing.key"),
            ("DOSO_DEVICE_REGISTRY", "devices.json"),
            ("DOSO_REPORTING_STATE", "reporting.json"),
        ] {
            values.insert(name.into(), self.path(file).to_string_lossy().into_owned());
        }
        values.insert("DOSO_MQTT_CLIENT_ID".into(), "doso-gateway".into());
        values.insert("DOSO_GATEWAY_PORT".into(), port.to_string());
        values.insert("DOSO_GATEWAY_API_KEY".into(), key.into());
        values
    }

    pub fn link_env(&self, device: &str, broker_port: u16) -> Result<BTreeMap<String, String>> {
        let path = |name: &str| self.path(name).to_string_lossy().into_owned();
        fs::write(self.path("cloud-choice"), "true\n")?;
        fs::write(
            self.path("video-source.json"),
            serde_json::to_vec(&serde_json::json!({
                "program": env::current_exe()?,
                "args": ["video-source", path("video-heartbeat")],
            }))?,
        )?;
        let mut values = self.mqtt_env(broker_port);
        for (name, value) in [
            ("DOSO_CLOUD_ON", "true".to_owned()),
            ("DOSO_CLOUD_CHOICE", path("cloud-choice")),
            ("DOSO_DEVICE_ID", device.to_owned()),
            ("DOSO_GNSS_STATUS", path("gnss-status.json")),
            ("DOSO_MQTT_CERT", path(&format!("{device}.pem"))),
            ("DOSO_MQTT_KEY", path(&format!("{device}.key"))),
            ("DOSO_DEVICE_SIGNING_KEY", path("device-signing.key")),
            ("DOSO_GATEWAY_PUBLIC_KEY", path("gateway-signing.pub")),
            ("DOSO_VIDEO_CONFIG", path("video-source.json")),
        ] {
            values.insert(name.into(), value);
        }
        Ok(values)
    }

    pub fn spawn(
        &mut self,
        binary: &Path,
        values: &BTreeMap<String, String>,
        name: &str,
    ) -> Result<()> {
        let mut command = Command::new(binary);
        apply_environment(&mut command, values);
        self.processes
            .push(Process::spawn(command, self.path(&format!("{name}.log")))?);
        Ok(())
    }

    pub fn check(&mut self) -> Result<()> {
        for process in &mut self.processes {
            process.check()?;
        }
        Ok(())
    }

    async fn run(&mut self, command: Command, name: &str) -> Result<()> {
        self.processes
            .push(Process::spawn(command, self.path(&format!("{name}.log")))?);
        let deadline = Instant::now() + Duration::from_secs(15);
        let process = self.processes.last_mut().unwrap();
        loop {
            if let Some(status) = process.child.try_wait()? {
                ensure!(
                    status.success(),
                    "{name} failed ({status}): {}",
                    process.logs()
                );
                self.processes.pop();
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "{name} timed out: {}",
                process.logs()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn openssl(&mut self, openssl: &Path, arguments: &str) -> Result<()> {
        let mut command = Command::new(openssl);
        command
            .current_dir(&self.directory)
            .env("OPENSSL_CONF", self.path("openssl.cnf"))
            .args(arguments.split_ascii_whitespace());
        self.run(command, "openssl").await
    }

    pub async fn broker(&mut self, device: &str) -> Result<u16> {
        let broker = executable("MOSQUITTO_BIN", "mosquitto")?;
        let openssl = executable("OPENSSL_BIN", "openssl")?;
        let plugin = env::var_os("MOSQUITTO_ACL_PLUGIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                broker.parent().unwrap().join(if cfg!(windows) {
                    "mosquitto_acl_file.dll"
                } else {
                    "mosquitto_acl_file.so"
                })
            });
        let plugin = fs::canonicalize(&plugin).with_context(|| {
            format!(
                "missing ACL plugin {}; set MOSQUITTO_ACL_PLUGIN",
                plugin.display()
            )
        })?;
        fs::write(
            self.path("openssl.cnf"),
            "[req]\ndistinguished_name=dn\n[dn]\n",
        )?;
        self.openssl(&openssl, "req -x509 -newkey rsa:2048 -nodes -days 2 -keyout ca.key -out ca.pem -subj /CN=DOSO-LAB-ONLY-ca -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign").await?;
        for name in [device, "doso-gateway", "server"] {
            self.openssl(&openssl, &format!("req -new -newkey rsa:2048 -nodes -keyout {name}.key -out {name}.csr -subj /CN={name}")).await?;
            let extensions = if name == "server" {
                "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,DNS:localhost\n"
            } else {
                "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\n"
            };
            fs::write(self.path("certificate.ext"), extensions)?;
            self.openssl(&openssl, &format!("x509 -req -in {name}.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out {name}.pem -days 2 -extfile certificate.ext")).await?;
        }
        for name in ["device", "gateway"] {
            self.openssl(
                &openssl,
                &format!("genpkey -algorithm ED25519 -out {name}-signing.key"),
            )
            .await?;
            self.openssl(
                &openssl,
                &format!("pkey -in {name}-signing.key -pubout -out {name}-signing.pub"),
            )
            .await?;
        }
        let acl = fs::read_to_string(root().join("cloud/broker/acl.conf.example"))?
            .replace("00000000-0000-4000-8000-000000000001", device);
        fs::write(self.path("acl.conf"), acl)?;
        let port = free_port()?;
        let portable = |path: &Path| {
            let path = path.to_string_lossy();
            let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
            path.strip_prefix(r"UNC\")
                .map_or_else(|| path.to_owned(), |rest| format!("//{rest}"))
                .replace('\\', "/")
        };
        let config = fs::read_to_string(root().join("cloud/broker/mosquitto.conf.example"))?
            .replace("@ACL_PLUGIN_PATH@", &portable(&plugin))
            .replace("listener 8883", &format!("listener {port} 127.0.0.1"))
            .replace("/etc/doso-mqtt/acl.conf", &portable(&self.path("acl.conf")))
            .replace(
                "/etc/doso-mqtt/tls/device-ca.pem",
                &portable(&self.path("ca.pem")),
            )
            .replace(
                "/etc/doso-mqtt/tls/server.pem",
                &portable(&self.path("server.pem")),
            )
            .replace(
                "/etc/doso-mqtt/tls/server.key",
                &portable(&self.path("server.key")),
            )
            .replace("log_type notice", "log_type all");
        fs::write(self.path("mosquitto.conf"), config)?;
        let mut command = Command::new(broker);
        command.arg("-c").arg(self.path("mosquitto.conf"));
        self.processes
            .push(Process::spawn(command, self.path("broker.log"))?);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.check()?;
            if self.processes.last().unwrap().logs().contains("running") {
                return Ok(port);
            }
            ensure!(
                Instant::now() < deadline,
                "broker startup timed out: {}",
                self.processes.last().unwrap().logs()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn check_auth(&mut self, port: u16, device: &str) -> Result<()> {
        let broker = executable("MOSQUITTO_BIN", "mosquitto")?;
        let publisher = broker.with_file_name(format!("mosquitto_pub{}", env::consts::EXE_SUFFIX));
        ensure!(publisher.is_file(), "missing {}", publisher.display());
        let openssl = executable("OPENSSL_BIN", "openssl")?;
        self.openssl(&openssl, &format!("req -x509 -newkey rsa:2048 -nodes -days 1 -keyout untrusted.key -out untrusted.pem -subj /CN={device}")).await?;
        let own = format!("doso/v1/devices/{device}/status/reports");
        let other = format!("doso/v1/devices/{}/status/reports", uuid::Uuid::new_v4());
        for (name, cert, key, topic, expected) in [
            (
                "valid",
                Some(format!("{device}.pem")),
                Some(format!("{device}.key")),
                own.as_str(),
                "allowed",
            ),
            ("no_certificate", None, None, own.as_str(), ""),
            (
                "untrusted_certificate",
                Some("untrusted.pem".into()),
                Some("untrusted.key".into()),
                own.as_str(),
                "",
            ),
            (
                "other_device",
                Some(format!("{device}.pem")),
                Some(format!("{device}.key")),
                other.as_str(),
                "denied",
            ),
        ] {
            let mut command = Command::new(&publisher);
            command
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &port.to_string(),
                    "-V",
                    "mqttv5",
                    "--cafile",
                ])
                .arg(self.path("ca.pem"))
                .args(["-q", "1", "-t", topic, "-m", "auth-check", "-d"]);
            if let (Some(cert), Some(key)) = (cert, key) {
                command
                    .arg("--cert")
                    .arg(self.path(&cert))
                    .arg("--key")
                    .arg(self.path(&key));
            }
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .spawn()
                .with_context(|| format!("run {name} MQTT check"))?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait()?.is_none() {
                if Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    bail!("{name} MQTT check timed out");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let output = child.wait_with_output()?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if expected.is_empty() {
                ensure!(
                    !output.status.success()
                        && !text.contains("received CONNACK (0)")
                        && (text.contains("SSL") || text.contains("TLS")),
                    "{name} unexpectedly connected: {text}"
                );
            } else {
                ensure!(
                    text.contains("received CONNACK (0)")
                        && if expected == "allowed" {
                            output.status.success()
                                && (text.contains("RC:0)") || text.contains("RC:16)"))
                        } else {
                            text.contains("RC:135)")
                        },
                    "{name} unexpected MQTT result: {text}"
                );
            }
            println!("PASS MQTT {name}");
        }
        Ok(())
    }

    pub fn close(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for process in self.processes.iter_mut().rev() {
            if let Err(error) = process.stop() {
                failures.push(error.to_string());
            }
        }
        ensure!(
            failures.is_empty(),
            "lab cleanup failed: {}",
            failures.join(", ")
        );
        self.processes.clear();
        if self.directory.exists() {
            fs::remove_dir_all(&self.directory).context("remove private lab directory")?;
        }
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("lab cleanup failed: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_uses_configured_cloud_switches_not_inherited_values() {
        for (values, expected) in [
            ([("DOSO_REMOTE_ENABLED", "true")].as_slice(), "|true"),
            (
                [("DOSO_CLOUD_ON", "false"), ("DOSO_REMOTE_ENABLED", "true")].as_slice(),
                "false|true",
            ),
        ] {
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("powershell.exe");
                command.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::Write(\"$env:DOSO_CLOUD_ON|$env:DOSO_REMOTE_ENABLED\")",
                ]);
                command
            };
            #[cfg(not(windows))]
            let mut command = {
                let mut command = Command::new("sh");
                command.args([
                    "-c",
                    "printf '%s|%s' \"${DOSO_CLOUD_ON-}\" \"${DOSO_REMOTE_ENABLED-}\"",
                ]);
                command
            };
            command.env("DOSO_CLOUD_ON", "inherited");
            command.env("DOSO_REMOTE_ENABLED", "inherited");
            let values = values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            apply_environment(&mut command, &values);
            let result = command.output().expect("start environment probe");
            assert!(result.status.success());
            assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
        }
    }
}
