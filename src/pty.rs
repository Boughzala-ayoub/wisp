//! ConPTY: run a child process attached to a Windows pseudoconsole.
//!
//! Windows has no Unix-style pty. Instead, `CreatePseudoConsole` starts a
//! hidden conhost that hosts the child's console and talks VT to us over two
//! pipes: we write keystrokes into one and read rendered output from the other.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::FromRawHandle;
use std::thread;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR, Result};

/// A child process running inside a pseudoconsole.
pub struct Pty {
    hpc: HPCON,
    input: File,
    /// Owned here until [`Pty::on_exit`] hands it to the watcher thread.
    process: Option<SendHandle>,
}

impl Pty {
    /// Starts `command_line` in a `cols` x `rows` pseudoconsole.
    ///
    /// Returns the pty plus the read end of its output pipe. Read that pipe on a
    /// dedicated thread and keep draining it until EOF: conhost blocks when the
    /// pipe is full, and [`Pty`]'s drop waits on conhost.
    pub fn spawn(command_line: &str, cwd: Option<&str>, cols: u16, rows: u16) -> Result<(Pty, File)> {
        unsafe {
            let (mut in_read, mut in_write) = (HANDLE::default(), HANDLE::default());
            let (mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut in_read, &mut in_write, None, 0)?;
            CreatePipe(&mut out_read, &mut out_write, None, 0)?;

            // dwFlags = 0: the inherit-cursor flag makes conhost wait for a cursor
            // position reply, and other projects saw it deadlock ClosePseudoConsole.
            let hpc = CreatePseudoConsole(size(cols, rows), in_read, out_write, 0)?;
            // conhost duplicated its ends. Ours would keep the pipes open forever.
            CloseHandle(in_read)?;
            CloseHandle(out_write)?;
            let input = File::from_raw_handle(in_write.0);
            let output = File::from_raw_handle(out_read.0);

            let mut attr_size = 0;
            // The first call only reports the required size, so its error is expected.
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attr_size);
            let mut attr_buf = vec![0u8; attr_size];
            let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr().cast());
            InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut attr_size)?;
            let attached = UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(hpc.0 as *const c_void),
                size_of::<HPCON>(),
                None,
                None,
            );

            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            // Without this, a child of a process whose stdio is redirected (a test
            // runner, a parent shell piping us) inherits those handles and writes
            // past the pseudoconsole. Null std handles force it onto the console.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.lpAttributeList = attrs;
            let mut cmd = wide(command_line);
            let cwd = cwd.map(wide);
            let env = environment_block();
            let mut info = PROCESS_INFORMATION::default();
            let spawned = attached.and_then(|()| {
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(cmd.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    Some(env.as_ptr().cast()),
                    cwd.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
                    &startup.StartupInfo,
                    &mut info,
                )
            });
            DeleteProcThreadAttributeList(attrs);
            if let Err(e) = spawned {
                ClosePseudoConsole(hpc);
                return Err(e);
            }
            CloseHandle(info.hThread)?;

            Ok((Pty { hpc, input, process: Some(SendHandle(info.hProcess.0 as usize)) }, output))
        }
    }

    /// Sends bytes to the child as if typed.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        (&self.input).write_all(data)
    }

    /// A second handle to the input pipe, for callbacks that reply to the child.
    pub fn writer(&self) -> io::Result<File> {
        self.input.try_clone()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        unsafe { ResizePseudoConsole(self.hpc, size(cols, rows)) }
    }

    /// Calls `on_exit` from a background thread once the child process ends.
    /// Only the first call registers anything.
    pub fn on_exit(&mut self, on_exit: impl FnOnce() + Send + 'static) {
        let Some(process) = self.process.take() else { return };
        thread::spawn(move || {
            let process = process.handle();
            unsafe {
                WaitForSingleObject(process, INFINITE);
                let _ = CloseHandle(process);
            }
            on_exit();
        });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // ClosePseudoConsole waits for conhost to flush its last output. If the
        // thread draining the output pipe is itself waiting on us, that never
        // happens, so close from a thread of its own.
        let hpc = SendHandle(self.hpc.0 as usize);
        thread::spawn(move || {
            let hpc = hpc;
            unsafe { ClosePseudoConsole(HPCON(hpc.0 as isize)) }
        });
        if let Some(process) = self.process.take() {
            unsafe {
                let _ = CloseHandle(process.handle());
            }
        }
    }
}

/// Raw handles are plain integers; this lets them cross into helper threads.
struct SendHandle(usize);

impl SendHandle {
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
}

fn size(cols: u16, rows: u16) -> COORD {
    COORD { X: cols as i16, Y: rows as i16 }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Our environment plus the variables that tell programs what they're talking to.
fn environment_block() -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            !matches!(k.to_ascii_uppercase().as_str(), "TERM_PROGRAM" | "TERM_PROGRAM_VERSION" | "COLORTERM")
        })
        .collect();
    vars.push(("COLORTERM".into(), "truecolor".into()));
    vars.push(("TERM_PROGRAM".into(), "wisp".into()));
    vars.push(("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()));
    // CreateProcess expects the block sorted, case-insensitively, by name.
    vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Collect everything the child writes until it exits.
    fn run(command_line: &str) -> Vec<u8> {
        let (mut pty, mut output) = Pty::spawn(command_line, None, 80, 24).unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = output.read(&mut buf) {
                all.extend_from_slice(&buf[..n]);
            }
            let _ = tx.send(all);
        });
        let (exit_tx, exit_rx) = mpsc::channel();
        pty.on_exit(move || exit_tx.send(()).unwrap());
        // conhost may ask for device attributes and stall output until answered.
        pty.write(b"\x1b[?62;22c").unwrap();
        exit_rx.recv_timeout(Duration::from_secs(10)).expect("child did not exit");
        drop(pty); // closing the pseudoconsole is what ends the output stream
        rx.recv_timeout(Duration::from_secs(10)).expect("output pipe never closed")
    }

    #[test]
    fn child_output_comes_through() {
        let out = run("cmd.exe /c echo hello-from-conpty");
        let text = String::from_utf8_lossy(&out);
        eprintln!("raw conpty output: {:?}", text);
        assert!(text.contains("hello-from-conpty"), "{text:?}");
    }

    #[test]
    fn environment_reaches_child() {
        let out = run("cmd.exe /c echo %TERM_PROGRAM%-%COLORTERM%");
        assert!(String::from_utf8_lossy(&out).contains("wisp-truecolor"));
    }
}
