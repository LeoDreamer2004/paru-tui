use crate::config::Config;
use crate::exec::command_status;
use crate::exec::{self};
use crate::printtr;
use crate::util::ask;

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use anyhow::Result;
use aur_depends::Actions;
use aur_depends::Base;
use srcinfo::Srcinfo;
use tr::tr;

pub fn check_pgp_keys(
    config: &Config,
    actions: &Actions,
    srcinfos: &HashMap<String, Srcinfo>,
) -> Result<()> {
    let mut import: HashMap<&str, Vec<&Base>> = HashMap::new();
    let mut seen = HashSet::new();
    let c = config.color;

    for base in &actions.build {
        let srcinfo = match base {
            Base::Aur(base) => {
                let pkg = base.package_base();
                srcinfos.get(pkg).unwrap()
            }
            Base::Pkgbuild(base) => base.srcinfo.as_ref(),
        };
        for key in &srcinfo.base.valid_pgp_keys {
            if !seen.insert(key) {
                continue;
            }

            let mut cmd = Command::new(&config.gpg_bin);
            cmd.args(&config.gpg_flags)
                .arg("--list-keys")
                .arg(key)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let status = command_status(&mut cmd)?;

            if status.success().is_err() {
                import.entry(key).or_default().push(base);
            }
        }
    }

    if !import.is_empty() {
        println!(
            "{} {}",
            c.action.paint("::"),
            c.bold.paint(tr!("keys need to be imported:"))
        );
        for (key, base) in &import {
            let base = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            printtr!(
                "     {key} wanted by: {base}",
                key = c.bold.paint(*key),
                base = base.join("  ")
            );
        }
        let prompt = "import?".to_owned();
        #[cfg(feature = "tui")]
        let prompt = if crate::tui::bridge::connected() {
            format!(
                "Import build signing keys?\n{}",
                import
                    .iter()
                    .map(|(key, bases)| format!(
                        "{}: {}",
                        key,
                        bases
                            .iter()
                            .map(|b| b.package_base())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        } else {
            prompt
        };
        if ask(config, &prompt, true) {
            import_keys(config, &import)?;
        }
    }

    Ok(())
}

fn import_keys(config: &Config, import: &HashMap<&str, Vec<&Base>>) -> Result<()> {
    let mut cmd = Command::new(&config.gpg_bin);
    cmd.args(&config.gpg_flags)
        .arg("--recv-keys")
        .args(import.keys());
    exec::command(&mut cmd)
}
