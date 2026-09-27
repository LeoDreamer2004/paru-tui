//! Native libalpm transactions. The prepared transaction stays locked between
//! presenting the plan and committing exactly that plan. No pacman output parsing.
use super::bridge::{self, PlanItem, Question};
use crate::{
    args::Args,
    config::Config,
    exec::{self, Status},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fmt::{Debug, Display},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub configuration: String,
    pub operation: String,
    pub options: Vec<(String, Option<String>)>,
    pub targets: Vec<String>,
    pub assume_installed: Vec<String>,
}

#[derive(Default)]
struct HookErrors {
    post_transaction: bool,
    current_hook: String,
    messages: Vec<String>,
}

pub fn execute<S: AsRef<str> + Display + Debug>(config: &Config, args: &Args<S>) -> Result<Status> {
    let request = Request {
        configuration: snapshot(&config.pacman)?,
        operation: args.op.as_ref().into(),
        options: args
            .args
            .iter()
            .map(|a| {
                (
                    a.key.as_ref().into(),
                    a.value.as_ref().map(|v| v.as_ref().into()),
                )
            })
            .collect(),
        targets: args.targets.iter().map(|s| s.as_ref().into()).collect(),
        assume_installed: config.assume_installed.clone(),
    };
    validate(&request)?;
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut file, &request)?;
    let binary = std::env::current_exe()?;
    let mut command = if nix::unistd::Uid::effective().is_root() {
        Command::new(&binary)
    } else {
        let mut cmd = Command::new(&config.sudo_bin);
        cmd.args(&config.sudo_flags);
        super::settings::configure_sudo(&mut cmd, &config.sudo_bin);
        cmd.arg(&binary);
        cmd
    };
    command
        .arg("--alpm-worker")
        .arg(std::env::var("PARU_TUI_SOCKET")?)
        .arg(file.path());
    exec::command_status(&mut command)
}

fn snapshot(config: &pacmanconf::Config) -> Result<String> {
    let mut text = String::from("[options]\n");
    let mut add = |key: &str, value: String| -> Result<()> {
        if value.contains(['\n', '\r', '\0']) {
            bail!("Invalid configuration value: {key}");
        }
        if !value.is_empty() {
            text.push_str(&format!("{key} = {value}\n"));
        }
        Ok(())
    };
    for (k, v) in [
        ("RootDir", &config.root_dir),
        ("DBPath", &config.db_path),
        ("LogFile", &config.log_file),
        ("GPGDir", &config.gpg_dir),
    ] {
        add(k, v.clone())?;
    }
    for (k, values) in [
        ("CacheDir", &config.cache_dir),
        ("HookDir", &config.hook_dir),
        ("HoldPkg", &config.hold_pkg),
        ("IgnorePkg", &config.ignore_pkg),
        ("IgnoreGroup", &config.ignore_group),
        ("Architecture", &config.architecture),
        ("NoUpgrade", &config.no_upgrade),
        ("NoExtract", &config.no_extract),
        ("SigLevel", &config.sig_level),
        ("LocalFileSigLevel", &config.local_file_sig_level),
        ("RemoteFileSigLevel", &config.remote_file_sig_level),
    ] {
        for v in values {
            add(k, v.clone())?;
        }
    }
    add("ParallelDownloads", config.parallel_downloads.to_string())?;
    if let Some(user) = &config.download_user {
        add("DownloadUser", user.clone())?;
    }
    if !config.xfer_command.is_empty() {
        bail!("Native transactions do not support XferCommand yet");
    }
    for (key, value) in [
        ("CheckSpace", config.check_space),
        ("UseSyslog", config.use_syslog),
        ("DisableDownloadTimeout", config.disable_download_timeout),
        ("DisableSandbox", config.disable_sandbox),
        (
            "DisableSandboxFilesystem",
            config.disable_sandbox_filesystem,
        ),
        ("DisableSandboxSyscalls", config.disable_sandbox_syscalls),
    ] {
        if value {
            text.push_str(&format!("{key}\n"));
        }
    }
    for repo in &config.repos {
        if repo.name.contains(['\n', '\r', ']', '\0']) {
            bail!("Invalid repository name");
        }
        text.push_str(&format!("\n[{}]\n", repo.name));
        for (k, values) in [
            ("Server", &repo.servers),
            ("SigLevel", &repo.sig_level),
            ("Usage", &repo.usage),
        ] {
            for value in values {
                if value.contains(['\n', '\r', '\0']) {
                    bail!("Invalid repository option");
                }
                text.push_str(&format!("{k} = {value}\n"));
            }
        }
    }
    Ok(text)
}

fn validate(request: &Request) -> Result<()> {
    if !matches!(
        request.operation.as_str(),
        "sync" | "upgrade" | "remove" | "database" | "S" | "U" | "R" | "D"
    ) {
        bail!("Unsupported native operation: {}", request.operation);
    }
    for (key, _) in &request.options {
        if !matches!(
            key.as_str(),
            "y" | "refresh"
                | "u"
                | "sysupgrade"
                | "asdeps"
                | "asdep"
                | "asexplicit"
                | "asexp"
                | "needed"
                | "noconfirm"
                | "confirm"
                | "ask"
                | "color"
                | "noprogressbar"
                | "verbose"
                | "v"
                | "dbonly"
                | "noscriptlet"
                | "d"
                | "nodeps"
                | "w"
                | "downloadonly"
                | "ignore"
                | "ignoregroup"
                | "overwrite"
                | "assume-installed"
                | "n"
                | "nosave"
                | "s"
                | "recursive"
                | "c"
                | "cascade"
                | "unneeded"
                | "config"
                | "root"
                | "r"
                | "dbpath"
                | "b"
                | "cachedir"
                | "arch"
                | "gpgdir"
                | "hookdir"
                | "logfile"
                | "disable-download-timeout"
        ) {
            bail!("Unsupported native transaction option --{key}; no command was executed");
        }
    }
    Ok(())
}

pub fn run_file(path: &str) -> Result<()> {
    let request: Request = serde_json::from_reader(std::fs::File::open(path)?)?;
    let result = run(&request);
    if let Err(error) = &result {
        bridge::notify(format!("Transaction failed: {error:#}"));
    }
    result
}

fn transaction_handle(configuration: &pacmanconf::Config) -> Result<alpm::Alpm> {
    let mut handle = alpm::Alpm::new(
        configuration.root_dir.as_str(),
        configuration.db_path.as_str(),
    )?;
    // configure_alpm replaces hookdirs; pacman.conf lists only the additional
    // directories, not libalpm's built-in (root-relative) system hook directory.
    // Preserve that directory first so custom hooks can still override it.
    let mut configuration = configuration.clone();
    configuration.hook_dir = handle
        .hookdirs()
        .iter()
        .map(str::to_owned)
        .chain(configuration.hook_dir)
        .collect();
    alpm_utils::configure_alpm(&mut handle, &configuration)?;
    Ok(handle)
}

fn run(request: &Request) -> Result<()> {
    validate(request)?;
    let mut configuration: pacmanconf::Config = request.configuration.parse()?;
    for (key, value) in &request.options {
        if let Some(value) = value {
            match key.as_str() {
                "root" | "r" => configuration.root_dir = value.clone(),
                "dbpath" | "b" => configuration.db_path = value.clone(),
                "cachedir" => configuration.cache_dir = vec![value.clone()],
                "arch" => configuration.architecture = vec![value.clone()],
                "gpgdir" => configuration.gpg_dir = value.clone(),
                "hookdir" => configuration.hook_dir.push(value.clone()),
                "logfile" => configuration.log_file = value.clone(),
                _ => {}
            }
        }
        if key == "disable-download-timeout" {
            configuration.disable_download_timeout = true;
        }
    }
    let mut handle = transaction_handle(&configuration)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let registrations: Vec<_> = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .into_iter()
        .map(|sig| signal_hook::flag::register(sig, cancelled.clone()))
        .collect::<std::io::Result<_>>()?;
    handle.set_question_cb(cancelled.clone(), question);
    handle.set_progress_cb(None, |phase, pkg, percent, current, total, last| {
        let key = (format!("{phase:?} {pkg}"), percent / 5);
        if last.as_ref() != Some(&key) {
            bridge::notify(format!(
                "{phase:?} · {pkg} · {percent}% · {current}/{total}"
            ));
            *last = Some(key);
        }
    });
    handle.set_dl_cb(
        std::collections::HashMap::<String, std::time::Instant>::new(),
        |file, event, last| {
            use super::bridge::DownloadEvent as D;
            let file = file.to_owned();
            let message = match event.event() {
                alpm::DownloadEvent::Init(_) => D::Init { file },
                alpm::DownloadEvent::Progress(p) => {
                    let now = std::time::Instant::now();
                    if last
                        .get(&file)
                        .is_some_and(|t| now.duration_since(*t).as_millis() < 100)
                        && p.downloaded < p.total
                    {
                        return;
                    }
                    last.insert(file.clone(), now);
                    D::Progress {
                        file,
                        downloaded: p.downloaded.max(0) as u64,
                        total: p.total.max(0) as u64,
                    }
                }
                alpm::DownloadEvent::Retry(r) => {
                    last.remove(&file);
                    D::Retry {
                        file,
                        resume: r.resume,
                    }
                }
                alpm::DownloadEvent::Completed(c) => {
                    last.remove(&file);
                    D::Completed {
                        file,
                        total: c.total.max(0) as u64,
                        failed: c.result == alpm::DownloadResult::Failed,
                    }
                }
            };
            bridge::download(message);
        },
    );
    // PostTransaction hook failures do not necessarily make trans_commit fail.
    // Keep these errors separate from recoverable download/mirror errors.
    let hook_errors = Arc::new(Mutex::new(HookErrors::default()));
    handle.set_log_cb(hook_errors.clone(), |level, text, state| {
        if level.intersects(alpm::LogLevel::ERROR) && !text.trim().is_empty() {
            let mut state = state.lock().unwrap();
            if state.post_transaction {
                let message = format!("{}: {}", state.current_hook, text.trim());
                state.messages.push(message);
            }
        }
        if level.intersects(alpm::LogLevel::ERROR | alpm::LogLevel::WARNING)
            && !text.trim().is_empty()
        {
            bridge::notify(format!("{level:?}: {}", text.trim()));
        }
    });
    handle.set_event_cb(hook_errors.clone(), |event, state| {
        let event = event.event();
        match &event {
            alpm::Event::HookStart(hook) => {
                let mut state = state.lock().unwrap();
                state.post_transaction = hook.when() == alpm::HookWhen::PostTransaction;
                state.current_hook.clear();
            }
            alpm::Event::HookRunStart(hook) => {
                state.lock().unwrap().current_hook = hook.name().to_owned();
            }
            alpm::Event::HookDone(_) => state.lock().unwrap().post_transaction = false,
            _ => {}
        }
        bridge::notify(format!("{event:?}"))
    });
    for dep in &request.assume_installed {
        handle.add_assume_installed(&alpm::Depend::new(dep.as_str()))?;
    }
    let count = |short: &str, long: &str| {
        request
            .options
            .iter()
            .filter(|(k, _)| k == short || k == long)
            .count()
    };
    let mut flags = alpm::TransFlag::NONE;
    for (k, v) in &request.options {
        match k.as_str() {
            "dbonly" => flags |= alpm::TransFlag::DB_ONLY,
            "noscriptlet" => flags |= alpm::TransFlag::NO_SCRIPTLET,
            "needed" => flags |= alpm::TransFlag::NEEDED,
            "asdeps" | "asdep" => flags |= alpm::TransFlag::ALL_DEPS,
            "asexplicit" | "asexp" => flags |= alpm::TransFlag::ALL_EXPLICIT,
            "w" | "downloadonly" => flags |= alpm::TransFlag::DOWNLOAD_ONLY,
            "n" | "nosave" => flags |= alpm::TransFlag::NO_SAVE,
            "s" | "recursive" => flags |= alpm::TransFlag::RECURSE,
            "c" | "cascade" => flags |= alpm::TransFlag::CASCADE,
            "unneeded" => flags |= alpm::TransFlag::UNNEEDED,
            "ignore" => {
                for name in v.as_deref().unwrap_or_default().split(',') {
                    handle.add_ignorepkg(name)?;
                }
            }
            "ignoregroup" => {
                for name in v.as_deref().unwrap_or_default().split(',') {
                    handle.add_ignoregroup(name)?;
                }
            }
            "assume-installed" => {
                if let Some(dep) = v {
                    handle.add_assume_installed(&alpm::Depend::new(dep.as_str()))?;
                }
            }
            "overwrite" => {
                for pattern in v.as_deref().unwrap_or_default().split(',') {
                    handle.add_overwrite_file(pattern)?;
                }
            }
            _ => {}
        }
    }
    if count("d", "nodeps") > 0 {
        flags |= if count("d", "nodeps") > 1 {
            alpm::TransFlag::NO_DEPS
        } else {
            alpm::TransFlag::NO_DEP_VERSION
        };
    }
    if count("s", "recursive") > 1 {
        flags |= alpm::TransFlag::RECURSE_ALL;
    }
    if count("y", "refresh") > 0 {
        bridge::notify("Refreshing repository databases".into());
        handle.syncdbs_mut().update(count("y", "refresh") > 1)?;
    }
    handle.trans_init(flags)?;
    let result = (|| -> Result<()> {
        if cancelled.load(Ordering::Relaxed) {
            bail!("Transaction cancelled");
        }
        if matches!(request.operation.as_str(), "database" | "D") {
            let reason = if flags.contains(alpm::TransFlag::ALL_DEPS) {
                alpm::PackageReason::Depend
            } else if flags.contains(alpm::TransFlag::ALL_EXPLICIT) {
                alpm::PackageReason::Explicit
            } else {
                bail!("Unsupported database operation");
            };
            for target in &request.targets {
                handle.localdb().pkg(target.as_str())?.set_reason(reason)?;
            }
            return Ok(());
        }
        if matches!(request.operation.as_str(), "sync" | "S") && count("u", "sysupgrade") > 0 {
            handle.sync_sysupgrade(count("u", "sysupgrade") > 1)?;
        }
        for target in &request.targets {
            match request.operation.as_str() {
                "sync" | "S" => {
                    let pkg = if let Some((repo, name)) = target.split_once('/') {
                        handle
                            .syncdbs()
                            .iter()
                            .find(|db| db.name() == repo)
                            .and_then(|db| db.pkg(name).ok())
                    } else {
                        handle.syncdbs().find_satisfier(target.as_str())
                    }
                    .with_context(|| format!("Target not found: {target}"))?;
                    if !handle.trans_add().iter().any(|p| p.name() == pkg.name()) {
                        handle
                            .trans_add_pkg(pkg)
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                    }
                }
                "upgrade" | "U" => {
                    if target.contains("://") {
                        bail!("Native file installation requires a local package archive");
                    }
                    let pkg =
                        handle.pkg_load(target.as_str(), true, handle.local_file_siglevel())?;
                    handle
                        .trans_add_pkg(pkg)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                }
                "remove" | "R" => {
                    handle.trans_remove_pkg(handle.localdb().pkg(target.as_str())?)?;
                }
                _ => unreachable!(),
            }
        }
        handle
            .trans_prepare()
            .map_err(|e| anyhow::anyhow!("Prepare failed: {e:?}"))?;
        if cancelled.load(Ordering::Relaxed) {
            bail!("Transaction cancelled");
        }
        if handle.trans_add().is_empty() && handle.trans_remove().is_empty() {
            bridge::notify("Nothing to do".into());
            return Ok(());
        }
        let mut plan = Vec::new();
        let mut download = 0i64;
        let mut installed = 0i64;
        let mut old = 0i64;
        for p in handle.trans_add() {
            download += p.download_size();
            installed += p.isize();
            old += handle
                .localdb()
                .pkg(p.name())
                .map(|p| p.isize())
                .unwrap_or(0);
            plan.push(PlanItem {
                name: p.name().into(),
                version: p.version().to_string(),
                source: p.db().map(|db| db.name()).unwrap_or("local archive").into(),
            });
        }
        for p in handle.trans_remove() {
            if !handle.trans_add().iter().any(|a| a.name() == p.name()) {
                old += p.isize();
            }
            plan.push(PlanItem {
                name: p.name().into(),
                version: p.version().to_string(),
                source: "REMOVE".into(),
            });
            if configuration.hold_pkg.iter().any(|name| name == p.name()) {
                bail!("Refusing to remove HoldPkg {}", p.name());
            }
        }
        let text = if matches!(request.operation.as_str(), "remove" | "R") {
            format!("Remove the prepared packages?\n\nPackages to remove: {}\nFreed disk space: {:.2} MiB", plan.len(), old as f64 / 1048576.)
        } else {
            format!("Install the prepared transaction?\n\nDownload size: {:.2} MiB\nInstalled size: {:.2} MiB\nNet change: {:.2} MiB",download as f64/1048576.,installed as f64/1048576.,(installed-old) as f64/1048576.)
        };
        let prompt = Question {
            text,
            secret: false,
            notice: false,
            build: None,
            download: None,
            default: Some(true),
            plan,
        };
        if bridge::try_request(&prompt)? != "yes" || cancelled.load(Ordering::Relaxed) {
            bail!("Transaction cancelled");
        }
        if download > 0 {
            bridge::download(bridge::DownloadEvent::Start {
                total: download as u64,
            });
        }
        // The same libalpm transaction and database lock remain live across confirmation.
        handle
            .trans_commit()
            .map_err(|e| anyhow::anyhow!("Commit failed: {e:?}"))?;
        let errors = &hook_errors.lock().unwrap().messages;
        if !errors.is_empty() {
            bail!(
                "Post-transaction hooks failed; packages have already been changed: {}",
                errors.join("; ")
            );
        }
        bridge::notify("Transaction completed".into());
        Ok(())
    })();
    let release = handle.trans_release();
    for id in registrations {
        signal_hook::low_level::unregister(id);
    }
    result?;
    release?;
    Ok(())
}

fn question(mut raw: alpm::AnyQuestion, cancelled: &mut Arc<AtomicBool>) {
    use alpm::Question as Q;
    if cancelled.load(Ordering::Relaxed) {
        raw.set_answer(false);
        return;
    }
    let ask = |text: String| -> bool {
        match bridge::try_request(&Question {
            text,
            secret: false,
            notice: false,
            build: None,
            download: None,
            default: Some(false),
            plan: Vec::new(),
        }) {
            Ok(answer) => answer == "yes",
            Err(_) => {
                cancelled.store(true, Ordering::Relaxed);
                false
            }
        }
    };
    match raw.question() {
        Q::SelectProvider(mut q) => {
            let choices = q
                .providers()
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    format!(
                        "{}  {}/{}",
                        i + 1,
                        p.db().map(|db| db.name()).unwrap_or(""),
                        p.name()
                    )
                })
                .collect::<Vec<_>>();
            loop {
                let prompt = Question {
                    text: format!(
                        "Select provider for {}:\n{}",
                        q.depend(),
                        choices.join("\n")
                    ),
                    secret: false,
                    notice: false,
                    build: None,
                    download: None,
                    default: None,
                    plan: Vec::new(),
                };
                match bridge::try_request(&prompt) {
                    Ok(value) => {
                        if let Ok(i) = value.trim().parse::<usize>() {
                            if i > 0 && i <= choices.len() {
                                q.set_index((i - 1) as i32);
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        cancelled.store(true, Ordering::Relaxed);
                        q.set_index(0);
                        break;
                    }
                }
            }
        }
        Q::InstallIgnorepkg(mut q) => {
            let answer = ask(format!("Install ignored package {}?", q.pkg().name()));
            q.set_install(answer);
        }
        Q::Replace(q) => {
            q.set_replace(ask(format!(
                "Replace {} with {}/{}?",
                q.oldpkg().name(),
                q.newdb().name(),
                q.newpkg().name()
            )));
        }
        Q::Conflict(mut q) => {
            let answer = ask(format!("Remove conflicting package?\n{:?}", q.conflict()));
            q.set_remove(answer);
        }
        Q::Corrupted(mut q) => {
            let answer = ask(format!(
                "Remove corrupted file {}?\n{}",
                q.filepath(),
                q.reason()
            ));
            q.set_remove(answer);
        }
        Q::RemovePkgs(mut q) => {
            let answer = ask(format!(
                "Skip packages with unresolved dependencies?\n{}",
                q.packages()
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
            q.set_skip(answer);
        }
        Q::ImportKey(mut q) => {
            let answer = ask(format!("Import signing key?\n{q:?}"));
            q.set_import(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transaction_preserves_system_hooks_before_custom_overrides() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("db");
        std::fs::create_dir(&db).unwrap();
        let system = root.path().join("usr/share/libalpm/hooks");
        let custom = root.path().join("custom-hooks");
        let cli = root.path().join("cli-hooks");
        let mut config: pacmanconf::Config = format!(
            "[options]\nRootDir = {}\nDBPath = {}\nLogFile = {}/pacman.log\n",
            root.path().display(),
            db.display(),
            root.path().display()
        )
        .parse()
        .unwrap();
        for additional in [vec![], vec![custom, cli]] {
            config.hook_dir = additional.iter().map(|p| p.display().to_string()).collect();
            let restored: pacmanconf::Config = snapshot(&config).unwrap().parse().unwrap();
            let handle = transaction_handle(&restored).unwrap();
            let expected: Vec<_> = std::iter::once(system.as_path())
                .chain(additional.iter().map(|p| p.as_path()))
                .collect();
            let actual: Vec<_> = handle.hookdirs().iter().map(std::path::Path::new).collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn configuration_snapshot_preserves_trust_and_repository_policy() {
        let config: pacmanconf::Config = "[options]\nRootDir = /tmp/root\nDBPath = /tmp/db\nArchitecture = x86_64\nSigLevel = Required DatabaseOptional\nLocalFileSigLevel = Optional\nHoldPkg = pacman\nIgnorePkg = example\nNoExtract = usr/share/doc/*\nCheckSpace\nParallelDownloads = 4\n[core]\nServer = https://example.invalid/core\nSigLevel = Required\nUsage = Sync Search Install Upgrade\n".parse().unwrap();
        let restored: pacmanconf::Config = snapshot(&config).unwrap().parse().unwrap();
        assert_eq!(restored.sig_level, config.sig_level);
        assert_eq!(restored.local_file_sig_level, config.local_file_sig_level);
        assert_eq!(restored.repos, config.repos);
        assert_eq!(restored.hold_pkg, config.hold_pkg);
        assert_eq!(restored.ignore_pkg, config.ignore_pkg);
        assert_eq!(restored.no_extract, config.no_extract);
        assert!(restored.check_space);
        assert_eq!(restored.parallel_downloads, 4);
    }
}
