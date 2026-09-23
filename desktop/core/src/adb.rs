//! Обёртка над `adb`: список устройств, установка APK, разрешения, запуск, проброс порта.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PACKAGE: &str = "dev.escanor";
const ACTIVITY: &str = "dev.escanor/.MainActivity";
const PERMISSIONS: [&str; 3] =
    ["android.permission.CAMERA", "android.permission.RECORD_AUDIO", "android.permission.POST_NOTIFICATIONS"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    /// "device", "unauthorized", "offline", …
    pub state: String,
    pub model: Option<String>,
}

impl AdbDevice {
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }

    pub fn label(&self) -> String {
        match &self.model {
            Some(model) => format!("{} ({})", model.replace('_', " "), self.serial),
            None => self.serial.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Adb {
    path: PathBuf,
}

impl Adb {
    /// Ищет adb рядом с программой (дистрибутив для Windows кладёт его туда), иначе берёт из PATH.
    pub fn locate() -> Self {
        let name = if cfg!(windows) { "adb.exe" } else { "adb" };
        let bundled = std::env::current_exe().ok().and_then(|exe| {
            let dir = exe.parent()?.to_path_buf();
            [dir.join(name), dir.join("platform-tools").join(name)].into_iter().find(|p| p.is_file())
        });
        Self { path: bundled.unwrap_or_else(|| PathBuf::from(name)) }
    }

    fn command(&self) -> Command {
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut cmd = Command::new(&self.path);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    fn run(&self, serial: Option<&str>, args: &[&str]) -> Result<String> {
        let mut cmd = self.command();
        if let Some(serial) = serial {
            cmd.args(["-s", serial]);
        }
        let output =
            cmd.args(args).output().with_context(|| format!("не удалось запустить {}", self.path.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            // Разные версии adb пишут причину то в stderr, то в stdout.
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("adb {}: {} {}", args.join(" "), stderr.trim(), stdout.trim());
        }
        Ok(stdout)
    }

    pub fn devices(&self) -> Result<Vec<AdbDevice>> {
        Ok(parse_devices(&self.run(None, &["devices", "-l"])?))
    }

    pub fn is_installed(&self, serial: &str) -> Result<bool> {
        let out = self.run(Some(serial), &["shell", "pm", "path", PACKAGE]).unwrap_or_default();
        Ok(out.contains("package:"))
    }

    /// SHA-256 установленного APK (считает сам телефон, sha256sum есть в Android).
    /// `None` — не удалось узнать.
    pub fn installed_apk_hash(&self, serial: &str) -> Option<String> {
        let out = self.run(Some(serial), &["shell", "pm", "path", PACKAGE]).ok()?;
        let path = out.lines().find_map(|l| l.strip_prefix("package:"))?.trim().to_string();
        let out = self.run(Some(serial), &["shell", "sha256sum", &path]).ok()?;
        out.split_whitespace().next().map(str::to_lowercase)
    }

    pub fn install(&self, serial: &str, apk: &Path) -> Result<()> {
        let apk = apk.to_string_lossy();
        match self.run(Some(serial), &["install", "-r", "-g", &apk]) {
            Ok(_) => Ok(()),
            // Установленная версия подписана другим ключом (например, собрана до перехода на общий ключ):
            // поверх её не поставить. Своих данных у приложения нет, поэтому удаляем и ставим заново.
            Err(e) if format!("{e:#}").contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") => {
                self.run(Some(serial), &["uninstall", PACKAGE])?;
                self.run(Some(serial), &["install", "-g", &apk])?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Выдаёт разрешения и запускает приложение: оно поднимает foreground-сервис с сервером.
    pub fn launch(&self, serial: &str) -> Result<()> {
        for permission in PERMISSIONS {
            // POST_NOTIFICATIONS и т.п. могут быть недоступны для выдачи — это не критично.
            let _ = self.run(Some(serial), &["shell", "pm", "grant", PACKAGE, permission]);
        }
        self.run(Some(serial), &["shell", "am", "start", "-n", ACTIVITY, "--ez", "autostart", "true"])?;
        Ok(())
    }

    pub fn forward(&self, serial: &str, local: u16, remote: u16) -> Result<()> {
        self.run(Some(serial), &["forward", &format!("tcp:{local}"), &format!("tcp:{remote}")])?;
        Ok(())
    }

    pub fn remove_forward(&self, serial: &str, local: u16) {
        let _ = self.run(Some(serial), &["forward", "--remove", &format!("tcp:{local}")]);
    }
}

pub fn file_hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(std::fs::read(path)?);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// APK для установки: рядом с программой, а при разработке — результат сборки Gradle.
pub fn find_apk() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("escanor.apk")));
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../android/app/build/outputs/apk/debug/app-debug.apk");
    beside.into_iter().chain([dev]).find(|p| p.is_file())
}

fn parse_devices(output: &str) -> Vec<AdbDevice> {
    output
        .lines()
        .skip_while(|line| !line.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts.find_map(|p| p.strip_prefix("model:")).map(str::to_string);
            Some(AdbDevice { serial, state, model })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_sha256sum_format() {
        let path = std::env::temp_dir().join("escanor-hash-test.txt");
        std::fs::write(&path, "abc").unwrap();
        // Так же выводит `sha256sum` на телефоне: 64 строчные шестнадцатеричные цифры.
        assert_eq!(file_hash(&path).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parses_device_list() {
        let out = "* daemon started successfully\nList of devices attached\n\
                   0A081FDD4000ZX         device usb:1-1 product:redfin model:Pixel_5 device:redfin transport_id:3\n\
                   emulator-5554          unauthorized transport_id:1\n\n";
        let devices = parse_devices(out);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "0A081FDD4000ZX");
        assert!(devices[0].is_ready());
        assert_eq!(devices[0].label(), "Pixel 5 (0A081FDD4000ZX)");
        assert_eq!(devices[1].state, "unauthorized");
        assert_eq!(devices[1].model, None);
    }
}
