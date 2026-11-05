use crate::{atomic_write, home};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct LoginItem {
    label: String,
    program_arguments: Vec<PathBuf>,
    run_at_load: bool,
}

fn path() -> Result<PathBuf> {
    Ok(home()?.join("Library/LaunchAgents/gg.dreamfactory.pay-attention.plist"))
}

// launchd doesn't resolve relative paths against the caller, so a bad path fails here and keeps the old registration.
pub fn enable(exe: &Path) -> Result<()> {
    let exe = exe.canonicalize()?;
    anyhow::ensure!(exe.is_file(), "{} is not a file", exe.display());
    let mut bytes = Vec::new();
    plist::to_writer_xml(
        &mut bytes,
        &LoginItem {
            label: "gg.dreamfactory.pay-attention".into(),
            program_arguments: vec![exe],
            run_at_load: true,
        },
    )?;
    atomic_write(&path()?, &bytes)
}

pub fn disable() -> Result<()> {
    match fs::remove_file(path()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

// True only when the registration launches this copy; another copy's shows as off, and turning it on retargets.
pub fn is_enabled(exe: &Path) -> bool {
    let Ok(exe) = exe.canonicalize() else {
        return false;
    };
    path()
        .ok()
        .and_then(|path| plist::from_file::<_, LoginItem>(path).ok())
        .is_some_and(|item| item.program_arguments.first() == Some(&exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn registration_is_escaped_atomic_and_validated() {
        if std::env::var_os("PAY_ATTENTION_TEST_LOGIN_ITEM").is_none() {
            let home = tempfile::tempdir().unwrap();
            // Pipes, not inherited fds: the child's RLIMIT_FSIZE would also fail its own writes to a log file.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "login_item::tests::registration_is_escaped_atomic_and_validated",
                ])
                .env("HOME", home.path())
                .env("PAY_ATTENTION_TEST_LOGIN_ITEM", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let path = path().unwrap();
        let exe = home().unwrap().join("Pay & <Attention> \"app\"");
        fs::write(&exe, "executable").unwrap();
        assert!(!is_enabled(&exe));
        disable().unwrap();
        enable(&exe).unwrap();
        assert!(is_enabled(&exe));
        let item: LoginItem = plist::from_file(&path).unwrap();
        assert_eq!(item.label, "gg.dreamfactory.pay-attention");
        assert_eq!(item.program_arguments, [exe.canonicalize().unwrap()]);
        assert!(item.run_at_load);
        let saved = fs::read(&path).unwrap();
        assert!(enable(Path::new("missing/pay-attention-app")).is_err());
        assert!(enable(&home().unwrap()).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        let other = home().unwrap().join("other copy");
        fs::write(&other, "executable").unwrap();
        assert!(!is_enabled(&other));
        let inode = fs::metadata(&path).unwrap().ino();
        enable(&exe).unwrap();
        assert_ne!(fs::metadata(&path).unwrap().ino(), inode);
        let parent = path.parent().unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(enable(&exe).is_err());
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), saved);
        // Force a partial temporary-file write; the old registration must survive.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 32,
                rlim_max: 32,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
        assert!(enable(&exe).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
        assert!(is_enabled(&exe));
        fs::remove_file(&exe).unwrap();
        assert!(!is_enabled(&exe));
        fs::write(&path, "invalid plist").unwrap();
        assert!(!is_enabled(&other));
        disable().unwrap();
        assert!(!path.exists());
        assert!(!is_enabled(&other));
        disable().unwrap();
    }
}
