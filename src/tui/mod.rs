pub mod bridge;
pub(crate) mod cache;
mod catalog;
mod comments;
mod devel;
mod downloads;
mod i18n;
mod motion;
mod pty;
mod raster;
mod removal;
mod search;
mod selection;
mod session;
pub mod settings;
mod theme;
pub(crate) mod transaction;
mod tree;
mod ui;

pub fn entry() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if std::path::Path::new(&args[0])
        .file_name()
        .is_some_and(|s| s == "git")
    {
        return settings::git_helper(&args[1..]);
    }
    if std::path::Path::new(&args[0])
        .file_name()
        .is_some_and(|s| s == "paru-tui-askpass")
    {
        bridge::connect(&std::env::var("PARU_TUI_SOCKET")?)?;
        let password =
            bridge::password(args.get(1).map(String::as_str).unwrap_or("sudo password"))?;
        println!("{password}");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--alpm-worker") {
        bridge::connect(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing frontend socket"))?,
        )?;
        return transaction::run_file(
            args.get(3)
                .ok_or_else(|| anyhow::anyhow!("Missing transaction request"))?,
        );
    }
    if args.get(1).is_some_and(|s| s == "--worker") {
        bridge::connect(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing worker socket"))?,
        )?;
        let rt = tokio::runtime::Runtime::new()?;
        let code = rt.block_on(crate::run(&args[3..]));
        std::process::exit(code);
    }
    if args.get(1).is_some_and(|s| s == "--help" || s == "-h") {
        println!("paru-tui — native package workspace\n\n1 Updates · 2 Install · 3 List · 4 Settings · 5 Activity\nTab/Shift+Tab source/panel · Up/Down package · / search\nu update current source · a update all · p AUR proxy rule\nEnter search/install selected package · o open AUR page\nList: t flat/tree · Left/Right collapse/expand · Enter/Space toggle\nList: d/Delete removal options · Space toggle · Enter review\nTab panel focus · 1–5 pages · r refresh · q quit\n\nSettings: {}", settings::Settings::path()?.display());
        return Ok(());
    }
    if args.len() != 1 {
        anyhow::bail!("Unknown arguments; use --help");
    }
    ui::run()
}
