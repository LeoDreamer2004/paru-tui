//! Linux PTY ownership. Child output never reaches the outer terminal directly.
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, ExitStatus, Stdio},
};

pub struct Pty {
    master: File,
    child: Child,
    pending: Vec<u8>,
    pub eof: bool,
}

impl Pty {
    pub fn spawn(
        program: &str,
        args: &[String],
        envs: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> io::Result<Self> {
        let mut master = -1;
        let mut slave = -1;
        let size = size(rows, cols);
        // openpty returns two owned file descriptors on success only.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(envs.iter().cloned())
            .env("TERM", "xterm-256color")
            .env_remove("LINES")
            .env_remove("COLUMNS")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                // Only async-signal-safe operations between fork and exec.
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                for sig in [
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGPIPE,
                    libc::SIGHUP,
                    libc::SIGTERM,
                ] {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = libc::SIG_DFL;
                    libc::sigemptyset(&mut action.sa_mask);
                    if libc::sigaction(sig, &action, std::ptr::null_mut()) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Self {
            master,
            child,
            pending: Vec::new(),
            eof: false,
        })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size(rows, cols)) }
            == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn read_available(&mut self) -> io::Result<Vec<u8>> {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        // Bound work per frame so noisy builds cannot starve input/redrawing.
        while !self.eof && output.len() < 256 * 1024 {
            match self.master.read(&mut buffer) {
                Ok(0) => self.eof = true,
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => self.eof = true,
                Err(e) => return Err(e),
            }
        }
        Ok(output)
    }

    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        self.flush_input()
    }

    pub fn flush_input(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() && !self.eof {
            match self.master.write(&self.pending) {
                Ok(0) => break,
                Ok(count) => {
                    self.pending.drain(..count);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    self.pending.clear();
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // On UI errors, hang up the owned terminal rather than leaving an
        // invisible updater waiting for input. Closing master also sends HUP.
        if matches!(self.child.try_wait(), Ok(None)) {
            unsafe {
                let foreground = libc::tcgetpgrp(self.master.as_raw_fd());
                if foreground > 0 {
                    libc::kill(-foreground, libc::SIGHUP);
                }
                libc::kill(-(self.child.id() as i32), libc::SIGHUP);
            }
        }
    }
}

fn size(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}
