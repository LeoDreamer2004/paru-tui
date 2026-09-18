use crate::config::Config;
use anyhow::Result;
use cini::Ini;
use raur::Raur;
use serde::{Deserialize, Serialize};
use srcinfo::Srcinfo;
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::mpsc::Sender,
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Remote {
    #[serde(default)]
    pub target: Option<String>,
    pub updated: Option<i64>,
    pub aur_url: Option<String>,
    pub orphaned: bool,
    pub out_of_date: Option<i64>,
    pub fields: Vec<(String, String)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    #[serde(default)]
    pub devel: bool,
    #[serde(default)]
    pub remote: Option<Remote>,
    pub name: String,
    pub base: String,
    pub source: String,
    pub version: String,
    pub installed: Option<String>,
    pub next: Option<String>,
    pub description: String,
    pub url: String,
    pub size: i64,
    pub dependencies: Vec<String>,
    pub ignored: bool,
    pub search: String,
}
impl Package {
    pub fn is_aur(&self) -> bool {
        self.remote
            .as_ref()
            .map_or(self.source == "aur", |r| r.aur_url.is_some())
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub installed: Vec<Package>,
    #[serde(default)]
    pub dependencies: super::tree::Graph,
    pub aur_loaded: bool,
    pub repo_fresh: bool,
}
#[derive(Clone, Debug)]
pub struct CommentSpan {
    pub text: String,
    pub url: Option<String>,
}
#[derive(Clone, Debug)]
pub struct CommentLine {
    pub spans: Vec<CommentSpan>,
    pub code: bool,
}
impl CommentLine {
    pub fn plain(text: impl Into<String>, code: bool) -> Self {
        Self {
            spans: vec![CommentSpan {
                text: text.into(),
                url: None,
            }],
            code,
        }
    }
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}
#[derive(Clone, Debug)]
pub struct AurComment {
    pub id: String,
    pub header: String,
    pub pinned: bool,
    pub lines: Vec<CommentLine>,
}
pub enum Event {
    Loaded(Catalog),
    Inspection(String, Vec<(String, String)>),
    Error(String),
    Warning(String),
    DetailError(String, String),
    Pkgbuild {
        id: u64,
        base: String,
        text: String,
    },
    PkgbuildError {
        id: u64,
        message: String,
    },
    AurResolved {
        id: u64,
        name: String,
        base: String,
    },
    Comments {
        id: u64,
        comments: Vec<AurComment>,
        done: bool,
        error: Option<String>,
    },
    Search {
        id: u64,
        packages: Vec<Package>,
        done: bool,
        error: Option<String>,
    },
}
pub(super) fn package(pkg: &alpm::Package, source: &str, installed: Option<String>) -> Package {
    let description = pkg.desc().unwrap_or_default().to_string();
    Package {
        devel: false,
        remote: None,
        name: pkg.name().into(),
        base: pkg.base().unwrap_or(pkg.name()).into(),
        source: source.into(),
        version: pkg.version().to_string(),
        installed,
        next: None,
        description: description.clone(),
        url: pkg.url().unwrap_or_default().into(),
        size: pkg.isize(),
        dependencies: pkg.depends().iter().map(|d| d.to_string()).collect(),
        ignored: pkg.should_ignore(),
        search: format!("{} {} {}", pkg.name(), source, description).to_lowercase(),
    }
}
pub(super) fn mark_aur(package: &mut Package, base: &str) {
    package.source = "aur".into();
    package.base = base.into();
    package.search = format!("{} aur {}", package.name, package.description).to_lowercase();
}

fn cached_aur_bases(clone_dir: &Path, installed: &HashSet<String>) -> BTreeMap<String, String> {
    let mut bases = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(clone_dir) else {
        return bases;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path().join(".SRCINFO");
        let Ok(srcinfo) = Srcinfo::from_path(path) else {
            continue;
        };
        let base = srcinfo.base.pkgbase.to_owned();
        for package in srcinfo.pkgs {
            if installed.contains(&*package.pkgname) {
                bases.insert(package.pkgname.to_string(), base.clone());
            }
        }
    }
    bases
}
pub(super) fn config() -> Result<Config> {
    let mut config = Config::new()?;
    if let Some(path) = &config.config_path {
        let text = std::fs::read_to_string(path)?;
        config.parse(None, &text)?;
    }
    config.parse_args(["-Q", "--color", "never"])?;
    // Database diagnostics belong in the UI error path, never on its terminal.
    config.alpm.set_log_cb((), |_, _, _| {});
    Ok(config)
}
pub fn load(sender: Sender<Event>, settings: super::settings::Settings) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(load_inner(&sender, &settings)));
        if let Err(e) = result {
            let _ = sender.send(Event::Error(format!("{e:#}")));
        }
    });
}
async fn load_inner(sender: &Sender<Event>, settings: &super::settings::Settings) -> Result<()> {
    let config = config()?;
    let mut sync = BTreeMap::new();
    for db in config.alpm.syncdbs() {
        for pkg in db.pkgs() {
            sync.entry(pkg.name().to_string())
                .or_insert((pkg, db.name()));
        }
    }
    let mut catalog = Catalog::default();
    for pkg in config.alpm.localdb().pkgs() {
        let found = sync.get(pkg.name());
        let mut entry = package(
            pkg,
            found.map_or("foreign", |(_, db)| db),
            Some(pkg.version().to_string()),
        );
        if let Some((remote, _)) = found {
            if remote.version() > pkg.version() {
                entry.next = Some(remote.version().to_string());
            }
        }
        catalog.installed.push(entry);
    }
    catalog.installed.sort_by(|a, b| a.name.cmp(&b.name));
    catalog.dependencies = super::tree::Graph::installed(&config.alpm, &catalog.installed);
    let initial_foreign = catalog
        .installed
        .iter()
        .filter(|package| package.source == "foreign")
        .map(|package| package.name.clone())
        .collect::<HashSet<_>>();
    let installed = catalog
        .installed
        .iter()
        .map(|p| p.name.clone())
        .collect::<HashSet<_>>();
    let cached = cached_aur_bases(&config.fetch.clone_dir, &installed);
    for package in &mut catalog.installed {
        if package.source == "foreign" {
            if let Some(base) = cached.get(&package.name) {
                mark_aur(package, base);
            }
        }
    }
    let _ = sender.send(Event::Loaded(catalog.clone()));
    match fresh_repositories(&config, &mut catalog) {
        Ok(()) => {
            catalog.repo_fresh = true;
        }
        Err(e) => {
            let _ = sender.send(Event::Warning(format!(
                "Repository refresh failed; showing local cache: {e}"
            )));
        }
    }
    let foreign: Vec<_> = catalog
        .installed
        .iter()
        .filter(|p| {
            p.source == "foreign" || (!catalog.repo_fresh && initial_foreign.contains(&p.name))
        })
        .map(|p| p.name.clone())
        .collect();
    for package in &mut catalog.installed {
        if package.source == "foreign" {
            if let Some(base) = cached.get(&package.name) {
                mark_aur(package, base);
            }
        }
    }
    let _ = sender.send(Event::Loaded(catalog.clone()));
    for chunk in foreign.chunks(50) {
        let packages =
            match tokio::time::timeout(std::time::Duration::from_secs(15), config.raur.info(chunk))
                .await
            {
                Ok(Ok(packages)) => packages,
                Ok(Err(error)) => {
                    let _ = sender.send(Event::Warning(format!("AUR lookup failed: {error:#}")));
                    break;
                }
                Err(_) => {
                    let _ = sender.send(Event::Warning("AUR lookup timed out".into()));
                    break;
                }
            };
        for remote in packages {
            if let Ok(index) = catalog
                .installed
                .binary_search_by(|package| package.name.cmp(&remote.name))
            {
                let package = &mut catalog.installed[index];
                mark_aur(package, &remote.package_base);
                if alpm::Version::new(remote.version.as_str())
                    > alpm::Version::new(package.version.as_str())
                {
                    package.next = Some(remote.version);
                }
            }
        }
    }
    let _ = sender.send(Event::Loaded(catalog.clone()));
    if let Err(e) = super::devel::scan(&config, settings, &mut catalog, sender).await {
        let _ = sender.send(Event::Warning(format!("Git update check failed: {e:#}")));
    }
    catalog.aur_loaded = true;
    let _ = sender.send(Event::Loaded(catalog));
    Ok(())
}

// Refresh only an isolated copy of the sync databases. Never run -Sy against
// the system database while merely browsing updates.
fn fresh_repositories(config: &Config, catalog: &mut Catalog) -> Result<()> {
    use std::fs;
    let temporary = tempfile::tempdir()?;
    let sync = temporary.path().join("sync");
    fs::create_dir(&sync)?;
    let original = std::path::Path::new(&config.pacman.db_path).join("sync");
    if original.is_dir() {
        for entry in fs::read_dir(original)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|e| e == "db" || e == "sig")
            {
                let destination = sync.join(entry.file_name());
                fs::copy(entry.path(), &destination)?;
                fs::File::options()
                    .write(true)
                    .open(destination)?
                    .set_times(fs::FileTimes::new().set_modified(entry.metadata()?.modified()?))?;
            }
        }
    }
    let mut alpm = alpm::Alpm::new(
        config.pacman.root_dir.as_str(),
        temporary.path().to_str().unwrap(),
    )?;
    alpm_utils::configure_alpm(&mut alpm, &config.pacman)?;
    alpm.set_logfile(temporary.path().join("preview.log").to_str().unwrap())?;
    alpm.syncdbs_mut().update(false)?;
    let mut available = BTreeMap::new();
    for db in alpm.syncdbs() {
        for pkg in db.pkgs() {
            available.entry(pkg.name()).or_insert((pkg, db.name()));
        }
    }
    for installed in &mut catalog.installed {
        if let Some((remote, source)) = available.get(installed.name.as_str()) {
            installed.source = (*source).into();
            installed.search = format!(
                "{} {} {}",
                installed.name, installed.source, installed.description
            )
            .to_lowercase();
            installed.next = (remote.version() > alpm::Version::new(installed.version.as_str()))
                .then(|| remote.version().to_string());
        } else {
            installed.source = "foreign".into();
            installed.search =
                format!("{} foreign {}", installed.name, installed.description).to_lowercase();
            installed.next = None;
        }
    }
    Ok(())
}
pub fn inspect(name: String, local: bool, key: String, sender: Sender<Event>) {
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<(String, String)>> {
            let config = config()?;
            let p = if local {
                config.alpm.localdb().pkg(name.as_str())?
            } else {
                config
                    .alpm
                    .syncdbs()
                    .iter()
                    .find_map(|db| db.pkg(name.as_str()).ok())
                    .ok_or_else(|| anyhow::anyhow!("Package not found"))?
            };
            Ok(repository_fields(p, local))
        })();
        let _ = sender.send(match result {
            Ok(fields) => Event::Inspection(key, fields),
            Err(e) => Event::DetailError(key, e.to_string()),
        });
    });
}
pub(super) fn repository_fields(p: &alpm::Package, local: bool) -> Vec<(String, String)> {
    let date = |v: i64| {
        chrono::DateTime::from_timestamp(v, 0)
            .map(|d| d.with_timezone(&chrono::Local).to_string())
            .unwrap_or_default()
    };
    let mut fields = vec![
        ("Architecture".into(), p.arch().unwrap_or_default().into()),
        (
            "Licenses".into(),
            p.licenses().iter().collect::<Vec<_>>().join("  "),
        ),
        (
            "Groups".into(),
            p.groups().iter().collect::<Vec<_>>().join("  "),
        ),
        (
            "Provides".into(),
            p.provides()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("  "),
        ),
        (
            "Optional dependencies".into(),
            p.optdepends()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "Conflicts with".into(),
            p.conflicts()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("  "),
        ),
        (
            "Replaces".into(),
            p.replaces()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("  "),
        ),
        ("Packager".into(), p.packager().unwrap_or_default().into()),
        ("Build date".into(), date(p.build_date())),
    ];
    if local {
        fields.push(("Validated by".into(), format!("{:?}", p.validation())));
        fields.push((
            "Install script".into(),
            if p.has_scriptlet() { "Yes" } else { "No" }.into(),
        ));
        fields.push((
            "Backup files".into(),
            p.backup()
                .iter()
                .map(|b| b.name().to_owned())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        fields.push((
            "Install date".into(),
            p.install_date().map(date).unwrap_or_default(),
        ));
        fields.push(("Install reason".into(), format!("{:?}", p.reason())));
        fields.push((
            "Required by".into(),
            p.required_by()
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("  "),
        ));
        fields.push((
            "Optional for".into(),
            p.optional_for()
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("  "),
        ));
    }
    for (_, value) in &mut fields {
        if value.is_empty() {
            *value = "None".into();
        }
    }
    fields
}

pub(super) fn repository_package(config: &Config, p: &alpm::Package) -> Package {
    let source = p.db().map(|db| db.name()).unwrap_or("unknown");
    let installed = config
        .alpm
        .localdb()
        .pkg(p.name())
        .ok()
        .map(|p| p.version().to_string());
    let mut result = package(p, source, installed);
    let mut fields = repository_fields(p, false);
    fields.push((
        "Download size".into(),
        format!("{:.2} MiB", p.size() as f64 / 1048576.0),
    ));
    fields.push(("Date source".into(), "Repository build date".into()));
    result.remote = Some(Remote {
        updated: (p.build_date() > 0).then_some(p.build_date()),
        fields,
        ..Remote::default()
    });
    result
}

pub(super) fn aur_package(config: &Config, p: raur::Package, detailed: bool) -> Result<Package> {
    let mut url = config.aur_url.clone();
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid AUR URL"))?
        .pop_if_empty()
        .push("packages")
        .push(&p.name);
    url.set_query(None);
    url.set_fragment(None);
    let mut fields = vec![
        (
            "Maintainer".into(),
            p.maintainer.clone().unwrap_or_else(|| "None".into()),
        ),
        ("Last modified".into(), date(p.last_modified)),
        ("First submitted".into(), date(p.first_submitted)),
        ("Votes".into(), p.num_votes.to_string()),
        ("Popularity".into(), format!("{:.2}", p.popularity)),
        ("Date source".into(), "AUR last modified".into()),
    ];
    if detailed {
        fields.extend([
            ("Depends on".into(), p.depends.join("  ")),
            ("Build dependencies".into(), p.make_depends.join("  ")),
            ("Check dependencies".into(), p.check_depends.join("  ")),
            ("Optional dependencies".into(), p.opt_depends.join("\n")),
            ("Provides".into(), p.provides.join("  ")),
            ("Conflicts with".into(), p.conflicts.join("  ")),
            ("Replaces".into(), p.replaces.join("  ")),
            ("Licenses".into(), p.license.join("  ")),
            ("Groups".into(), p.groups.join("  ")),
        ]);
    }
    let description = p.description.unwrap_or_default();
    Ok(Package {
        devel: false,
        remote: Some(Remote {
            target: Some(format!("{}/{}", config.aur_namespace(), p.name)),
            updated: (p.last_modified > 0).then_some(p.last_modified),
            aur_url: Some(url.into()),
            orphaned: p.maintainer.is_none(),
            out_of_date: p.out_of_date,
            fields,
        }),
        installed: config
            .alpm
            .localdb()
            .pkg(p.name.as_str())
            .ok()
            .map(|p| p.version().to_string()),
        search: format!("{} aur {}", p.name, description).to_lowercase(),
        name: p.name,
        base: p.package_base,
        source: "aur".into(),
        version: p.version,
        next: None,
        description,
        url: p.url.unwrap_or_default(),
        size: 0,
        dependencies: Vec::new(),
        ignored: false,
    })
}
pub(super) fn date(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "None".into();
    }
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|d| d.with_timezone(&chrono::Local).to_string())
        .unwrap_or_else(|| "None".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_split_pkgbuild_identifies_installed_aur_packages() {
        let directory = tempfile::tempdir().unwrap();
        let clone = directory.path().join("split-base");
        std::fs::create_dir(&clone).unwrap();
        std::fs::write(
            clone.join(".SRCINFO"),
            "pkgbase = split-base\n\tpkgver = 1\n\tpkgrel = 1\npkgname = split-one\npkgname = split-two\n",
        )
        .unwrap();
        let installed = HashSet::from(["split-two".to_owned(), "unrelated".to_owned()]);
        let cached = cached_aur_bases(directory.path(), &installed);
        assert_eq!(
            cached.get("split-two").map(String::as_str),
            Some("split-base")
        );
        assert!(!cached.contains_key("split-one"));
        assert!(!cached.contains_key("unrelated"));
    }
}
