//! Automatic updates for official Windows builds.
//!
//! Official builds set, at build time, where to look
//! (`AUDIONET_UPDATE_URL`, a `latest.json` manifest) and whom to trust
//! (`AUDIONET_UPDATE_PUBLIC_KEY`, an Ed25519 public key in hex). Builds
//! without both never update themselves; nothing here names a particular
//! server.
//!
//! Security: the manifest must carry a valid Ed25519 signature
//! (`latest.json.sig`, hex) from the release key, whose private half never
//! leaves the release machine. The manifest pins the package's SHA-256 and
//! size, so a compromised web server can serve nothing the app will run.
//! Only strictly newer versions are installed (no downgrades), only over
//! HTTPS (plain HTTP only for localhost testing), and zip entries may only
//! be plain file names (no folders, no `..`).
//!
//! Installation: the package is downloaded to the temporary folder,
//! verified, extracted into a staging folder next to the program, and the
//! temporary zip is deleted. Then each program file is renamed aside
//! (`*.old-update`; Windows allows renaming a running program) and the new
//! file moved into place. Any failure puts everything back. The new copy is
//! started by the window code (`ui.rs`), which rolls back if it does not
//! confirm that it started; the new copy deletes the renamed old files.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The package this build installs.
pub const PRODUCT: &str = "audionet-windows-x64";
/// Upper bound on a package download.
const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024;
/// Suffix of program files renamed aside during an update.
const OLD_SUFFIX: &str = ".old-update";
/// Prefix of the staging folder next to the program.
const STAGING_PREFIX: &str = ".audionet-update-";

/// Where this build looks for updates and the key it trusts, if any.
pub fn configured() -> Option<(&'static str, &'static str)> {
    let url = option_env!("AUDIONET_UPDATE_URL").filter(|s| !s.is_empty())?;
    let key = option_env!("AUDIONET_UPDATE_PUBLIC_KEY").filter(|s| !s.is_empty())?;
    Some((url, key))
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The signed description of the newest release.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema: u32,
    pub product: String,
    pub version: String,
    /// Package file name, next to the manifest on the server.
    pub file: String,
    /// SHA-256 of the package, lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// A downloaded, verified and extracted update, ready to install.
#[derive(Debug)]
pub struct Prepared {
    pub version: String,
    pub staging: PathBuf,
    /// Plain file names inside `staging`.
    pub files: Vec<String>,
}

/// Files renamed aside by `install`, for `rollback`.
#[derive(Debug, Default)]
pub struct Installed {
    moves: Vec<(PathBuf, Option<PathBuf>)>,
}

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// Whether `candidate` is strictly newer than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks the signature over the exact manifest bytes, then its contents.
pub fn verify_manifest(
    bytes: &[u8],
    signature_hex: &str,
    public_key_hex: &str,
) -> Result<Manifest, String> {
    let key: [u8; 32] = from_hex(public_key_hex)
        .and_then(|k| k.try_into().ok())
        .ok_or("this build's update key is malformed")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "this build's update key is invalid")?;
    let sig: [u8; 64] = from_hex(signature_hex)
        .and_then(|s| s.try_into().ok())
        .ok_or("the update signature is malformed")?;
    key.verify_strict(bytes, &Signature::from_bytes(&sig))
        .map_err(|_| "the update is not signed by the AudioNet release key; it was ignored")?;
    let m: Manifest = serde_json::from_slice(bytes)
        .map_err(|e| format!("the update description is invalid: {e}"))?;
    if m.schema != 1 {
        return Err(format!(
            "the update description uses an unknown format ({})",
            m.schema
        ));
    }
    if m.product != PRODUCT {
        return Err(format!("the update is for {}, not {PRODUCT}", m.product));
    }
    if parse_version(&m.version).is_none() {
        return Err(format!(
            "the update has an invalid version \"{}\"",
            m.version
        ));
    }
    if !is_plain_file_name(&m.file) || !m.file.ends_with(".zip") {
        return Err("the update names an invalid package file".into());
    }
    if from_hex(&m.sha256).is_none_or(|h| h.len() != 32) {
        return Err("the update has an invalid checksum".into());
    }
    if m.size == 0 || m.size > MAX_PACKAGE_BYTES {
        return Err("the update package size is out of range".into());
    }
    Ok(m)
}

fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.starts_with('.')
}

/// HTTPS only, except plain HTTP to this computer for testing.
fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
    {
        Ok(())
    } else {
        Err(format!("updates must come over HTTPS, not {url}"))
    }
}

/// The package URL: `file` next to the manifest.
pub fn package_url(manifest_url: &str, file: &str) -> String {
    match manifest_url.rfind('/') {
        Some(i) => format!("{}{file}", &manifest_url[..=i]),
        None => file.to_owned(),
    }
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .into()
}

fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    check_url(url)?;
    let mut response = http()
        .get(url)
        .call()
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("could not read {url}: {e}"))
}

/// Fetches and verifies the manifest. `Ok(None)` when there is nothing
/// newer than this copy (or only the version `skip` names).
pub fn check(
    manifest_url: &str,
    public_key_hex: &str,
    skip: Option<&str>,
) -> Result<Option<Manifest>, String> {
    let bytes = get_bytes(manifest_url, 64 * 1024)?;
    let sig = get_bytes(&format!("{manifest_url}.sig"), 1024)?;
    let sig = String::from_utf8(sig).map_err(|_| "the update signature is malformed")?;
    let m = verify_manifest(&bytes, &sig, public_key_hex)?;
    let wanted = is_newer(&m.version, current_version()) && skip != Some(m.version.as_str());
    Ok(wanted.then_some(m))
}

/// Downloads the package to `temp_dir`, checks size and SHA-256, extracts
/// it into a staging folder in `install_dir`, and deletes the download.
pub fn prepare(
    manifest_url: &str,
    m: &Manifest,
    temp_dir: &Path,
    install_dir: &Path,
) -> Result<Prepared, String> {
    let url = package_url(manifest_url, &m.file);
    check_url(&url)?;
    let zip_path = temp_dir.join(format!("audionet-update-{}.zip", m.version));
    let result = download(&url, m, &zip_path).and_then(|()| {
        let staging = install_dir.join(format!("{STAGING_PREFIX}{}", m.version));
        let files = extract(&zip_path, &staging)?;
        Ok(Prepared {
            version: m.version.clone(),
            staging,
            files,
        })
    });
    // The download is never needed again, whatever happened.
    let _ = fs::remove_file(&zip_path);
    result
}

fn download(url: &str, m: &Manifest, to: &Path) -> Result<(), String> {
    let response = http()
        .get(url)
        .call()
        .map_err(|e| format!("could not download the update: {e}"))?;
    let mut body = response.into_body().into_reader();
    let mut file = fs::File::create(to).map_err(|e| format!("could not save the update: {e}"))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = body
            .read(&mut buf)
            .map_err(|e| format!("the update download was interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > m.size {
            return Err("the update package is larger than announced".into());
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("could not save the update: {e}"))?;
    }
    if total != m.size {
        return Err("the update package is smaller than announced".into());
    }
    if to_hex(&hasher.finalize()) != m.sha256.to_ascii_lowercase() {
        return Err("the update package does not match its signed checksum; it was ignored".into());
    }
    Ok(())
}

/// Extracts plain files only into a fresh `staging` folder.
pub fn extract(zip_path: &Path, staging: &Path) -> Result<Vec<String>, String> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging).map_err(|e| format!("could not prepare the update: {e}"))?;
    let file = fs::File::open(zip_path).map_err(|e| format!("could not open the update: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("the update package is not a valid zip: {e}"))?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("could not read the update package: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        if !is_plain_file_name(&name) {
            let _ = fs::remove_dir_all(staging);
            return Err(format!(
                "the update package contains an unexpected path \"{name}\""
            ));
        }
        let mut out = fs::File::create(staging.join(&name))
            .map_err(|e| format!("could not unpack the update: {e}"))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("could not unpack the update: {e}"))?;
        names.push(name);
    }
    if !names
        .iter()
        .any(|n| n.eq_ignore_ascii_case("audionet-desktop.exe"))
    {
        let _ = fs::remove_dir_all(staging);
        return Err("the update package does not contain the AudioNet app".into());
    }
    Ok(names)
}

/// Replaces the program files with the staged ones. On failure, puts back
/// everything already changed and returns the error.
pub fn install(prepared: &Prepared, install_dir: &Path) -> Result<Installed, String> {
    let mut done = Installed::default();
    for name in &prepared.files {
        let target = install_dir.join(name);
        let result = (|| -> std::io::Result<Option<PathBuf>> {
            let aside = if target.exists() {
                let aside = free_aside_name(&target);
                fs::rename(&target, &aside)?;
                Some(aside)
            } else {
                None
            };
            if let Err(e) = fs::rename(prepared.staging.join(name), &target) {
                if let Some(a) = &aside {
                    let _ = fs::rename(a, &target);
                }
                return Err(e);
            }
            Ok(aside)
        })();
        match result {
            Ok(aside) => done.moves.push((target, aside)),
            Err(e) => {
                rollback(done);
                return Err(format!(
                    "could not replace {name} in {}: {e}",
                    install_dir.display()
                ));
            }
        }
    }
    let _ = fs::remove_dir_all(&prepared.staging);
    Ok(done)
}

/// A name to rename `target` to; an earlier leftover may still be locked.
fn free_aside_name(target: &Path) -> PathBuf {
    let base = format!("{}{OLD_SUFFIX}", target.display());
    if fs::remove_file(&base).is_ok() || !Path::new(&base).exists() {
        return PathBuf::from(base);
    }
    (1..)
        .map(|n| PathBuf::from(format!("{base}-{n}")))
        .find(|p| fs::remove_file(p).is_ok() || !p.exists())
        .expect("an unused name")
}

/// Puts back the files `install` replaced.
pub fn rollback(installed: Installed) {
    for (target, aside) in installed.moves.into_iter().rev() {
        let _ = fs::remove_file(&target);
        if let Some(aside) = aside {
            let _ = fs::rename(&aside, &target);
        }
    }
}

/// Deletes leftovers of earlier updates: renamed old files (once the old
/// copy has exited) and staging folders.
pub fn clean_up(install_dir: &Path) {
    let Ok(entries) = fs::read_dir(install_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(OLD_SUFFIX) {
            let _ = fs::remove_file(entry.path());
        } else if name.starts_with(STAGING_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn manifest_json(version: &str, file: &str) -> Vec<u8> {
        format!(
            r#"{{"schema":1,"product":"audionet-windows-x64","version":"{version}","file":"{file}","sha256":"{}","size":10}}"#,
            "ab".repeat(32)
        )
        .into_bytes()
    }

    fn signed(bytes: &[u8]) -> (String, String) {
        let k = key();
        (
            to_hex(&k.sign(bytes).to_bytes()),
            to_hex(k.verifying_key().as_bytes()),
        )
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("0.2", "0.1.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
    }

    #[test]
    fn accepts_a_correctly_signed_manifest() {
        let bytes = manifest_json("0.3.0", "audionet-windows-x64-0.3.0.zip");
        let (sig, pk) = signed(&bytes);
        let m = verify_manifest(&bytes, &sig, &pk).unwrap();
        assert_eq!(m.version, "0.3.0");
    }

    #[test]
    fn rejects_tampering_and_wrong_keys() {
        let bytes = manifest_json("0.3.0", "audionet-windows-x64-0.3.0.zip");
        let (sig, pk) = signed(&bytes);
        let mut tampered = bytes.clone();
        tampered[40] ^= 1;
        assert!(
            verify_manifest(&tampered, &sig, &pk)
                .unwrap_err()
                .contains("not signed")
        );
        let other = to_hex(
            SigningKey::from_bytes(&[9u8; 32])
                .verifying_key()
                .as_bytes(),
        );
        assert!(
            verify_manifest(&bytes, &sig, &other)
                .unwrap_err()
                .contains("not signed")
        );
        assert!(verify_manifest(&bytes, "zz", &pk).is_err());
    }

    #[test]
    fn rejects_paths_in_the_package_name() {
        for file in [
            "../evil.zip",
            "sub/a.zip",
            "C:evil.zip",
            ".hidden.zip",
            "a.exe",
        ] {
            let bytes = manifest_json("0.3.0", file);
            let (sig, pk) = signed(&bytes);
            assert!(verify_manifest(&bytes, &sig, &pk).is_err(), "{file}");
        }
    }

    #[test]
    fn package_sits_next_to_the_manifest() {
        assert_eq!(
            package_url(
                "https://audionet.example.com/downloads/latest.json",
                "a.zip"
            ),
            "https://audionet.example.com/downloads/a.zip"
        );
        assert!(check_url("http://audionet.example.com/latest.json").is_err());
        assert!(check_url("http://127.0.0.1:8750/latest.json").is_ok());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "audionet-update-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn extract_install_and_clean_up() {
        let dir = temp_dir("install");
        fs::write(dir.join("audionet-desktop.exe"), b"old app").unwrap();
        fs::write(dir.join("README.txt"), b"old readme").unwrap();
        let zip_path = dir.join("pkg.zip");
        write_zip(
            &zip_path,
            &[
                ("audionet-desktop.exe", b"new app"),
                ("audionet.exe", b"new cli"),
            ],
        );
        let staging = dir.join(format!("{STAGING_PREFIX}9.9.9"));
        let files = extract(&zip_path, &staging).unwrap();
        let prepared = Prepared {
            version: "9.9.9".into(),
            staging: staging.clone(),
            files,
        };
        let installed = install(&prepared, &dir).unwrap();
        assert_eq!(
            fs::read(dir.join("audionet-desktop.exe")).unwrap(),
            b"new app"
        );
        assert_eq!(fs::read(dir.join("audionet.exe")).unwrap(), b"new cli");
        assert_eq!(fs::read(dir.join("README.txt")).unwrap(), b"old readme");
        assert!(dir.join("audionet-desktop.exe.old-update").exists());
        assert!(!staging.exists());
        // Rollback restores exactly the previous state.
        rollback(installed);
        assert_eq!(
            fs::read(dir.join("audionet-desktop.exe")).unwrap(),
            b"old app"
        );
        assert!(!dir.join("audionet.exe").exists());
        clean_up(&dir);
        assert!(!dir.join("audionet-desktop.exe.old-update").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_refuses_paths_and_packages_without_the_app() {
        let dir = temp_dir("extract");
        let staging = dir.join("stage");
        let bad = dir.join("bad.zip");
        write_zip(
            &bad,
            &[("audionet-desktop.exe", b"x"), ("../escape.txt", b"x")],
        );
        assert!(
            extract(&bad, &staging)
                .unwrap_err()
                .contains("unexpected path")
        );
        assert!(!dir.join("escape.txt").exists());
        let empty = dir.join("empty.zip");
        write_zip(&empty, &[("README.txt", b"x")]);
        assert!(
            extract(&empty, &staging)
                .unwrap_err()
                .contains("does not contain")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
