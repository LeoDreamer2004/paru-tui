use super::{
    catalog::{Catalog, Event},
    settings::{configure_git, Settings},
};
use crate::{
    config::Config,
    devel::{read_devel_info, remote_commit, RepoInfo},
};
use anyhow::{Context, Result};
use futures::{stream, StreamExt};
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn scan_compares_split_base_commits_and_routes_each_git_request_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("local")).unwrap();
        std::fs::write(dir.path().join("local/ALPM_DB_VERSION"), "9\n").unwrap();
        for (name, base) in [
            ("numpy-latest", "numpy-base"),
            ("numpy-tools", "numpy-base"),
            ("stable", "stable"),
            ("failed", "failed"),
        ] {
            let path = dir.path().join(format!("local/{name}-1-1"));
            fs::create_dir(&path).unwrap();
            fs::write(
                path.join("desc"),
                format!("%NAME%\n{name}\n\n%VERSION%\n1-1\n\n%BASE%\n{base}\n\n"),
            )
            .unwrap();
        }
        let git = dir.path().join("git");
        // Assertions live in the child too: a wrong proxy yields a scan failure.
        fs::write(
            &git,
            r#"#!/bin/sh
case "$*" in
  *'/changed '*|*'/same '*) test "$https_proxy" = 'http://127.0.0.1:7890' || exit 2 ;;
  *) test -z "$https_proxy" || exit 3 ;;
esac
case "$*" in
  *'/failed '*) echo 'fixture unreachable' >&2; exit 4 ;;
  *'/changed '*) printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tHEAD\n' ;;
  *) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tHEAD\n' ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = Config::default();
        config.pacman.root_dir = "/".into();
        config.pacman.db_path = dir.path().to_str().unwrap().into();
        config.pacman.log_file = dir.path().join("alpm.log").to_str().unwrap().into();
        config.init_alpm().unwrap();
        config.git_bin = git.to_str().unwrap().into();
        config.devel_path = dir.path().join("devel.toml");
        let baseline = [
            ("numpy-base", "changed"),
            ("numpy-base", "same"),
            ("stable", "stable"),
            ("failed", "failed"),
        ]
        .iter()
        .map(|(base, url)| {
            format!(
                "[[{base}]]\nurl = 'https://example.org/{url}'\ncommit = '{}'\n",
                "a".repeat(40)
            )
        })
        .collect::<String>();
        fs::write(&config.devel_path, &baseline).unwrap();
        let settings = Settings {
            proxy_url: "http://127.0.0.1:7890".into(),
            proxy_bases: ["numpy-base".into()].into(),
            ..Default::default()
        };
        let mut catalog = Catalog::default();
        catalog.installed = config
            .alpm
            .localdb()
            .pkgs()
            .iter()
            .map(|p| super::super::catalog::package(p, "aur", Some(p.version().to_string())))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(scan(&config, &settings, &mut catalog, &tx))
            .unwrap();
        let updates: HashSet<_> = catalog
            .installed
            .iter()
            .filter(|p| p.devel)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(updates, ["numpy-latest", "numpy-tools"].into());
        assert!(catalog
            .installed
            .iter()
            .filter(|p| p.devel)
            .all(|p| p.next.as_deref() == Some("latest-commit")));
        let errors: Vec<_> = rx
            .try_iter()
            .filter_map(|e| {
                if let Event::Warning(s) = e {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("fixture unreachable"));
        assert!(!errors[0].contains("numpy-base"));
        assert_eq!(fs::read_to_string(&config.devel_path).unwrap(), baseline);
    }
}
