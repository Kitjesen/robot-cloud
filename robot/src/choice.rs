use std::{env, fs, io::Read, path::PathBuf, time::Duration};

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
}
