use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    En,
    ZhCn,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Accent {
    #[default]
    Teal,
    Blue,
    Purple,
    Pink,
    Amber,
}
impl Accent {
    pub const ALL: [Self; 5] = [
        Self::Teal,
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Amber,
    ];
    pub fn next(self) -> Self {
        Self::ALL[(Self::ALL.iter().position(|c| *c == self).unwrap() + 1) % Self::ALL.len()]
    }
    pub fn name(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Teal, Language::En) => "Teal",
            (Self::Blue, Language::En) => "Blue",
            (Self::Purple, Language::En) => "Purple",
            (Self::Pink, Language::En) => "Pink",
            (Self::Amber, Language::En) => "Amber",
            (Self::Teal, Language::ZhCn) => "青绿",
            (Self::Blue, Language::ZhCn) => "天蓝",
            (Self::Purple, Language::ZhCn) => "浅紫",
            (Self::Pink, Language::ZhCn) => "玫粉",
            (Self::Amber, Language::ZhCn) => "琥珀",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub language: Language,
    pub accent: Accent,
    pub proxy_url: String,
    pub git_proxy: bool,
    pub proxy_bases: BTreeSet<String>,
}
impl Settings {
    pub fn path() -> Result<PathBuf> {
        Ok(dirs::config_dir()
            .context("No config directory")?
            .join("paru-tui/settings.toml"))
    }
    pub fn load() -> Result<Self> {
        Self::load_path(&Self::path()?)
    }
    pub fn load_path(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text).context("Invalid paru-tui settings")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn set_proxy_input(&mut self, input: &str) -> Result<()> {
        let input = input.trim();
        let proxy_url = if input.is_empty() || input.contains("://") {
            input.to_owned()
        } else {
            url::Url::parse(&format!("http://{input}"))
                .context("Use host:port or a complete proxy URL")?;
            if !input
                .rsplit_once(':')
                .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
            {
                bail!("Include a port, for example 127.0.0.1:7890");
            }
            format!("http://{input}")
        };
        let candidate = Self {
            proxy_url,
            ..self.clone()
        };
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        if self.proxy_url.is_empty() {
            if self.git_proxy || !self.proxy_bases.is_empty() {
                bail!("Configure a proxy URL before enabling proxy rules");
            }
            return Ok(());
        }
        let url = url::Url::parse(&self.proxy_url).context("Invalid proxy URL")?;
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            || url.host_str().is_none()
        {
            bail!("Use http(s)://host:port or socks5(h)://host:port");
        }
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let path = Self::path()?;
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        use std::io::Write;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
    pub fn use_proxy(&self, base: Option<&str>, git: bool) -> bool {
        !self.proxy_url.is_empty()
            && (base.is_some_and(|b| self.proxy_bases.contains(b)) || (git && self.git_proxy))
    }
}
const PROXY_ENV: &[&str] = &[
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];
pub fn route(command: &mut Command, settings: &Settings, base: Option<&str>, git: bool) {
    for key in PROXY_ENV {
        command.env_remove(key);
    }
    if settings.use_proxy(base, git) {
        for key in &PROXY_ENV[..6] {
            command.env(key, &settings.proxy_url);
        }
    }
}
fn worker_settings() -> Option<Settings> {
    let path = std::env::var_os("PARU_TUI_POLICY")?;
    // Invalid execution policy must never silently fall back to direct access.
    match Settings::load_path(Path::new(&path)).and_then(|s| {
        s.validate()?;
        Ok(s)
    }) {
        Ok(settings) => Some(settings),
        Err(e) => {
            eprintln!("Cannot load proxy policy: {e}");
            std::process::exit(1);
        }
    }
}
pub fn base_in(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(".SRCINFO"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("pkgbase = ").map(str::to_owned))
        })
}
pub fn configure_build(command: &mut Command, dir: &Path) {
    if let Some(settings) = worker_settings() {
        let base =
            base_in(dir).or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()));
        route(command, &settings, base.as_deref(), false);
        if let Some(base) = base {
            command.env("PARU_TUI_BUILD_BASE", base);
        }
    }
}
pub(crate) fn validate_worker(chroot: bool) -> Result<()> {
    if super::bridge::connected() && chroot {
        bail!("Native TUI transactions do not support chroot builds yet");
    }
    if let Some(settings) = worker_settings() {
        if chroot && (!settings.proxy_bases.is_empty() || settings.git_proxy) {
            bail!("Managed proxy rules are not supported inside a chroot; disable chroot mode for this operation");
        }
    }
    Ok(())
}
pub fn git_helper(args: &[String]) -> Result<()> {
    let settings = worker_settings().context("Missing worker proxy policy")?;
    let real = std::env::var_os("PARU_TUI_REAL_GIT").context("Missing real git path")?;
    let mut dir = std::env::current_dir()?;
    for pair in args.windows(2) {
        if pair[0] == "-C" {
            dir = dir.join(&pair[1]);
        }
    }
    let base = std::env::var("PARU_TUI_BUILD_BASE")
        .ok()
        .or_else(|| base_in(&dir))
        .or_else(|| {
            args.iter().find_map(|s| {
                if s.contains("://") && !s.starts_with('-') {
                    let name = s
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()?
                        .trim_end_matches(".git");
                    Some(name.to_string())
                } else {
                    None
                }
            })
        })
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()));
    let mut command = Command::new(real);
    let remote = args
        .iter()
        .find(|s| {
            s.starts_with("git://")
                || s.starts_with("https://")
                || s.starts_with("http://")
                || s.starts_with("ssh://")
                || s.starts_with("git@")
        })
        .cloned()
        .or_else(|| {
            Command::new(std::env::var_os("PARU_TUI_REAL_GIT")?)
                .args(["config", "--get", "remote.origin.url"])
                .current_dir(&dir)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });
    configure_git(&mut command, &settings, base.as_deref(), remote.as_deref())?;
    command.args(args);
    Err(command.exec().into())
}

pub(super) fn configure_git(
    command: &mut Command,
    settings: &Settings,
    base: Option<&str>,
    remote: Option<&str>,
) -> Result<()> {
    if settings.use_proxy(base, true)
        && remote.is_some_and(|r| {
            r.starts_with("ssh://") || r.starts_with("git@") || r.starts_with("git://")
        })
    {
        bail!("Managed proxy requires an HTTP(S) Git remote; refusing an unproxied SSH/git connection");
    }
    route(command, settings, base, true);
    // Per-command override defeats inherited Git proxy configuration for direct repositories.
    command.arg("-c").arg(format!(
        "http.proxy={}",
        if settings.use_proxy(base, true) {
            &settings.proxy_url
        } else {
            ""
        }
    ));
    Ok(())
}
pub fn configure_sudo(command: &mut Command, binary: &str) {
    if std::env::var_os("PARU_TUI_SOCKET").is_some()
        && Path::new(binary).file_name().is_some_and(|s| s == "sudo")
    {
        command.arg("-A");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_presets_cycle_and_round_trip_with_legacy_settings() {
        let legacy: Settings = toml::from_str("language = 'en'").unwrap();
        assert_eq!(legacy.accent, Accent::Teal);
        let mut settings = legacy;
        for preset in Accent::ALL {
            assert_eq!(settings.accent, preset);
            let saved = toml::to_string(&settings).unwrap();
            assert_eq!(toml::from_str::<Settings>(&saved).unwrap().accent, preset);
            settings.accent = settings.accent.next();
        }
        assert_eq!(settings.accent, Accent::Teal);
    }
    #[test]
    fn proxy_scope_is_per_base_and_git_override() {
        let mut s = Settings {
            proxy_url: "socks5h://127.0.0.1:1080".into(),
            ..Default::default()
        };
        s.proxy_bases.insert("split-base".into());
        assert!(s.use_proxy(Some("split-base"), false));
        assert!(!s.use_proxy(Some("other"), false));
        assert!(!s.use_proxy(Some("other"), true));
        s.git_proxy = true;
        assert!(s.use_proxy(Some("other"), true));
        assert!(!s.use_proxy(Some("other"), false));
    }
    #[test]
    fn proxy_input_accepts_host_port_and_preserves_invalid_draft() {
        let mut s = Settings::default();
        s.set_proxy_input(" 127.0.0.1:7890 ").unwrap();
        assert_eq!(s.proxy_url, "http://127.0.0.1:7890");
        s.set_proxy_input("socks5h://127.0.0.1:1080").unwrap();
        assert_eq!(s.proxy_url, "socks5h://127.0.0.1:1080");
        assert!(s.set_proxy_input("bad address").is_err());
        assert_eq!(s.proxy_url, "socks5h://127.0.0.1:1080");
    }
    #[test]
    fn invalid_policy_is_rejected() {
        assert!(Settings {
            git_proxy: true,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Settings {
            proxy_url: "file:///tmp/proxy".into(),
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
