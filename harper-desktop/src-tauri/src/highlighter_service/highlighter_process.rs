use crate::communication::Server;
use crate::config::Config;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// The size at which the log starts again, so leaving it on costs a bounded amount of disk.
const MAX_LOG_BYTES: u64 = 1_000_000;

/// Where the highlighter's own output is kept.
pub fn log_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| path.join("harper-desktop").join("highlighter.log"))
}

/// Represents a highlighter process.
pub struct HighlighterProcess {
    child: Child,
}

impl HighlighterProcess {
    /// Must be called from within a Tokio runtime
    pub fn spawn() -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);

        // The highlighter is a background process that only talks over its pipes. Without this it
        // is handed a console window of its own, which flashes up or sits in the taskbar every
        // time the service starts.
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command
            .arg("highlighter")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The highlighter runs with no console and no window, so anything it says about why it
            // is about to die goes nowhere. It keeps dying, so keep what it says: a service that
            // turns itself off a few minutes in leaves nothing else to go on.
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(keep_output(stderr));
        }

        Ok(Self { child })
    }

    /// Builds the Tauri-side protocol server from the highlighter process stdio handles.
    ///
    /// This consumes the child stdio handles and can only succeed once per process.
    pub fn create_server(
        &mut self,
        config: Arc<Mutex<Config>>,
    ) -> io::Result<Server<ChildStdout, ChildStdin>> {
        let stdout = self.child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "highlighter stdout is unavailable",
            )
        })?;
        let stdin = self.child.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "highlighter stdin is unavailable",
            )
        })?;

        Ok(Server::new(stdout, stdin, config))
    }

    pub async fn terminate(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Appends everything the highlighter writes to its log, until the process ends.
async fn keep_output(stderr: tokio::process::ChildStderr) {
    let Some(path) = log_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        let _ = std::fs::File::create(&path);
    }

    let mut lines = BufReader::new(stderr).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        use std::io::Write as _;

        // Opened per line rather than held: the interesting lines are the last ones before a
        // crash, and a handle buffered in a process that is going down loses exactly those.
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        else {
            return;
        };

        let _ = writeln!(file, "{line}");
    }
}

impl Drop for HighlighterProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
