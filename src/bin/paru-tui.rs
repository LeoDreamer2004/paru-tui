fn main() {
    if let Err(error) = paru::tui::entry() {
        eprintln!("paru-tui: {error:#}");
        std::process::exit(1);
    }
}
