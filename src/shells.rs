//! The shells Wisp can open: whichever of PowerShell 7, Windows PowerShell,
//! Command Prompt, Git Bash and the WSL distributions are installed.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// What the new-tab menu and the tab title show.
    pub name: String,
    pub command: String,
}

/// Installed shells, the default first: PowerShell 7 if present, otherwise
/// Windows PowerShell.
pub fn detect() -> Vec<Shell> {
    let mut shells = Vec::new();
    let mut add = |name: &str, command: String| shells.push(Shell { name: name.into(), command });
    if let Some(pwsh) = find_on_path("pwsh.exe").or_else(|| existing(r"C:\Program Files\PowerShell\7\pwsh.exe")) {
        add("PowerShell", format!("\"{}\" -NoLogo", pwsh.display()));
    }
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join("System32");
    let windows_powershell = system.join(r"WindowsPowerShell\v1.0\powershell.exe");
    if windows_powershell.is_file() {
        add("Windows PowerShell", format!("\"{}\" -NoLogo", windows_powershell.display()));
    }
    let cmd = std::env::var("ComSpec").unwrap_or_else(|_| system.join("cmd.exe").display().to_string());
    add("Command Prompt", format!("\"{cmd}\""));
    if let Some(bash) = git_bash() {
        add("Git Bash", format!("\"{}\" --login -i", bash.display()));
    }
    for distro in wsl_distributions() {
        // wsl.exe keeps quotes as part of the name; WSL names can't have spaces anyway.
        add(&distro, format!("wsl.exe -d {distro} --cd ~"));
    }
    shells
}

/// The default shell's command line.
pub fn default_command() -> String {
    // Windows PowerShell exists on every Windows install, so detect() is never empty.
    detect().into_iter().next().map(|s| s.command).unwrap_or_else(|| "powershell.exe -NoLogo".into())
}

/// A display name for a command line: the matching detected shell's name, or
/// the program's file name.
pub fn name_for(command: &str) -> String {
    if let Some(shell) = detect().into_iter().find(|s| s.command == command) {
        return shell.name;
    }
    let program = command.trim_start_matches('"').split(['"', ' ']).next().unwrap_or(command);
    let file = Path::new(program).file_stem().map(|s| s.to_string_lossy().into_owned());
    file.unwrap_or_else(|| program.to_owned())
}

fn existing(path: &str) -> Option<PathBuf> {
    Some(PathBuf::from(path)).filter(|p| p.is_file())
}

fn find_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(exe)).find(|p| p.is_file())
}

/// Git for Windows records its install folder in the registry.
fn git_bash() -> Option<PathBuf> {
    let installed =
        registry_string(HKEY_LOCAL_MACHINE, r"SOFTWARE\GitForWindows", "InstallPath").map(PathBuf::from).or_else(
            || existing(r"C:\Program Files\Git\bin\bash.exe").map(|p| p.parent().unwrap().parent().unwrap().into()),
        )?;
    Some(installed.join(r"bin\bash.exe")).filter(|p| p.is_file())
}

/// Each registered WSL distribution, minus Docker Desktop's internal ones.
fn wsl_distributions() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let mut lxss = HKEY::default();
        let path = w!(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
        if RegOpenKeyExW(HKEY_CURRENT_USER, path, None, KEY_READ, &mut lxss) != ERROR_SUCCESS {
            return names;
        }
        for index in 0.. {
            let mut buf = [0u16; 128];
            let mut len = buf.len() as u32;
            let found = RegEnumKeyExW(lxss, index, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None);
            if found != ERROR_SUCCESS {
                break;
            }
            let subkey = String::from_utf16_lossy(&buf[..len as usize]);
            if let Some(name) = registry_string(lxss, &subkey, "DistributionName")
                && !name.starts_with("docker-desktop")
            {
                names.push(name);
            }
        }
        let _ = RegCloseKey(lxss);
    }
    names
}

fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let found = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            PCWSTR(HSTRING::from(value).as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if found != ERROR_SUCCESS {
        return None;
    }
    let units = (size as usize / 2).saturating_sub(1); // drop the terminating NUL
    Some(String::from_utf16_lossy(&buf[..units]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::term::Pane;
    use libghostty_vt::terminal::SizeReportSize;
    use std::time::{Duration, Instant};

    #[test]
    fn finds_the_shells_every_windows_has() {
        let names: Vec<String> = detect().into_iter().map(|s| s.name).collect();
        assert!(names.contains(&"Windows PowerShell".into()), "{names:?}");
        assert!(names.contains(&"Command Prompt".into()), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("docker-desktop")));
        assert_eq!(name_for(r#""C:\tools\nu.exe" -l"#), "nu");
    }

    /// Every detected shell starts in a pane and runs a command. WSL can take
    /// a few seconds to boot the first time.
    #[test]
    fn every_detected_shell_runs() {
        for shell in detect() {
            let marker = "wisp-shell-ok";
            let program =
                shell.command.trim_end_matches(" -NoLogo").trim_end_matches(" --login -i").trim_end_matches(" --cd ~");
            let command = match shell.name.as_str() {
                "PowerShell" | "Windows PowerShell" => format!("{program} -NoProfile -Command Write-Output {marker}"),
                "Command Prompt" => format!("{program} /c echo {marker}"),
                "Git Bash" => format!("{program} -c 'echo {marker}'"),
                // A WSL distribution; Wisp's variables have to cross into Linux too.
                _ => format!("{program} -e sh -c \"echo $TERM_PROGRAM-{marker}\""),
            };
            let size = SizeReportSize { rows: 10, columns: 80, cell_width: 9, cell_height: 19 };
            let expected = if shell.command.starts_with("wsl.exe") { format!("wisp-{marker}") } else { marker.into() };
            let mut pane = Pane::spawn(&command, None, size, &Config::default(), || {}, || {}).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                pane.pump();
                if pane.screen_text().contains(&expected) {
                    break;
                }
                assert!(Instant::now() < deadline, "{} never printed the marker: {:?}", shell.name, pane.screen_text());
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
