use std::{fs, process::Command};

#[test]
fn startup_setting_is_off_by_default_and_prefers_the_new_name() {
    let choice = std::env::temp_dir().join(format!("cloud-choice-{}", uuid::Uuid::new_v4()));
    fs::write(&choice, "true\n").unwrap();
    let cases = [
        (None, None, false),
        (None, Some("true"), true),
        (Some("true"), Some("false"), true),
        (Some("false"), Some("true"), false),
        (Some("invalid"), Some("true"), false),
        (Some("TRUE"), None, false),
        (Some(" true "), None, false),
    ];
    for (current, legacy, enabled) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_doso-cloud-link"));
        command
            .env_remove("DOSO_CLOUD_ON")
            .env_remove("DOSO_REMOTE_ENABLED")
            .env("DOSO_CLOUD_CHOICE", &choice)
            // Missing identity makes an enabled startup stop before network access.
            .env_remove("DOSO_DEVICE_ID");
        if let Some(value) = current {
            command.env("DOSO_CLOUD_ON", value);
        }
        if let Some(value) = legacy {
            command.env("DOSO_REMOTE_ENABLED", value);
        }
        let output = command.output().expect("start cloud link");
        assert_eq!(output.status.code(), Some(if enabled { 78 } else { 0 }));
        if !enabled {
            assert_eq!(output.stdout, b"cloud_off\n");
        }
    }
    fs::remove_file(choice).unwrap();
}

#[test]
fn installation_gate_cannot_replace_local_choice() {
    let choice = std::env::temp_dir().join(format!("cloud-choice-{}", uuid::Uuid::new_v4()));
    for value in [None, Some("false"), Some("invalid")] {
        if let Some(value) = value {
            fs::write(&choice, value).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_doso-cloud-link"))
            .env("DOSO_CLOUD_ON", "true")
            .env("DOSO_CLOUD_CHOICE", &choice)
            .env_remove("DOSO_DEVICE_ID")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"cloud_off\n");
    }
    fs::remove_file(choice).unwrap();
}
