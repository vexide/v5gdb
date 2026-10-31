use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};

use thiserror::Error;
use tokio::process::{Child, Command};
use tracing::debug;
use which::which;

#[derive(Debug, Error)]
#[error("v5gdb couldn't find any debuggers installed on your system.\n\
Please install one of the following:\n{}", self.list())]
pub struct FindDebuggerError {
    known_debuggers: Vec<String>,
}

impl FindDebuggerError {
    fn list(&self) -> String {
        self.known_debuggers
            .iter()
            .map(|exe| format!("  - {exe}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Commands which are fed to GDB before connecting to the V5 Brain to improve performance and
/// compatibility.
const GDB_STARTUP_COMMANDS: [&str; 3] = [
    // Ask GDB to cache RAM data instead of asking the target to re-send it every single time.
    // GDB clears the cache whenever the program resumes. This overwrites the memory map that the
    // v5gdb library sends, so it should be kept in sync with:
    // `firmware/src/gdb_target/arch/memory_map.xml`
    "mem 0x100000 0x2c00000 ro cache",
    "mem 0x2c00000 0x8000000 rw cache",
    // Ask GDB to send larger 1024-byte-sized chunks of memory when it needs to read from the
    // target. This should help reduce the number of packet round trips. See:
    // <https://sourceware.org/gdb/current/onlinedocs/gdb.html/Caching-Target-Data.html>
    "set dcache line-size 1024",
];

#[derive(Debug, Error)]
#[error("couldn't find the specified debugger '{}'", path.display())]
pub struct UserDebuggerError {
    path: PathBuf,
    source: which::Error,
}

#[derive(Debug)]
pub struct Debugger {
    path: PathBuf,
}

impl Debugger {
    pub fn find() -> Result<Self, FindDebuggerError> {
        const ARM_GDB: &str = "arm-none-eabi-gdb";
        const GENERIC_GDB_NAMES: [&str; 2] = ["gdb-multiarch", "gdb"];

        // Prefer an ARM GDB, including one from a PROS toolchain, over a generic GDB that might not
        // have been built with ARM support.
        let resolved_gdb = which(ARM_GDB)
            .ok()
            .or_else(find_pros_toolchain_gdb)
            .or_else(|| GENERIC_GDB_NAMES.into_iter().find_map(|n| which(n).ok()));

        let Some(resolved_gdb) = resolved_gdb else {
            let mut known_debuggers = vec![
                ARM_GDB.to_string(),
                format!(
                    r#"PROS Toolchain via {} ("OneClick")"#,
                    vscode_link("PROS VS Code Extension", "extension/sigbots.pros")
                ),
                "PROS Toolchain via $PROS_TOOLCHAIN env variable".to_string(),
            ];
            known_debuggers.extend(GENERIC_GDB_NAMES.map(String::from));
            return Err(FindDebuggerError { known_debuggers });
        };

        Ok(Self { path: resolved_gdb })
    }

    /// Uses a debugger that the user chose.
    ///
    /// `path` can be a path or the name of an executable on PATH.
    pub fn from_user(path: &Path) -> Result<Self, UserDebuggerError> {
        let resolved = which(path).map_err(|source| UserDebuggerError {
            path: path.into(),
            source,
        })?;
        Ok(Self { path: resolved })
    }

    pub async fn start(&self, elf_files: &[PathBuf], extra_args: &[OsString]) -> io::Result<()> {
        let mut cmd = Command::new(&self.path);
        cmd.args(extra_args);

        let mut elves = elf_files.iter();
        if let Some(main_elf_file) = elves.next() {
            cmd.arg(format!("--eval-command=file {}", main_elf_file.display()));
        }
        for elf_file in elves {
            // `with confirm off` skips the "add symbol table from file" prompt.
            cmd.arg(format!(
                "--eval-command=with confirm off -- add-symbol-file {}",
                elf_file.display()
            ));
        }

        for command in GDB_STARTUP_COMMANDS {
            cmd.arg(format!("--eval-command={command}"));
        }
        cmd.arg("--eval-command=target remote :35537");

        let mut gdb = cmd.spawn()?;
        wait(&mut gdb).await?;

        Ok(())
    }
}

/// Looks for `arm-none-eabi-gdb` in the toolchain named by `$PROS_TOOLCHAIN`, then in the
/// toolchain installed by the PROS VS Code extension's "One-Click" installer.
///
/// The One-Click install layout is based on:
/// <https://github.com/purduesigbots/pros-vsc/blob/develop/src/one-click/path.ts>
fn find_pros_toolchain_gdb() -> Option<PathBuf> {
    let gdb_name = format!("arm-none-eabi-gdb{}", env::consts::EXE_SUFFIX);

    let mut toolchain_dirs = vec![];
    if let Some(dir) = env::var_os("PROS_TOOLCHAIN") {
        toolchain_dirs.push(PathBuf::from(dir));
    }
    toolchain_dirs.extend(one_click_toolchain_dirs());

    toolchain_dirs.into_iter().find_map(|dir| {
        let gdb = dir.join("bin").join(&gdb_name);
        debug!(path = ?gdb, "Checking for PROS Toolchain GDB");
        gdb.is_file().then_some(gdb)
    })
}

/// Returns the paths where the PROS VS Code extension might have installed its toolchain.
fn one_click_toolchain_dirs() -> Vec<PathBuf> {
    let Some(config_dir) = editor_config_dir() else {
        return vec![];
    };
    // Checking all apps lets us support nonstandard editors like VS Code forks or VS Code Insiders.
    let Ok(apps) = fs::read_dir(&config_dir) else {
        return vec![];
    };

    let toolchain_subdir: &Path = if cfg!(windows) {
        Path::new("pros-toolchain-windows/usr")
    } else if cfg!(target_os = "macos") {
        Path::new("pros-toolchain-macos")
    } else {
        Path::new("pros-toolchain-linux")
    };

    apps.filter_map(Result::ok)
        .map(|editor| {
            editor
                .path()
                .join("User/globalStorage/sigbots.pros/install")
                .join(toolchain_subdir)
        })
        .filter(|dir| dir.is_dir())
        .collect()
}

/// Returns the directory where VS Code stores its user data folder on this platform.
fn editor_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env::home_dir().map(|home| home.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::home_dir().map(|home| home.join(".config")))
    }
}

/// Returns `text`, made into a link that opens the given `vscode:{path}` link if v5gdb is running
/// in a VS Code terminal.
fn vscode_link(text: &str, path: &str) -> String {
    let in_vscode = env::var_os("TERM_PROGRAM").is_some_and(|p| p == "vscode");
    if !in_vscode || !io::stderr().is_terminal() {
        return text.to_string();
    }

    // Insiders builds register a different URL scheme than stable VS Code.
    let is_insiders = env::var("TERM_PROGRAM_VERSION").is_ok_and(|v| v.ends_with("-insider"));
    let scheme = if is_insiders {
        "vscode-insiders"
    } else {
        "vscode"
    };

    // OSC 8 terminal hyperlink:
    // <https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda>
    format!("\x1b]8;;{scheme}:{path}\x1b\\{text}\x1b]8;;\x1b\\")
}

async fn wait(process: &mut Child) -> io::Result<()> {
    cfg_select! {
        // On Unix, ignore SIGINT (Ctrl-C) since it's delivered to the whole foreground process
        // group including GDB, which has its own logic to handle signals.
        unix => {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigint = signal(SignalKind::interrupt())?;
            let mut sigterm = signal(SignalKind::terminate())?;

            loop {
                tokio::select! {
                    status = process.wait() => {
                        status?;
                        break;
                    }
                    _ = sigint.recv() => {}
                    _ = sigterm.recv() => {}
                }
            }
        }
        // It's similar on Windows but we receive a Ctrl-C event / a Ctrl-Break event instead.
        windows => {
            use tokio::signal::windows::{ctrl_break, ctrl_c};

            let mut ctrl_c = ctrl_c()?;
            let mut ctrl_break = ctrl_break()?;

            loop {
                tokio::select! {
                    status = process.wait() => {
                        status?;
                        break;
                    }
                    _ = ctrl_c.recv() => {}
                    _ = ctrl_break.recv() => {}
                }
            }
        }
        _ => {
            process.wait().await?;
        }
    }

    Ok(())
}
