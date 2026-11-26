use crate::download;
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

/// Validates a relative archive path to prevent absolute paths, drive prefixes, and `..` traversal.
pub fn validate_relative_entry_path(raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    ensure!(!trimmed.is_empty(), "archive_empty_entry_path");
    ensure!(!trimmed.contains('\0'), "archive_nul_entry_path");
    ensure!(
        !trimmed.starts_with('/') && !trimmed.starts_with('\\'),
        "archive_absolute_path_rejected: {trimmed}"
    );
    // Reject Windows drive letters like C: or C:\ even when inspecting on Linux.
    let bytes = trimmed.as_bytes();
    ensure!(
        !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'),
        "archive_drive_prefix_rejected: {trimmed}"
    );
    let normalized = trimmed.replace('\\', "/");
    let mut clean = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => bail!("archive_path_traversal_rejected: {trimmed}"),
            segment => clean.push(segment),
        }
    }
    Ok(clean)
}

/// Validates a symlink target inside an archive so that resolving it relative to its entry's parent
/// stays strictly inside the extraction root.
fn validate_symlink_target(entry_rel: &Path, target: &str) -> Result<()> {
    ensure!(!target.is_empty(), "archive_empty_symlink_target");
    ensure!(
        !target.starts_with('/') && !target.starts_with('\\'),
        "archive_absolute_symlink_rejected: {target}"
    );
    let bytes = target.as_bytes();
    ensure!(
        !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'),
        "archive_drive_symlink_rejected: {target}"
    );
    let mut depth: Vec<String> = entry_rel
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    for segment in target.replace('\\', "/").split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                if depth.pop().is_none() {
                    bail!("archive_symlink_escape_rejected: {target}");
                }
            }
            s => depth.push(s.to_owned()),
        }
    }
    Ok(())
}

/// Inspects a `.tar.gz` archive before extraction, rejecting path traversal, hardlinks,
/// device nodes, FIFOs, and symlinks escaping the extraction root.
pub fn validate_tar_gz(archive: &Path) -> Result<()> {
    let list_out = Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .output()
        .with_context(|| format!("tar -tzf {}", archive.display()))?;
    ensure!(
        list_out.status.success(),
        "archive_list_failed: {}",
        String::from_utf8_lossy(&list_out.stderr).trim()
    );
    let paths: Vec<String> = String::from_utf8_lossy(&list_out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect();
    ensure!(!paths.is_empty(), "archive_empty");
    let mut rel_paths = Vec::with_capacity(paths.len());
    for raw in &paths {
        rel_paths.push(validate_relative_entry_path(raw)?);
    }

    let verbose_out = Command::new("tar")
        .arg("-tzvf")
        .arg(archive)
        .output()
        .with_context(|| format!("tar -tzvf {}", archive.display()))?;
    ensure!(
        verbose_out.status.success(),
        "archive_verbose_list_failed: {}",
        String::from_utf8_lossy(&verbose_out.stderr).trim()
    );
    let verbose_lines: Vec<String> = String::from_utf8_lossy(&verbose_out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect();
    ensure!(
        verbose_lines.len() == rel_paths.len(),
        "archive_listing_mismatch"
    );
    for (line, rel_path) in verbose_lines.iter().zip(rel_paths.iter()) {
        let kind = line.chars().next().unwrap_or('?');
        match kind {
            '-' | 'd' => {}
            'l' => {
                let target = line
                    .split_once(" -> ")
                    .map(|(_, t)| t.trim())
                    .context("archive_symlink_target_unparseable")?;
                validate_symlink_target(rel_path, target)?;
            }
            'h' => bail!("archive_hardlink_rejected: {}", rel_path.display()),
            _ => bail!(
                "archive_unsupported_entry_type({kind}): {}",
                rel_path.display()
            ),
        }
    }
    Ok(())
}

/// Safely unpacks a validated `.tar.gz` into a fresh temporary directory next to `dest`,
/// audits the extracted tree, optionally verifies `SHA256SUMS`, and returns the temp directory.
fn extract_to_temp(archive: &Path, dest: &Path) -> Result<PathBuf> {
    validate_tar_gz(archive)?;
    let parent = dest.parent().context("archive_dest_has_no_parent")?;
    fs::create_dir_all(parent)?;
    let temp_dir = parent.join(format!(
        ".{}.extract.{}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("ota"),
        Uuid::new_v4()
    ));
    fs::create_dir_all(&temp_dir)?;
    let result = (|| -> Result<()> {
        let mut cmd = Command::new("tar");
        cmd.arg("-xzf").arg(archive).arg("-C").arg(&temp_dir);
        #[cfg(unix)]
        cmd.arg("--no-same-owner");
        let output = cmd
            .output()
            .with_context(|| format!("tar -xzf {}", archive.display()))?;
        ensure!(
            output.status.success(),
            "archive_extract_failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        audit_extracted_tree(&temp_dir)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }
    Ok(temp_dir)
}

/// Defense-in-depth post-extraction check ensuring no extracted entry escapes `root`.
fn audit_extracted_tree(root: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(root)?;
    audit_dir_recursive(&canonical_root, root)
}

fn audit_dir_recursive(canonical_root: &Path, current: &Path) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let link_target = fs::read_link(&path)?;
            ensure!(
                !link_target.is_absolute(),
                "extracted_absolute_symlink_rejected: {}",
                path.display()
            );
            let resolved = path.parent().unwrap_or(canonical_root).join(&link_target);
            if let Ok(canon) = fs::canonicalize(&resolved) {
                ensure!(
                    canon.starts_with(canonical_root),
                    "extracted_symlink_escape_rejected: {}",
                    path.display()
                );
            } else {
                // Dangling relative symlink: verify lexical depth stays within root.
                let rel = path.strip_prefix(canonical_root).unwrap_or(&path);
                validate_symlink_target(rel, &link_target.to_string_lossy())?;
            }
        } else if ft.is_dir() {
            audit_dir_recursive(canonical_root, &path)?;
        } else if !ft.is_file() {
            bail!("extracted_special_file_rejected: {}", path.display());
        }
    }
    Ok(())
}

/// If `root` does not satisfy `predicate` directly, checks whether `root` wraps a single
/// top-level directory that satisfies `predicate` (common for `tar -czf release.tar.gz v1.0.1/`).
fn resolve_payload_root(root: &Path, predicate: impl Fn(&Path) -> bool) -> PathBuf {
    if predicate(root) {
        return root.to_path_buf();
    }
    if let Ok(entries) = fs::read_dir(root) {
        let dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        if dirs.len() == 1 && predicate(&dirs[0]) {
            return dirs[0].clone();
        }
    }
    root.to_path_buf()
}

/// Verifies `SHA256SUMS` inside `root` when present.
pub fn verify_sha256sums_if_present(root: &Path) -> Result<()> {
    let sums_path = root.join("SHA256SUMS");
    if !sums_path.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(&sums_path).context("read SHA256SUMS")?;
    let mut checked = 0usize;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let expected_hash = parts.next().context("sha256sums_missing_hash")?.trim();
        let raw_file = parts
            .next()
            .context("sha256sums_missing_file")?
            .trim()
            .trim_start_matches('*');
        ensure!(
            expected_hash.len() == 64 && expected_hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "sha256sums_invalid_hash"
        );
        let rel = validate_relative_entry_path(raw_file)?;
        ensure!(!rel.as_os_str().is_empty(), "sha256sums_empty_path");
        let file_path = root.join(&rel);
        ensure!(
            file_path.is_file(),
            "sha256sums_missing_target: {}",
            rel.display()
        );
        let size = fs::metadata(&file_path)?.len();
        download::verify_hash(&file_path, size, Some(&expected_hash.to_ascii_lowercase()))
            .with_context(|| format!("sha256sums_mismatch: {}", rel.display()))?;
        checked += 1;
    }
    ensure!(checked > 0, "sha256sums_empty");
    Ok(())
}

/// Safely unpacks a Channel A `brainstem_release` `.tar.gz` into `release_dir` atomically,
/// verifying `bin/doso-brainstem` and optional `SHA256SUMS`.
pub fn extract_release_tar_gz(archive: &Path, release_dir: &Path) -> Result<()> {
    let temp_dir = extract_to_temp(archive, release_dir)?;
    let result = (|| -> Result<()> {
        let payload = resolve_payload_root(&temp_dir, |dir| {
            dir.join("bin/doso-brainstem").is_file() || dir.join("bin/doso-brainstem.exe").is_file()
        });
        ensure!(
            payload.join("bin/doso-brainstem").is_file()
                || payload.join("bin/doso-brainstem.exe").is_file(),
            "release_missing_doso_brainstem_binary"
        );
        verify_sha256sums_if_present(&payload)?;
        swap_extracted_dir(&payload, release_dir)
    })();
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

/// Safely unpacks a multi-component `component_release` `.tar.gz` into `release_dir` atomically.
/// For `component == "brainstem"`, enforces the `bin/doso-brainstem` presence check; for other
/// components (`cloud-link`, `ota`, `voice`, `nav`, ...), verifies optional `SHA256SUMS` and
/// ensures the extracted tree is non-empty.
pub fn extract_component_tar_gz(archive: &Path, release_dir: &Path, component: &str) -> Result<()> {
    if component == "brainstem" {
        return extract_release_tar_gz(archive, release_dir);
    }
    let temp_dir = extract_to_temp(archive, release_dir)?;
    let result = (|| -> Result<()> {
        let payload = resolve_payload_root(&temp_dir, |dir| {
            dir.join("bin").is_dir() || dir.join("SHA256SUMS").is_file()
        });
        let has_entries = fs::read_dir(&payload)?.next().is_some();
        ensure!(has_entries, "component_release_empty");
        verify_sha256sums_if_present(&payload)?;
        swap_extracted_dir(&payload, release_dir)
    })();
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn swap_extracted_dir(payload: &Path, release_dir: &Path) -> Result<()> {
    // Replace any stale directory at `release_dir` safely via backup swap.
    let backup = if release_dir.exists() {
        let old = release_dir.with_file_name(format!(
            ".{}.old.{}",
            release_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("release"),
            Uuid::new_v4()
        ));
        fs::rename(release_dir, &old)?;
        Some(old)
    } else {
        None
    };
    let move_res = fs::rename(payload, release_dir);
    if let Err(error) = move_res {
        if let Some(old) = backup {
            let _ = fs::rename(&old, release_dir);
        }
        return Err(error.into());
    }
    if let Some(old) = backup {
        let _ = fs::remove_dir_all(old);
    }
    Ok(())
}

/// Safely unpacks a Channel B `policy_package` `.tar.gz` into `dest_dir`,
/// validating that `policy.yaml` and its referenced ONNX model exist inside the extracted tree.
pub fn extract_policy_tar_gz(archive: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let temp_dir = extract_to_temp(archive, dest_dir)?;
    let result = (|| -> Result<()> {
        let payload = resolve_payload_root(&temp_dir, |dir| dir.join("policy.yaml").is_file());
        ensure!(
            payload.join("policy.yaml").is_file(),
            "policy_package_missing_policy_yaml"
        );
        verify_sha256sums_if_present(&payload)?;
        if dest_dir.exists() {
            fs::remove_dir_all(dest_dir)?;
        }
        fs::rename(&payload, dest_dir)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&temp_dir);
    result?;
    Ok(dest_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_and_absolute_paths() {
        assert!(validate_relative_entry_path("../etc/passwd").is_err());
        assert!(validate_relative_entry_path("bin/../../evil").is_err());
        assert!(validate_relative_entry_path("/etc/shadow").is_err());
        assert!(validate_relative_entry_path("C:\\Windows\\evil").is_err());
        assert!(validate_relative_entry_path("bin/doso-brainstem").is_ok());
        assert!(validate_relative_entry_path("./bin/doso-brainstem").is_ok());
    }

    #[test]
    fn validates_symlink_targets_within_root() {
        assert!(validate_symlink_target(Path::new("lib/liba.so"), "liba.so.1").is_ok());
        assert!(validate_symlink_target(Path::new("lib/sub/a"), "../b").is_ok());
        assert!(validate_symlink_target(Path::new("lib/a"), "../../etc/passwd").is_err());
        assert!(validate_symlink_target(Path::new("lib/a"), "/etc/passwd").is_err());
    }

    #[test]
    fn extracts_valid_release_and_verifies_sha256sums() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        let bin_bytes = b"fake-brainstem-elf";
        fs::write(src.join("bin/doso-brainstem"), bin_bytes).unwrap();
        let hash = crate::Manifest::artifact_hash(bin_bytes);
        fs::write(
            src.join("SHA256SUMS"),
            format!("{hash}  bin/doso-brainstem\n"),
        )
        .unwrap();

        let archive = dir.path().join("release.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());

        let dest = dir.path().join("releases/1.0.1");
        extract_release_tar_gz(&archive, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("bin/doso-brainstem")).unwrap(),
            bin_bytes
        );
    }

    #[test]
    fn rejects_corrupted_sha256sums() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin/doso-brainstem"), b"actual").unwrap();
        fs::write(
            src.join("SHA256SUMS"),
            format!("{}  bin/doso-brainstem\n", "0".repeat(64)),
        )
        .unwrap();

        let archive = dir.path().join("bad.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());

        let dest = dir.path().join("releases/1.0.1");
        assert!(extract_release_tar_gz(&archive, &dest).is_err());
        assert!(!dest.exists());
    }
}
