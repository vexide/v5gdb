use std::{io, path::PathBuf};

use thiserror::Error;
use tokio::process::{Child, Command};
use which::which;

#[derive(Debug, Error)]
#[error("One of the following GDB executables must be installed:\n{}", self.list())]
pub struct FindDebuggerError {
    known_debuggers: Vec<&'static str>,
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

pub struct Debugger {
    path: PathBuf,
}

impl Debugger {
    pub fn find() -> Result<Self, FindDebuggerError> {
        let known_gdb_names = ["arm-none-eabi-gdb", "gdb-multiarch", "gdb"];
        let Some(resolved_gdb) = known_gdb_names.into_iter().find_map(|n| which(n).ok()) else {
            return Err(FindDebuggerError {
                known_debuggers: known_gdb_names.into(),
            });
        };

        Ok(Self { path: resolved_gdb })
    }

    pub async fn start(&self, elf_files: &[PathBuf]) -> io::Result<()> {
        let mut cmd = Command::new(&self.path);

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

        cmd.arg("--eval-command=target remote :35537");

        let mut gdb = cmd.spawn()?;
        wait(&mut gdb).await?;

        Ok(())
    }
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
                    status = gdb.wait() => {
                        status?;
                        break;
                    }
                    _ = ctrl_c.recv() => {}
                    _ = ctrl_break.recv() => {}
                }
            }
        }
        _ => {
            gdb.wait().await?;
        }
    }

    Ok(())
}
