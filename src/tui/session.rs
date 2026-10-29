use super::{
    bridge::{Answer, Question},
    pty::Pty,
    settings::Settings,
};
use anyhow::{Context, Result};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::symlink,
        net::{UnixListener, UnixStream},
    },
    sync::mpsc::{self, Receiver, Sender},
};
pub struct Session {
    pub downloads: super::downloads::Downloads,
    pub pty: Pty,
    pub parser: vt100::Parser,
    listener: UnixListener,
    stream: Option<UnixStream>,
    questions: Receiver<(Question, UnixStream)>,
    sender: Sender<(Question, UnixStream)>,
    pending: bool,
    _files: tempfile::TempDir,
    pub code: Option<i32>,
    pub building: bool,
    pub build_name: String,
    pub started: std::time::Instant,
    pub elapsed: Option<std::time::Duration>,
    pub last_error: Option<String>,
    pub finished_at: Option<std::time::Instant>,
}
impl Session {
    pub fn start(args: Vec<String>, settings: &Settings) -> Result<Self> {
        settings.validate()?;
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("frontend.sock");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let policy = dir.path().join("settings.toml");
        std::fs::write(&policy, toml::to_string(settings)?)?;
        let binary = std::env::current_exe()?;
        symlink(&binary, dir.path().join("git"))?;
        let askpass = dir.path().join("paru-tui-askpass");
        symlink(&binary, &askpass)?;
        let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .context("git was not found in PATH")?;
        let mut command = vec!["--worker".into(), socket.to_string_lossy().into_owned()];
        command.extend([
            "--git".into(),
            dir.path().join("git").to_string_lossy().into_owned(),
        ]);
        command.extend(args);
        let envs = vec![
            (
                "PARU_TUI_SOCKET".into(),
                socket.to_string_lossy().into_owned(),
            ),
            (
                "SUDO_ASKPASS".into(),
                askpass.to_string_lossy().into_owned(),
            ),
            ("LC_ALL".into(), "C".into()),
            (
                "PARU_TUI_POLICY".into(),
                policy.to_string_lossy().into_owned(),
            ),
            (
                "PARU_TUI_REAL_GIT".into(),
                real_git.to_string_lossy().into_owned(),
            ),
            (
                "PATH".into(),
                format!(
                    "{}:{}",
                    dir.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
        ];
        let pty = Pty::spawn(
            binary.to_str().context("Executable path is not UTF-8")?,
            &command,
            &envs,
            20,
            100,
        )?;
        let (sender, questions) = mpsc::channel();
        Ok(Self {
            pty,
            parser: vt100::Parser::new(20, 100, 5000),
            listener,
            stream: None,
            questions,
            sender,
            pending: false,
            _files: dir,
            code: None,
            building: false,
            build_name: String::new(),
            downloads: Default::default(),
            started: std::time::Instant::now(),
            elapsed: None,
            last_error: None,
            finished_at: None,
        })
    }
    pub fn tick(&mut self) -> Result<Option<Question>> {
        {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let reader = stream.try_clone()?;
                    let tx = self.sender.clone();
                    std::thread::spawn(move || {
                        for line in BufReader::new(reader).lines() {
                            let Ok(line) = line else {
                                break;
                            };
                            let Ok(q) = serde_json::from_str(&line) else {
                                break;
                            };
                            let Ok(reply) = stream.try_clone() else {
                                break;
                            };
                            if tx.send((q, reply)).is_err() {
                                break;
                            }
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.pty.flush_input()?;
        let output = self.pty.read_available()?;
        if self.building {
            self.parser.process(&output);
        }
        if self.code.is_none()
            && let Some(status) = self.pty.status()?
        {
            use std::os::unix::process::ExitStatusExt;
            self.code = Some(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
            );
            self.elapsed = Some(self.started.elapsed());
            self.finished_at = Some(std::time::Instant::now());
            let output = self.pty.read_available()?;
            if self.building {
                self.parser.process(&output);
            }
        }
        let mut notice = None;
        if !self.pending {
            for _ in 0..128 {
                let Ok((q, stream)) = self.questions.try_recv() else {
                    break;
                };
                if let Some(event) = q.download {
                    self.downloads.update(event);
                    continue;
                }
                self.stream = Some(stream);
                if let Some(build) = q.build {
                    if build.active {
                        self.downloads = Default::default();
                        self.parser = vt100::Parser::new(20, 100, 5000);
                    }
                    self.building = build.active;
                    self.build_name = build.package;
                    self.answer(String::new(), false)?;
                    break;
                }
                if q.notice {
                    let lower = q.text.to_lowercase();
                    if (lower.contains("error") || lower.contains("failed"))
                        && q.text.trim() != "Error:"
                    {
                        self.last_error = Some(q.text.clone());
                    }
                    notice = Some(q);
                    continue;
                }
                self.pending = true;
                return Ok(Some(q));
            }
        }
        if notice.is_some() {
            return Ok(notice);
        }

        Ok(None)
    }
    pub fn answer(&mut self, value: String, cancel: bool) -> Result<()> {
        self.pending = false;
        if cancel {
            self.last_error = Some("Cancelled by user".into());
        }
        let stream = self.stream.as_mut().context("Backend disconnected")?;
        serde_json::to_writer(&mut *stream, &Answer { value, cancel })?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        Ok(())
    }
}
