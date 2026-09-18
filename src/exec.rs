#![allow(clippy::disallowed_methods)]

use crate::args::Args;
use crate::config::Config;

use std::ffi::OsStr;
use std::fmt::{Debug, Display, Formatter};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use log::debug;
use signal_hook::consts::signal::*;
use signal_hook::flag as signal_flag;
use std::sync::LazyLock;
use tr::tr;

pub static DEFAULT_SIGNALS: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| {
    let arc = Arc::new(AtomicBool::new(true));
    signal_flag::register_conditional_default(SIGTERM, Arc::clone(&arc)).unwrap();
    signal_flag::register_conditional_default(SIGINT, Arc::clone(&arc)).unwrap();
    signal_flag::register_conditional_default(SIGQUIT, Arc::clone(&arc)).unwrap();
    arc
});

static CAUGHT_SIGNAL: LazyLock<Arc<AtomicUsize>> = LazyLock::new(|| {
    let arc = Arc::new(AtomicUsize::new(0));
    signal_flag::register_usize(SIGTERM, Arc::clone(&arc), SIGTERM as usize).unwrap();
    signal_flag::register_usize(SIGINT, Arc::clone(&arc), SIGINT as usize).unwrap();
    signal_flag::register_usize(SIGQUIT, Arc::clone(&arc), SIGQUIT as usize).unwrap();
    arc
});

pub static RAISE_SIGPIPE: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| {
    let arc = Arc::new(AtomicBool::new(true));
    signal_flag::register_conditional_default(SIGPIPE, Arc::clone(&arc)).unwrap();
    arc
});

#[derive(Debug, Clone, Copy)]
pub struct Status(pub i32);

impl Display for Status {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

impl std::error::Error for Status {}

impl Status {
    pub fn code(self) -> i32 {
        self.0
    }

    pub fn success(self) -> Result<i32, Status> {
        if self.0 == 0 {
            Ok(0)
        } else {
            Err(self)
        }
    }
}

fn command_err(cmd: &Command) -> String {
    format!(
        "{} {} {}",
        tr!("failed to run:"),
        cmd.get_program().to_string_lossy(),
        cmd.get_args()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

pub fn command_status(cmd: &mut Command) -> Result<Status> {
    debug!("running command: {:?}", cmd);
    let term = &*CAUGHT_SIGNAL;

    DEFAULT_SIGNALS.store(false, Ordering::Relaxed);

    #[cfg(feature = "tui")]
    let capture = crate::tui::bridge::connected()
        && Path::new(cmd.get_program())
            .file_name()
            .is_some_and(|name| name == "sudo");
    #[cfg(feature = "tui")]
    let ret = if capture {
        sudo_status(cmd)
    } else {
        cmd.status()
            .map(|s| Status(s.code().unwrap_or(1)))
            .with_context(|| command_err(cmd))
    };
    #[cfg(not(feature = "tui"))]
    let ret = cmd
        .status()
        .map(|s| Status(s.code().unwrap_or(1)))
        .with_context(|| command_err(cmd));

    DEFAULT_SIGNALS.store(true, Ordering::Relaxed);

    match term.swap(0, Ordering::Relaxed) {
        0 => ret,
        n => std::process::exit(128 + n as i32),
    }
}

#[cfg(feature = "tui")]
fn sudo_status(cmd: &mut Command) -> Result<Status> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = tempfile::tempfile()?;
    cmd.stderr(Stdio::from(file.try_clone()?));
    let status = cmd
        .status()
        .map(|s| Status(s.code().unwrap_or(1)))
        .with_context(|| command_err(cmd))?;
    if status.0 != 0 {
        let mut message = String::new();
        file.seek(SeekFrom::Start(0))?;
        file.take(8192).read_to_string(&mut message)?;
        if !message.trim().is_empty() {
            return Err(anyhow::Error::new(status).context(message.trim().to_owned()));
        }
    }
    Ok(status)
}
#[cfg(all(test, feature = "tui"))]
#[test]
fn sudo_failure_retains_diagnostics_and_exit_code() {
    let error = sudo_status(Command::new("/bin/sh").args([
        "-c",
        "printf 'sudo: 3 incorrect password attempts\n' >&2; exit 1",
    ]))
    .unwrap_err();
    assert!(format!("{error:#}").contains("3 incorrect password attempts"));
    assert_eq!(error.downcast_ref::<Status>().unwrap().code(), 1);
}

pub fn command(cmd: &mut Command) -> Result<()> {
    command_status(cmd)?
        .success()
        .with_context(|| command_err(cmd))?;
    Ok(())
}

pub fn command_output(cmd: &mut Command) -> Result<Output> {
    debug!("running command: {:?}", cmd);
    let term = &*CAUGHT_SIGNAL;

    DEFAULT_SIGNALS.store(false, Ordering::Relaxed);

    let ret = cmd.output().with_context(|| command_err(cmd));

    DEFAULT_SIGNALS.store(true, Ordering::Relaxed);
    let ret = match term.swap(0, Ordering::Relaxed) {
        0 => ret?,
        n => std::process::exit(128 + n as i32),
    };

    if !ret.status.success() {
        bail!(
            "{}: {}",
            command_err(cmd),
            String::from_utf8_lossy(&ret.stderr).trim()
        );
    }

    Ok(ret)
}

pub fn spawn(cmd: &mut Command) -> Result<Child> {
    debug!("running command: {:?}", cmd);
    cmd.spawn().with_context(|| command_err(cmd))
}

pub fn wait(cmd: &Command, child: &mut Child) -> Result<Status> {
    let status = child
        .wait()
        .map(|s| Status(s.code().unwrap_or(1)))
        .with_context(|| command_err(cmd))?;
    Ok(status)
}

pub fn spawn_sudo(sudo: String, flags: Vec<String>) -> Result<()> {
    update_sudo(&sudo, &flags)?;
    thread::spawn(move || sudo_loop(&sudo, &flags));
    Ok(())
}

fn sudo_loop<S: AsRef<OsStr>>(sudo: &str, flags: &[S]) -> Result<()> {
    loop {
        thread::sleep(Duration::from_secs(250));
        update_sudo(sudo, flags)?;
    }
}

fn update_sudo<S: AsRef<OsStr>>(sudo: &str, flags: &[S]) -> Result<()> {
    let mut cmd = Command::new(sudo);
    cmd.args(flags);
    #[cfg(feature = "tui")]
    crate::tui::settings::configure_sudo(&mut cmd, sudo);
    let status = command_status(&mut cmd)?;
    status.success()?;
    Ok(())
}

fn wait_for_lock(config: &Config) {
    let path = Path::new(config.alpm.dbpath()).join("db.lck");
    let c = config.color;
    if path.exists() {
        println!(
            "{} {}",
            c.error.paint("::"),
            c.bold
                .paint(tr!("Pacman is currently in use, please wait..."))
        );

        while path.exists() {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

fn new_pacman<S: AsRef<str> + Display + Debug>(config: &Config, args: &Args<S>) -> Command {
    let mut cmd = if config.need_root {
        wait_for_lock(config);
        let mut cmd = Command::new(&config.sudo_bin);
        cmd.args(&config.sudo_flags);
        #[cfg(feature = "tui")]
        crate::tui::settings::configure_sudo(&mut cmd, &config.sudo_bin);
        cmd.arg(args.bin.as_ref());
        cmd
    } else {
        Command::new(args.bin.as_ref())
    };

    if let Some(config) = &config.pacman_conf {
        cmd.args(["--config", config]);
    }
    cmd.args(args.args());
    cmd
}

pub fn pacman<S: AsRef<str> + Display + Debug>(config: &Config, args: &Args<S>) -> Result<Status> {
    #[cfg(feature = "tui")]
    if std::env::var_os("PARU_TUI_SOCKET").is_some() {
        if config.clean >= 2 {
            let mut cmd = new_pacman(config, args);
            let term = &*CAUGHT_SIGNAL;
            DEFAULT_SIGNALS.store(false, Ordering::Relaxed);
            let result = crate::tui::cache::execute(&mut cmd, || term.load(Ordering::Relaxed) != 0);
            DEFAULT_SIGNALS.store(true, Ordering::Relaxed);
            return match term.swap(0, Ordering::Relaxed) {
                0 => result,
                n => std::process::exit(128 + n as i32),
            };
        }
        if config.need_root {
            return crate::tui::transaction::execute(config, args);
        }
    }
    let mut cmd = new_pacman(config, args);
    command_status(&mut cmd)
}

pub fn pacman_output<S: AsRef<str> + Display + std::fmt::Debug>(
    config: &Config,
    args: &Args<S>,
) -> Result<Output> {
    #[cfg(feature = "tui")]
    if std::env::var_os("PARU_TUI_SOCKET").is_some() && config.need_root {
        use std::os::unix::process::ExitStatusExt;
        let status = crate::tui::transaction::execute(config, args)?;
        return Ok(Output {
            status: std::process::ExitStatus::from_raw(status.0 << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    let mut cmd = new_pacman(config, args);
    cmd.stdin(Stdio::inherit());
    command_output(&mut cmd)
}

fn new_makepkg<S: AsRef<OsStr>>(
    config: &Config,
    dir: &Path,
    args: &[S],
    pkgdest: Option<&str>,
) -> Command {
    let mut cmd = Command::new(&config.makepkg_bin);
    #[cfg(feature = "tui")]
    crate::tui::settings::configure_build(&mut cmd, dir);
    if let Some(mconf) = &config.makepkg_conf {
        cmd.arg("--config").arg(mconf);
    }
    if let Some(dest) = pkgdest {
        cmd.env("PKGDEST", dest);
    }
    cmd.args(&config.mflags).args(args).current_dir(dir);
    cmd
}

pub fn makepkg_dest<S: AsRef<OsStr>>(
    config: &Config,
    dir: &Path,
    args: &[S],
    pkgdest: Option<&str>,
) -> Result<Status> {
    let mut cmd = new_makepkg(config, dir, args, pkgdest);
    #[cfg(feature = "tui")]
    crate::tui::bridge::build(
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        true,
    )?;
    let result = command_status(&mut cmd);
    #[cfg(feature = "tui")]
    crate::tui::bridge::build(
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        false,
    )?;
    result
}

pub fn makepkg<S: AsRef<OsStr>>(config: &Config, dir: &Path, args: &[S]) -> Result<Status> {
    makepkg_dest(config, dir, args, None)
}

pub fn makepkg_output_dest<S: AsRef<OsStr>>(
    config: &Config,
    dir: &Path,
    args: &[S],
    pkgdest: Option<&str>,
) -> Result<Output> {
    let mut cmd = new_makepkg(config, dir, args, pkgdest);
    command_output(&mut cmd)
}

pub fn makepkg_output<S: AsRef<OsStr>>(config: &Config, dir: &Path, args: &[S]) -> Result<Output> {
    makepkg_output_dest(config, dir, args, None)
}

pub fn has_command(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}
