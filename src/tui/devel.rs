use super::{
    catalog::{Catalog, Event},
    settings::{Settings, configure_git},
};
use crate::{
    config::Config,
    devel::{RepoInfo, read_devel_info, remote_commit},
};
use anyhow::{Context, Result};
use futures::{StreamExt, stream};
use std::{collections::HashSet, sync::mpsc::Sender, time::Duration};

async fn changed(
    config: &Config,
    settings: &Settings,
    base: &str,
    repo: &RepoInfo,
) -> Result<bool> {
    let mut command = tokio::process::Command::new(&config.git_bin);
    command.args(&config.git_flags);
    configure_git(command.as_std_mut(), settings, Some(base), Some(&repo.url))?;
    let commit = tokio::time::timeout(
        Duration::from_secs(15),
        remote_commit(command, &repo.url, repo.branch.as_deref()),
    )
    .await
    .context("Git update check timed out")??;
    Ok(commit != repo.commit)
}

/// Check paru's recorded source commits without advancing the installed baseline.
/// No package-name suffix heuristic: split packages use their recorded pkgbase.
pub(super) async fn scan(
    config: &Config,
    settings: &Settings,
    catalog: &mut Catalog,
    sender: &Sender<Event>,
) -> Result<()> {
    settings.validate()?;
    let Some(info) = read_devel_info(config)? else {
        return Ok(());
    };
    let bases: HashSet<_> = catalog
        .installed
        .iter()
        .filter(|p| p.is_aur() && !p.ignored && !config.ignore_devel.is_match(&p.name))
        .map(|p| p.base.clone())
        .collect();
    let requests = info
        .info
        .iter()
        .filter(|(base, _)| bases.contains(*base))
        .flat_map(|(base, info)| info.repos.iter().map(move |repo| (base, repo)))
        .filter(|(_, repo)| !config.ignore_devel_source.contains(&repo.url));
    let mut checks = stream::iter(requests)
        .map(|(base, repo)| async move {
            (base, &repo.url, changed(config, settings, base, repo).await)
        })
        .buffer_unordered(8);
    let mut updated = HashSet::new();
    let mut failures = vec![];
    while let Some((base, url, result)) = checks.next().await {
        match result {
            Ok(true) => {
                updated.insert(base.as_str());
            }
            Ok(false) => {}
            Err(e) => failures.push(format!("{base} ({url}): {e:#}")),
        }
    }
    for package in &mut catalog.installed {
        if package.is_aur()
            && !package.ignored
            && !config.ignore_devel.is_match(&package.name)
            && updated.contains(package.base.as_str())
        {
            package.devel = true;
            package.next = Some("latest-commit".into());
        }
    }
    if !failures.is_empty() {
        let _ = sender.send(Event::Warning(format!(
            "Git update check failed:\n{}",
            failures.join("\n")
        )));
    }
    Ok(())
}
