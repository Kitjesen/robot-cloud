use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Choice(PathBuf);

impl Choice {
    pub fn from_env() -> Self {
        Self(
            env::var_os("DOSO_CLOUD_CHOICE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/doso-cloud/choice")),
        )
    }

    pub fn enabled(&self) -> bool {
        let Ok(metadata) = fs::symlink_metadata(&self.0) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o022 != 0 {
                return false;
            }
        }
        let mut bytes = Vec::new();
        fs::File::open(&self.0)
            .and_then(|file| file.take(6).read_to_end(&mut bytes))
            .is_ok()
            && matches!(bytes.as_slice(), b"true" | b"true\n")
    }

    pub fn disable(&self) -> std::io::Result<()> {
        let parent = self.0.parent().unwrap_or(Path::new("."));
        let (mode, exists) = match fs::symlink_metadata(&self.0) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(std::io::Error::other("cloud choice is not a regular file"));
                }
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o777
                };
                #[cfg(not(unix))]
                let mode = 0o600;
                (mode, true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (0o600, false),
            Err(error) => return Err(error),
        };
        #[cfg(not(unix))]
        let _ = mode;
        let name = self
            .0
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("choice");
        let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
            }
            file.write_all(b"false\n")?;
            file.sync_all()?;
            if exists {
                let current = fs::symlink_metadata(&self.0)?;
                if !current.is_file() {
                    return Err(std::io::Error::other(
                        "cloud choice changed to a non-regular file",
                    ));
                }
            }
            #[cfg(windows)]
            if exists {
                fs::remove_file(&self.0)?;
            }
            fs::rename(&temporary, &self.0)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub async fn closed(&self) {
        while self.enabled() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_invalid_and_changed_choice_stop_connection() {
        let path = env::temp_dir().join(format!("cloud-choice-{}", uuid::Uuid::new_v4()));
        let choice = Choice(path.clone());
        assert!(!choice.enabled());
        for value in ["false", "TRUE", " true ", "true\nextra", ""] {
            fs::write(&path, value).unwrap();
            assert!(!choice.enabled());
        }
        fs::write(&path, "true\n").unwrap();
        assert!(Choice(path.clone()).enabled());
        let close = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            fs::write(&path, "false\n").unwrap();
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(choice.closed(), close);
        })
        .await
        .unwrap();
        assert!(!Choice(path.clone()).enabled());
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_writable_or_linked_choice() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let path = env::temp_dir().join(format!("cloud-choice-{}", uuid::Uuid::new_v4()));
        let link = path.with_extension("link");
        fs::write(&path, "true").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(!Choice(path.clone()).enabled());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Choice(path.clone()).enabled());
        symlink(&path, &link).unwrap();
        assert!(!Choice(link.clone()).enabled());
        fs::remove_file(link).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn disable_persists_false_without_following_a_link() {
        let path = env::temp_dir().join(format!("cloud-choice-{}", uuid::Uuid::new_v4()));
        fs::write(&path, "true\n").unwrap();
        Choice(path.clone()).disable().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "false\n");
        assert!(!Choice(path.clone()).enabled());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = path.with_extension("target");
            let link = path.with_extension("link");
            fs::write(&target, "true\n").unwrap();
            symlink(&target, &link).unwrap();
            assert!(Choice(link.clone()).disable().is_err());
            assert_eq!(fs::read_to_string(&target).unwrap(), "true\n");
            fs::remove_file(&link).unwrap();
            fs::remove_file(&target).unwrap();
        }
        fs::remove_file(path).unwrap();
    }
}
