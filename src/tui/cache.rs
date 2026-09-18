//! Run pacman's cache cleaner with its own confirmation prompts in the TUI.
//! Pacman writes prompts without a trailing newline, so line-oriented output
//! readers would wait forever while pacman waits for an answer.
use super::bridge::{self, Question};
use crate::exec::Status;
use anyhow::{Context, Result};
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct CachePrompt {
    text: String,
    default: bool,
}

#[derive(Clone, Copy)]
enum Source {
    Stdout,
    Stderr,
}

enum Output {
    Data(Source, Vec<u8>),
    End,
    Error(std::io::Error),
}

fn pump(
    mut pipe: impl Read + Send + 'static,
    source: Source,
    sender: Sender<Output>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    if sender
                        .send(Output::Data(source, chunk[..size].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Output::Error(error));
                    return;
                }
            }
        }
        let _ = sender.send(Output::End);
    })
}

fn prompt_from_output(bytes: &[u8]) -> Option<CachePrompt> {
    let output = String::from_utf8_lossy(bytes);
    let cleaned = strip_ansi(&output);
    let text = cleaned.trim_end_matches(char::is_whitespace);
    let (body, default) = if let Some(body) = text.strip_suffix("[Y/n]") {
        (body, true)
    } else {
        (text.strip_suffix("[y/N]")?, false)
    };
    let mut lines = body
        .lines()
        .map(|line| line.trim().trim_start_matches("::").trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let question = lines.pop()?;
    let context = lines.into_iter().rev().find(|line| {
        line.starts_with("Cache directory:") || line.starts_with("Database directory:")
    });
    Some(CachePrompt {
        text: context.map_or_else(|| question.to_owned(), |line| format!("{line}\n{question}")),
        default,
    })
}

fn strip_ansi(text: &str) -> String {
    static ANSI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    ANSI.get_or_init(|| regex::Regex::new("\\x1b\\[[0-9;]*[A-Za-z]").unwrap())
        .replace_all(text, "")
        .into_owned()
}

fn cancel(child: &mut std::process::Child) {
    // The command has its own session; signal its entire process group so a
    // pacman process launched through sudo cannot keep cleaning in the back.
    unsafe { nix::libc::kill(-(child.id() as i32), nix::libc::SIGTERM) };
    let _ = child.kill();
    let _ = child.wait();
}

fn execute_with(
    cmd: &mut Command,
    mut ask: impl FnMut(&CachePrompt) -> Result<bool>,
    mut interrupted: impl FnMut() -> bool,
) -> Result<Status> {
    // Pacman prefers /dev/tty for questions. Detach this child so it falls
    // back to stderr, which we can forward to the TUI; stdin remains piped.
    unsafe {
        cmd.pre_exec(|| {
            if nix::libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run: {cmd:?}"))?;
    let (sender, receiver) = mpsc::channel();
    let stdout = pump(child.stdout.take().unwrap(), Source::Stdout, sender.clone());
    let stderr = pump(child.stderr.take().unwrap(), Source::Stderr, sender);
    let mut input = child.stdin.take().unwrap();
    let mut stdout_buffer = Vec::new();
    let mut stderr_buffer = Vec::new();
    let mut stderr_diagnostic = Vec::new();
    let mut cancelled = None;
    let mut open_streams = 2;
    while open_streams > 0 {
        if interrupted() {
            cancel(&mut child);
            anyhow::bail!("Interrupted");
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Output::Data(source, bytes)) => {
                let buffer = match source {
                    Source::Stdout => &mut stdout_buffer,
                    Source::Stderr => {
                        if stderr_diagnostic.len() < 8192 {
                            stderr_diagnostic.extend_from_slice(
                                &bytes[..bytes.len().min(8192 - stderr_diagnostic.len())],
                            );
                        }
                        &mut stderr_buffer
                    }
                };
                buffer.extend_from_slice(&bytes);
                if let Some(prompt) = prompt_from_output(buffer) {
                    let yes = if cancelled.is_some() {
                        false
                    } else {
                        match ask(&prompt) {
                            Ok(yes) => yes,
                            Err(error) => {
                                cancelled = Some(error);
                                false
                            }
                        }
                    };
                    let reply = input
                        .write_all(if yes { b"y\n" } else { b"n\n" })
                        .and_then(|()| input.flush());
                    if let Err(error) = reply {
                        cancel(&mut child);
                        return Err(cancelled.unwrap_or_else(|| error.into()));
                    }
                    buffer.clear();
                } else if buffer.len() > 16384 {
                    buffer.drain(..buffer.len() - 16384);
                }
            }
            Ok(Output::End) => open_streams -= 1,
            Ok(Output::Error(error)) => {
                cancel(&mut child);
                return Err(error.into());
            }
            Err(error) => {
                if matches!(error, mpsc::RecvTimeoutError::Timeout) {
                    continue;
                }
                cancel(&mut child);
                return Err(error.into());
            }
        }
    }
    drop(input);
    stdout.join().expect("stdout reader panicked");
    stderr.join().expect("stderr reader panicked");
    let status = Status(child.wait()?.code().unwrap_or(1));
    if let Some(error) = cancelled {
        return Err(error);
    }
    if status.code() != 0 {
        let diagnostic = String::from_utf8_lossy(&stderr_diagnostic);
        let diagnostic = diagnostic.trim();
        if !diagnostic.is_empty() {
            return Err(anyhow::Error::new(status).context(diagnostic.to_owned()));
        }
    }
    Ok(status)
}

pub(crate) fn execute(cmd: &mut Command, interrupted: impl FnMut() -> bool) -> Result<Status> {
    execute_with(
        cmd,
        |prompt| {
            let answer = bridge::try_request(&Question {
                text: prompt.text.clone(),
                secret: false,
                notice: false,
                build: None,
                download: None,
                default: Some(prompt.default),
                plan: Vec::new(),
            })?;
            Ok(answer == "yes")
        },
        interrupted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_both_pacman_questions_and_preserves_their_defaults() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("printf 'Cache directory: /tmp/cache\\n:: Do you want to remove ALL files from cache? [y/N] '; read first; [ \"$first\" = y ] || exit 71; printf '\\nDatabase directory: /tmp/db\\n:: Do you want to remove unused repositories? [Y/n] ' >&2; read second; [ \"$second\" = n ] || exit 72");
        let mut prompts = Vec::new();
        let status = execute_with(
            &mut command,
            |prompt| {
                prompts.push(prompt.clone());
                Ok(prompts.len() == 1)
            },
            || false,
        )
        .unwrap();
        assert_eq!(status.code(), 0);
        assert_eq!(prompts.len(), 2);
        assert_eq!(
            prompts[0].text,
            "Cache directory: /tmp/cache\nDo you want to remove ALL files from cache?"
        );
        assert!(!prompts[0].default);
        assert_eq!(
            prompts[1].text,
            "Database directory: /tmp/db\nDo you want to remove unused repositories?"
        );
        assert!(prompts[1].default);
    }

    #[test]
    fn cancellation_stops_the_cache_command() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("printf 'Delete files? [y/N] '; read first; [ \"$first\" = n ] || exit 71; printf 'Delete repository data? [Y/n] '; read second; [ \"$second\" = n ] || exit 72");
        let mut shown = 0;
        let error = execute_with(
            &mut command,
            |_| {
                shown += 1;
                Err(bridge::Cancelled.into())
            },
            || false,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Cancelled by user"));
        assert!(error.is::<bridge::Cancelled>());
        assert_eq!(shown, 1);
    }

    #[test]
    fn cache_child_has_no_control_terminal_for_unforwarded_prompts() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("if (: >/dev/tty) 2>/dev/null; then exit 86; fi; printf 'Clear cache? [y/N] ' >&2; read answer; [ \"$answer\" = n ]");
        let status = execute_with(
            &mut command,
            |prompt| {
                assert_eq!(prompt.text, "Clear cache?");
                Ok(false)
            },
            || false,
        )
        .unwrap();
        assert_eq!(status.code(), 0);
    }

    #[test]
    fn interrupt_terminates_a_waiting_cache_command() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 5");
        let mut checks = 0;
        let error = execute_with(
            &mut command,
            |_| panic!("no prompt expected"),
            || {
                checks += 1;
                checks > 1
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Interrupted"));
    }
}
