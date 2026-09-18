//! Composable removal options, held separately from an executable confirmation.
use super::settings::Language;

pub struct Removal {
    pub package: String,
    pub cursor: usize,
    pub checked: [bool; 5],
}
impl Removal {
    pub fn new(package: String) -> Self {
        Self {
            package,
            cursor: 0,
            checked: [true, true, true, false, false],
        }
    }
    pub fn toggle(&mut self) {
        self.checked[self.cursor] = !self.checked[self.cursor];
        match self.cursor {
            0 if !self.checked[0] => self.checked[1] = false,
            1 if self.checked[1] => self.checked[0] = true,
            2 if self.checked[2] => self.checked[4] = false,
            4 if self.checked[4] => self.checked[2] = false,
            _ => {}
        }
    }
    pub fn args(&self) -> Vec<String> {
        let mut flags = String::from("-R");
        for (enabled, flag) in self.checked[..4].iter().zip(['s', 's', 'c', 'n']) {
            if *enabled {
                flags.push(flag);
            }
        }
        let mut args = vec![flags];
        if self.checked[4] {
            args.push("--unneeded".into());
        }
        args.extend(["--".into(), self.package.clone()]);
        args
    }
    pub fn label(index: usize, language: Language) -> (&'static str, &'static str, &'static str) {
        let zh = language == Language::ZhCn;
        match index {
            0 => (
                "-s",
                if zh {
                    "清理不再需要的依赖"
                } else {
                    "Remove unused dependencies"
                },
                if zh {
                    "递归移除不再被其他包依赖、且原本作为依赖安装的软件包。"
                } else {
                    "Recursively remove dependencies no longer required by other packages, excluding explicitly installed packages."
                },
            ),
            1 => (
                "-ss",
                if zh {
                    "也清理显式安装的依赖"
                } else {
                    "Include explicit dependencies"
                },
                if zh {
                    "在 -s 基础上，也移除不再被其他包依赖、但曾由用户显式安装的依赖包。"
                } else {
                    "Extend -s to include unused dependencies that were explicitly installed. Enables -s as well."
                },
            ),
            2 => (
                "-c",
                if zh {
                    "级联删除依赖此包的软件"
                } else {
                    "Cascade to dependent packages"
                },
                if zh {
                    "递归删除所有依赖目标包的软件，可能影响多个应用。与“跳过仍被依赖的包”互斥。"
                } else {
                    "Recursively remove packages depending on the target; may affect multiple applications. Excludes skipping required targets."
                },
            ),
            3 => (
                "-n",
                if zh {
                    "不保留配置备份"
                } else {
                    "Do not save configuration backups"
                },
                if zh {
                    "删除包管理器登记的配置文件时不生成 .pacsave；不会清理用户主目录中的配置。"
                } else {
                    "Do not create .pacsave backups for package-managed configuration files. Home-directory configuration is unaffected."
                },
            ),
            _ => (
                "--unneeded",
                if zh {
                    "跳过仍被依赖的包"
                } else {
                    "Skip required targets"
                },
                if zh {
                    "只删除不被其他包依赖的目标；与级联删除互斥。"
                } else {
                    "Remove only targets not required by other packages. Mutually exclusive with cascading removal."
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn combinations_preserve_targets_and_option_dependencies() {
        let mut r = Removal::new("example".into());
        assert_eq!(r.args(), ["-Rssc", "--", "example"]);
        r.toggle();
        r.cursor = 2;
        r.toggle();
        r.cursor = 0;
        assert_eq!(r.args(), ["-R", "--", "example"]);
        r.toggle();
        r.cursor = 2;
        r.toggle();
        assert_eq!(r.args(), ["-Rsc", "--", "example"]);
        r.cursor = 1;
        r.toggle();
        r.cursor = 3;
        r.toggle();
        assert_eq!(r.args(), ["-Rsscn", "--", "example"]);
        r.cursor = 4;
        r.toggle();
        assert_eq!(r.args(), ["-Rssn", "--unneeded", "--", "example"]);
        r.cursor = 0;
        r.toggle();
        assert!(!r.checked[1]);
        assert_eq!(r.args(), ["-Rn", "--unneeded", "--", "example"]);
    }
}
