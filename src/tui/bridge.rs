use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::{Mutex, OnceLock},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadEvent {
    Start {
        total: u64,
    },
    Init {
        file: String,
    },
    Progress {
        file: String,
        downloaded: u64,
        total: u64,
    },
    Retry {
        file: String,
        resume: bool,
    },
    Completed {
        file: String,
        total: u64,
        failed: bool,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEvent {
    pub package: String,
    pub active: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    #[serde(default)]
    pub download: Option<DownloadEvent>,
    pub text: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub notice: bool,
    #[serde(default)]
    pub build: Option<BuildEvent>,
    pub default: Option<bool>,
    pub plan: Vec<PlanItem>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub name: String,
    pub version: String,
    pub source: String,
}
static PLAN: Mutex<Vec<PlanItem>> = Mutex::new(Vec::new());
pub fn plan(actions: &aur_depends::Actions<'_>) {
    if CHANNEL.get().is_none() {
        return;
    }
    let mut items = Vec::new();
    for p in &actions.install {
        items.push(PlanItem {
            name: p.pkg.name().into(),
            version: p.pkg.version().to_string(),
            source: p.pkg.db().map(|d| d.name()).unwrap_or("repo").into(),
        });
    }
    for base in &actions.build {
        match base {
            aur_depends::Base::Aur(base) => {
                for p in &base.pkgs {
                    items.push(PlanItem {
                        name: p.pkg.name.clone(),
                        version: p.pkg.version.clone(),
                        source: "aur".into(),
                    });
                }
            }
            aur_depends::Base::Pkgbuild(base) => {
                for p in &base.pkgs {
                    items.push(PlanItem {
                        name: p.pkg.pkgname.clone(),
                        version: base.srcinfo.version(),
                        source: base.repo.clone(),
                    });
                }
            }
        }
    }
    *PLAN.lock().unwrap() = items;
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Answer {
    pub value: String,
    pub cancel: bool,
}
#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Cancelled by user")
    }
}
impl std::error::Error for Cancelled {}
static CHANNEL: OnceLock<Mutex<BufReader<UnixStream>>> = OnceLock::new();
pub fn connect(path: &str) -> anyhow::Result<()> {
    CHANNEL
        .set(Mutex::new(BufReader::new(UnixStream::connect(path)?)))
        .map_err(|_| anyhow::anyhow!("Frontend already connected"))?;
    Ok(())
}
fn exchange(channel: &mut BufReader<UnixStream>, question: &Question) -> anyhow::Result<String> {
    serde_json::to_writer(channel.get_mut(), &question)?;
    channel.get_mut().write_all(b"\n")?;
    channel.get_mut().flush()?;
    let mut line = String::new();
    if channel.read_line(&mut line)? == 0 {
        anyhow::bail!("Frontend disconnected");
    }
    let answer: Answer = serde_json::from_str(&line)?;
    if answer.cancel {
        return Err(Cancelled.into());
    }
    Ok(answer.value)
}
fn request(question: Question) -> Option<String> {
    let channel = CHANNEL.get()?;
    let result = (|| -> anyhow::Result<String> {
        let mut channel = channel
            .lock()
            .map_err(|_| anyhow::anyhow!("Frontend lock poisoned"))?;
        exchange(&mut channel, &question)
    })();
    match result {
        Ok(answer) => Some(answer),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
pub fn confirm(text: &str, default: bool) -> Option<bool> {
    request(Question {
        text: text.into(),
        secret: false,
        notice: false,
        build: None,
        download: None,
        default: Some(default),
        plan: PLAN.lock().unwrap().clone(),
    })
    .map(|s| s == "yes")
}
pub fn input(text: &str) -> Option<String> {
    request(Question {
        text: text.into(),
        secret: false,
        notice: false,
        build: None,
        download: None,
        default: None,
        plan: Vec::new(),
    })
}
pub fn choose(text: &str, choices: &[String]) -> Option<usize> {
    CHANNEL.get()?;
    let choices = choices
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}  {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    loop {
        let reply = input(&format!("{text}\n{choices}\n\nEnter a provider number:"))?;
        if let Ok(index) = reply.trim().parse::<usize>() {
            if index > 0 && index <= choices.lines().count() {
                return Some(index - 1);
            }
        }
    }
}

pub fn password(text: &str) -> anyhow::Result<String> {
    request(Question {
        text: text.into(),
        secret: true,
        notice: false,
        build: None,
        download: None,
        default: None,
        plan: Vec::new(),
    })
    .ok_or_else(|| anyhow::anyhow!("No frontend for authentication"))
}

// Fallible interface for libalpm: errors must unwind through trans_release.
pub fn try_request(question: &Question) -> anyhow::Result<String> {
    let mut channel = CHANNEL
        .get()
        .ok_or_else(|| anyhow::anyhow!("Frontend disconnected"))?
        .lock()
        .map_err(|_| anyhow::anyhow!("Frontend lock poisoned"))?;
    exchange(&mut channel, question)
}
pub fn notify(text: String) {
    emit(text, None);
}
pub fn download(event: DownloadEvent) {
    emit(String::new(), Some(event));
}
fn emit(text: String, download: Option<DownloadEvent>) {
    if let Some(channel) = CHANNEL.get() {
        if let Ok(mut channel) = channel.lock() {
            let message = Question {
                text,
                secret: false,
                notice: true,
                build: None,
                download,
                default: None,
                plan: Vec::new(),
            };
            let _ = serde_json::to_writer(channel.get_mut(), &message);
            let _ = channel.get_mut().write_all(b"\n");
        }
    }
}

pub fn connected() -> bool {
    CHANNEL.get().is_some()
}
pub fn build(package: String, active: bool) -> anyhow::Result<()> {
    if connected() {
        try_request(&Question {
            text: String::new(),
            secret: false,
            notice: false,
            download: None,
            build: Some(BuildEvent { package, active }),
            default: None,
            plan: Vec::new(),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_protocol_preserves_plan_and_rejects_cancel_or_disconnect() {
        for response in [
            Some(r#"{"value":"yes","cancel":false}"#),
            Some(r#"{"value":"yes","cancel":true}"#),
            Some("invalid"),
            None,
        ] {
            let (worker, frontend) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut frontend = BufReader::new(frontend);
                let mut line = String::new();
                frontend.read_line(&mut line).unwrap();
                let q: Question = serde_json::from_str(&line).unwrap();
                assert_eq!(q.default, Some(false));
                assert_eq!(q.plan[0].name, "example");
                if let Some(reply) = response {
                    writeln!(frontend.get_mut(), "{reply}").unwrap();
                }
            });
            let result = exchange(
                &mut BufReader::new(worker),
                &Question {
                    text: "Proceed?".into(),
                    secret: false,
                    notice: false,
                    build: None,
                    download: None,
                    default: Some(false),
                    plan: vec![PlanItem {
                        name: "example".into(),
                        version: "2".into(),
                        source: "aur".into(),
                    }],
                },
            );
            if response.is_some_and(|s| s.contains("false")) {
                assert_eq!(result.unwrap(), "yes");
            } else {
                assert!(result.is_err());
            }
            thread.join().unwrap();
        }
    }
}
