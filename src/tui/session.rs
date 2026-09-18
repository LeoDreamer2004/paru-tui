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
        if self.code.is_none() {
            if let Some(status) = self.pty.status()? {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_only_collects_acknowledged_build_output() {
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("test.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (sender, questions) = mpsc::channel();
        let pty = Pty::spawn("/bin/sh", &["-c".into(), "printf 'outside\\n'; read start; printf 'building\\n'; read end; printf 'after\\n'".into()], &[], 20, 100).unwrap();
        let mut session = Session {
            pty,
            parser: vt100::Parser::new(20, 100, 5000),
            listener,
            stream: None,
            questions,
            sender,
            pending: false,
            _files: files,
            code: None,
            building: false,
            build_name: String::new(),
            downloads: Default::default(),
            started: std::time::Instant::now(),
            elapsed: None,
            last_error: None,
            finished_at: None,
        };
        let mut client = UnixStream::connect(path).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let poll = |session: &mut Session, predicate: fn(&Session) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                session.tick().unwrap();
                if predicate(session) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "build handshake timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        // Drain initial output before the build handshake.
        std::thread::sleep(std::time::Duration::from_millis(50));
        session.tick().unwrap();
        assert!(session.parser.screen().contents().is_empty());
        let message = |active| Question {
            text: String::new(),
            secret: false,
            notice: false,
            download: None,
            build: Some(super::super::bridge::BuildEvent {
                package: "example".into(),
                active,
            }),
            default: None,
            plan: vec![],
        };
        let mut progress = message(false);
        progress.build = None;
        progress.notice = true;
        progress.download = Some(super::super::bridge::DownloadEvent::Progress {
            file: "example.pkg.tar.zst".into(),
            downloaded: 50,
            total: 100,
        });
        serde_json::to_writer(&mut client, &progress).unwrap();
        client.write_all(b"\n").unwrap();
        poll(&mut session, |s| s.downloads.visible());
        assert!(!session.pending);
        assert!(!session.building);
        serde_json::to_writer(&mut client, &message(true)).unwrap();
        client.write_all(b"\n").unwrap();
        poll(&mut session, |s| s.building);
        assert!(!session.downloads.visible());
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        session.pty.send(b"go\n").unwrap();
        poll(&mut session, |s| {
            s.parser.screen().contents().contains("building")
        });
        serde_json::to_writer(&mut client, &message(false)).unwrap();
        client.write_all(b"\n").unwrap();
        poll(&mut session, |s| !s.building);
        reply.clear();
        reader.read_line(&mut reply).unwrap();
        session.pty.send(b"finish\n").unwrap();
        poll(&mut session, |s| s.code.is_some());
        let output = session.parser.screen().contents();
        assert!(output.contains("building"));
        assert!(!output.contains("outside"));
        assert!(!output.contains("after"));
    }
}
