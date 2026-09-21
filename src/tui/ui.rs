use super::theme::{
    accent, mix, selection, BLUE, BORDER, GREEN, MUTED as DIM, RED, SURFACE as COMMAND_BG, TEXT,
    YELLOW,
};
use super::{
    bridge::Question,
    catalog::{self, AurComment, Catalog, Event, Package},
    session::Session,
    settings::{Language, Settings},
};
use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget, Wrap},
    Frame, Terminal,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    io::{self, IsTerminal},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
fn style(color: Color) -> Style {
    Style::default().fg(color)
}
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}
fn package_badges(package: &Package, language: Language) -> String {
    let Some(remote) = package.remote.as_ref().filter(|_| package.is_aur()) else {
        return String::new();
    };
    let zh = language == Language::ZhCn;
    format!(
        "{}{}",
        if remote.out_of_date.is_some() {
            if zh {
                "[旧]"
            } else {
                "[old]"
            }
        } else {
            ""
        },
        if remote.orphaned {
            if zh {
                "[孤]"
            } else {
                "[orphan]"
            }
        } else {
            ""
        },
    )
}
fn panel(title: String, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            title,
            style(if active { accent() } else { DIM }),
        ))
        .border_style(style(if active { accent() } else { BORDER }))
}
struct Guard(Vec<signal_hook::SigId>);
impl Drop for Guard {
    fn drop(&mut self) {
        restore();
        for id in &self.0 {
            signal_hook::low_level::unregister(*id);
        }
    }
}
fn restore() {
    let _ = disable_raw_mode();
    let _ = super::selection::cleanup(&mut io::stdout());
    let _ = execute!(
        io::stdout(),
        crossterm::terminal::EndSynchronizedUpdate,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}
struct Dialog {
    title: String,
    scroll: u16,
    body: String,
    question: Option<Question>,
    action: Option<Vec<String>>,
    yes: bool,
    input: String,
}
struct PkgbuildView {
    id: u64,
    base: String,
    pkgbuild: Option<String>,
    pkgbuild_error: Option<String>,
    comments: Vec<AurComment>,
    comments_loading: bool,
    comments_error: Option<String>,
    focus: usize,
    scroll: [u16; 2],
    scroll_max: [u16; 2],
}
struct CommentCache {
    id: u64,
    width: u16,
    language: Language,
    lines: Vec<Line<'static>>,
    links: Vec<super::comments::LinkHit>,
}
struct Toast {
    message: String,
    since: Instant,
    error: bool,
}
impl Toast {
    fn new(message: String) -> Self {
        let lower = message.to_lowercase();
        let error = ["error", "failed", "cannot", "invalid", "not saved"]
            .iter()
            .any(|s| lower.contains(s));
        Self {
            message,
            since: Instant::now(),
            error,
        }
    }
    fn backend(message: String) -> Option<Self> {
        let toast = Self::new(message);
        (toast.error && toast.message.trim() != "Error:").then_some(toast)
    }
    fn lifetime(&self) -> f32 {
        if self.error {
            8.0
        } else {
            4.0
        }
    }
    fn expansion(&self) -> f32 {
        let elapsed = self.since.elapsed().as_secs_f32();
        let smooth = |t: f32| {
            let t = t.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        if elapsed < 0.32 {
            smooth(elapsed / 0.32)
        } else {
            1.0 - smooth((elapsed - self.lifetime()) / 0.28)
        }
    }
    fn animating(&self) -> bool {
        let elapsed = self.since.elapsed().as_secs_f32();
        elapsed < 0.32 || (elapsed >= self.lifetime() && elapsed < self.lifetime() + 0.28)
    }
}
struct App {
    motion: RefCell<super::motion::Motion>,
    selection_layer: RefCell<super::selection::SelectionLayer>,
    tab_layer: RefCell<super::selection::SelectionLayer>,
    raster_lists: RefCell<super::raster::Lists>,
    dialog_scroll_max: u16,
    catalog: Catalog,
    confirmed_aur: HashMap<String, String>,
    inspections: HashMap<String, Vec<(String, String)>>,
    settings: Settings,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    loading: bool,
    page: usize,
    source: usize,
    focus: usize,
    activity_scroll: u16,
    toast: Option<Toast>,
    search_service: super::search::Search,
    install_query: String,
    submitted_query: String,
    search_results: Vec<Package>,
    search_indices: Vec<usize>,
    search_selected: usize,
    search_id: u64,
    search_pending: bool,
    selected: [usize; 2],
    package_selected: usize,
    updates: [Vec<usize>; 2],
    filtered: Vec<usize>,
    tree_mode: bool,
    tree: super::tree::Tree,
    query: String,
    searching: bool,
    editing_proxy: bool,
    proxy_error: Option<String>,
    proxy_input: String,
    setting_selected: usize,
    detail_scroll: u16,
    detail_pending: Option<String>,
    detail_requested: HashSet<String>,
    last_navigation: Instant,
    status: String,
    activity: VecDeque<String>,
    dialog: Option<Dialog>,
    pkgbuild_view: Option<PkgbuildView>,
    pkgbuild_generation: Arc<AtomicU64>,
    pkgbuild_panels: [Rect; 2],
    pkgbuild_comment_inner: Rect,
    pkgbuild_comment_cache: Option<CommentCache>,
    removal: Option<super::removal::Removal>,
    session: Option<Session>,
    reported: bool,
    help_open: bool,
    help_scroll: u16,
}
impl App {
    fn paragraph<'a>(&self, text: impl Into<Text<'a>>) -> Paragraph<'a> {
        let mut text = text.into();
        for line in &mut text.lines {
            for span in &mut line.spans {
                span.content = super::i18n::translate(&span.content, self.settings.language).into();
            }
        }
        Paragraph::new(text)
    }
    fn panel(&self, title: String, active: bool) -> Block<'static> {
        let strength = self
            .motion
            .borrow_mut()
            .focus(&title, active, Instant::now());
        let color = mix(BORDER, accent(), strength);
        panel(
            super::i18n::translate(&title, self.settings.language),
            active,
        )
        .border_style(style(color))
    }
    fn content_panel(&self, title: String, active: bool) -> Block<'static> {
        self.panel(title, active).padding(Padding::horizontal(2))
    }
    fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let settings = Settings::load()?;
        Ok(Self {
            motion: RefCell::new(Default::default()),
            selection_layer: RefCell::new(Default::default()),
            tab_layer: RefCell::new(Default::default()),
            raster_lists: RefCell::new(Default::default()),
            dialog_scroll_max: 0,
            catalog: Catalog::default(),
            confirmed_aur: HashMap::new(),
            inspections: HashMap::new(),
            settings,
            tx: tx.clone(),
            rx,
            loading: false,
            page: 0,
            source: 0,
            focus: 0,
            activity_scroll: 0,
            toast: None,
            search_service: super::search::Search::new(tx.clone()),
            install_query: String::new(),
            submitted_query: String::new(),
            search_results: Vec::new(),
            search_indices: Vec::new(),
            search_selected: 0,
            search_id: 0,
            search_pending: false,
            selected: [0, 0],
            package_selected: 0,
            updates: [vec![], vec![]],
            filtered: vec![],
            tree_mode: false,
            tree: Default::default(),
            query: String::new(),
            searching: false,
            editing_proxy: false,
            proxy_error: None,
            proxy_input: String::new(),
            setting_selected: 0,
            detail_scroll: 0,
            detail_pending: None,
            detail_requested: HashSet::new(),
            last_navigation: Instant::now(),
            status: "Reading package databases…".into(),
            activity: VecDeque::new(),
            dialog: None,
            pkgbuild_view: None,
            pkgbuild_generation: Arc::new(AtomicU64::new(0)),
            pkgbuild_panels: [Rect::default(); 2],
            pkgbuild_comment_inner: Rect::default(),
            pkgbuild_comment_cache: None,
            removal: None,
            session: None,
            reported: false,
            help_open: false,
            help_scroll: 0,
        })
    }
    fn note(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.show_notice(self.status.clone());
        self.activity.push_front(format!(
            "{}  {}",
            chrono::Local::now().format("%H:%M:%S"),
            self.status
        ));
        self.activity.truncate(100);
    }
    fn show_notice(&mut self, message: String) {
        let next = Toast::new(message);
        let correction = next.message.starts_with("Settings saved")
            || next.message.starts_with("Git proxy setting saved")
            || next.message.starts_with("Language saved");
        if !next.error
            && !correction
            && self
                .toast
                .as_ref()
                .is_some_and(|t| t.error && t.expansion() > 0.0)
        {
            return;
        }
        self.toast = Some(next);
    }
    fn refresh(&mut self) {
        if !self.loading {
            self.loading = true;
            catalog::load(self.tx.clone(), self.settings.clone());
            self.note("Scanning packages");
        }
    }
    fn packages(&self) -> &[Package] {
        &self.catalog.installed
    }
    fn inspection_key(&self, package: &Package) -> String {
        if self.page == 4 {
            format!(
                "search:{}:{}/{}",
                self.search_id, package.source, package.name
            )
        } else {
            format!("installed:{}", package.name)
        }
    }
    fn search_packages(&mut self) {
        self.search_id = self.search_id.wrapping_add(1);
        self.submitted_query = self.install_query.trim().to_owned();
        self.search_results.clear();
        self.search_indices.clear();
        self.inspections
            .retain(|key, _| !key.starts_with("search:"));
        self.detail_requested
            .retain(|key| !key.starts_with("search:"));
        self.detail_pending = None;
        self.search_selected = 0;
        self.search_pending = !self.submitted_query.is_empty();
        self.detail_scroll = 0;
        self.motion.borrow_mut().reset_lists();
        self.search_service
            .submit(self.search_id, self.submitted_query.clone());
    }
    fn reindex(&mut self) {
        self.motion.borrow_mut().reset_lists();
        let query = self.query.to_lowercase();
        self.updates = [vec![], vec![]];
        for (i, p) in self.catalog.installed.iter().enumerate() {
            if p.next.is_some() {
                let source = usize::from(p.is_aur());
                self.updates[source].push(i);
            }
        }
        for source in 0..2 {
            self.selected[source] =
                self.selected[source].min(self.updates[source].len().saturating_sub(1));
        }
        self.filtered = self
            .packages()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.search.contains(&query))
            .map(|(i, _)| i)
            .collect();
        if self.tree_mode {
            self.tree.sync(&self.catalog.installed);
            self.rebuild_tree();
        }
        self.package_selected = self
            .package_selected
            .min(self.filtered.len().saturating_sub(1));
    }
    fn rebuild_tree(&mut self) {
        let roots = if self.query.is_empty() {
            self.catalog.dependencies.roots.clone()
        } else {
            let query = self.query.to_lowercase();
            self.catalog
                .installed
                .iter()
                .enumerate()
                .filter(|(_, p)| p.search.contains(&query))
                .map(|(i, _)| i)
                .collect()
        };
        self.tree.rebuild(&self.catalog.dependencies, &roots);
        self.filtered = self.tree.rows.iter().map(|r| r.package).collect();
    }
    fn tree_key(&mut self, key: KeyCode) {
        let selected = self.package_selected;
        let Some(row) = self.tree.rows.get(selected) else {
            return;
        };
        match key {
            KeyCode::Left | KeyCode::Char('h') => {
                if row.open {
                    self.tree.set_open(selected, false);
                } else if let Some(parent) = row.parent {
                    self.package_selected = parent;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if row.open {
                    self.package_selected += 1;
                } else {
                    self.tree.set_open(selected, true);
                }
            }
            _ => self.tree.set_open(selected, !row.open),
        }
        self.rebuild_tree();
        self.detail_scroll = 0;
        self.last_navigation = Instant::now();
    }
    fn current(&self) -> Option<&Package> {
        if self.page == 0 {
            self.updates[self.source]
                .get(self.selected[self.source])
                .map(|i| &self.catalog.installed[*i])
        } else if self.page == 1 {
            self.filtered
                .get(self.package_selected)
                .map(|i| &self.packages()[*i])
        } else if self.page == 4 {
            self.search_results.get(self.search_selected)
        } else {
            None
        }
    }
    fn active(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.code.is_none())
    }
    fn tick(&mut self) -> Result<()> {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Search {
                    id,
                    packages,
                    done,
                    error,
                } => {
                    if id != self.search_id {
                        continue;
                    }
                    self.search_indices = (0..packages.len()).collect();
                    self.search_results = packages;
                    self.search_pending = !done;
                    self.search_selected = self
                        .search_selected
                        .min(self.search_results.len().saturating_sub(1));
                    if let Some(error) = error {
                        self.note(error);
                    }
                }
                Event::Loaded(mut catalog) => {
                    for package in &mut catalog.installed {
                        if package.source == "foreign" {
                            if let Some(base) = self.confirmed_aur.get(&package.name) {
                                catalog::mark_aur(package, base);
                            }
                        }
                    }
                    self.loading = !catalog.aur_loaded;
                    self.catalog = catalog;
                    let versions: HashMap<_, _> = self
                        .catalog
                        .installed
                        .iter()
                        .map(|p| (p.name.as_str(), p.version.as_str()))
                        .collect();
                    for result in &mut self.search_results {
                        result.installed =
                            versions.get(result.name.as_str()).map(|v| (*v).to_owned());
                    }
                    self.detail_pending = None;
                    self.inspections.clear();
                    self.detail_requested.clear();
                    self.reindex();
                    if !self.loading {
                        self.note("Scan complete");
                    }
                }
                Event::Inspection(key, fields) => {
                    if self.detail_pending.as_ref() == Some(&key) {
                        self.detail_pending = None;
                    }
                    if self.detail_requested.contains(&key) {
                        self.inspections.insert(key, fields);
                    }
                }
                Event::Pkgbuild { id, base, text } => {
                    if let Some(view) = self.pkgbuild_view.as_mut().filter(|view| view.id == id) {
                        view.base = base.clone();
                        view.pkgbuild = Some(text);
                        view.comments_loading = true;
                        self.pkgbuild_comment_cache = None;
                        super::search::view_comments(
                            id,
                            base,
                            self.tx.clone(),
                            self.pkgbuild_generation.clone(),
                        );
                    }
                }
                Event::PkgbuildError { id, message } => {
                    if let Some(view) = self.pkgbuild_view.as_mut().filter(|view| view.id == id) {
                        view.pkgbuild_error = Some(message);
                        self.pkgbuild_comment_cache = None;
                    }
                }
                Event::AurResolved { id, name, base } => {
                    if let Some(view) = self.pkgbuild_view.as_mut().filter(|view| view.id == id) {
                        view.base = base.clone();
                    }
                    self.confirmed_aur.insert(name.clone(), base.clone());
                    if let Some(package) = self
                        .catalog
                        .installed
                        .iter_mut()
                        .find(|package| package.name == name && package.source == "foreign")
                    {
                        catalog::mark_aur(package, &base);
                        self.reindex();
                    }
                }
                Event::Comments {
                    id,
                    comments,
                    done,
                    error,
                } => {
                    if let Some(view) = self.pkgbuild_view.as_mut().filter(|view| view.id == id) {
                        view.comments.extend(comments);
                        view.comments_loading = !done;
                        view.comments_error = error;
                        self.pkgbuild_comment_cache = None;
                    }
                }
                Event::DetailError(key, e) => {
                    if self.detail_pending.as_ref() == Some(&key) {
                        self.detail_pending = None;
                    }
                    if self.detail_requested.contains(&key) {
                        self.note(e);
                    }
                }
                Event::Warning(warning) => self.note(warning),
                Event::Error(error) => {
                    self.loading = false;
                    self.note(error);
                }
            }
        }
        if matches!(self.page, 1 | 4)
            && !self.searching
            && self.detail_pending.is_none()
            && self.last_navigation.elapsed() > Duration::from_millis(250)
        {
            if let Some(p) = self.current() {
                let key = self.inspection_key(p);
                let name = p.name.clone();
                let aur = self.page == 4 && p.is_aur();
                if (self.page == 1 || aur) && self.detail_requested.insert(key.clone()) {
                    self.detail_pending = Some(key.clone());
                    if aur {
                        super::search::inspect_aur(name, key, self.tx.clone());
                    } else {
                        catalog::inspect(name, true, key, self.tx.clone());
                    }
                }
            }
        }
        if let Some(session) = &mut self.session {
            if let Some(question) = session.tick()? {
                if question.notice {
                    self.status = question.text;
                    if let Some(next) = Toast::backend(self.status.clone()) {
                        self.toast = Some(next);
                    }
                    self.activity.push_front(format!(
                        "{}  {}",
                        chrono::Local::now().format("%H:%M:%S"),
                        self.status
                    ));
                    self.activity.truncate(100);
                } else {
                    let plan = question
                        .plan
                        .iter()
                        .map(|p| format!("{}/{}  {}", p.source, p.name, p.version))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.dialog = Some(Dialog {
                        scroll: 0,
                        title: question_title(&question).into(),
                        body: if plan.is_empty() {
                            question.text.clone()
                        } else {
                            format!("{}\n\n{plan}", question.text)
                        },
                        yes: question.default.unwrap_or(false),
                        question: Some(question),
                        action: None,
                        input: String::new(),
                    });
                }
            }
            if let Some(code) = session.code {
                if !self.reported
                    && session
                        .finished_at
                        .is_some_and(|t| t.elapsed() >= Duration::from_millis(150))
                {
                    self.reported = true;
                    self.dialog = None;
                    let outcome = if code == 0 {
                        "Operation completed".into()
                    } else {
                        format!(
                            "Operation failed with exit code {code}{}",
                            session
                                .last_error
                                .as_ref()
                                .map(|reason| format!("\n{reason}"))
                                .unwrap_or_default()
                        )
                    };
                    self.note(outcome);
                    self.refresh();
                }
            }
        }
        Ok(())
    }
    fn update(&mut self, all: bool) {
        if self.active() {
            self.note("An operation is already running");
            return;
        }
        let (title, args) = if all {
            ("UPDATE ALL", vec!["-Syu".into()])
        } else if self.source == 0 {
            ("UPDATE REPOSITORIES", vec!["-Syu".into(), "--repo".into()])
        } else {
            ("UPDATE AUR", vec!["-Sua".into()])
        };
        let mut args: Vec<String> = args;
        if all || self.source == 1 {
            args.push("--devel".into());
        }
        args.push("--noupgrademenu".into());
        self.dialog = Some(Dialog {
            scroll: 0, title: title.into(),
            body: format!("{} repository candidates  {} AUR candidates\n\nUpdate the selected source? The final transaction plan will be shown before installation.",
                self.updates[0].len(), self.updates[1].len()),
            question: None, action: Some(args), yes: true, input: String::new(),
        });
    }
    fn update_one(&mut self) {
        if self.active() {
            self.note("An operation is already running");
            return;
        }
        let Some(p) = self.current() else {
            return;
        };
        if !p.is_aur() {
            self.note("Enter updates one AUR package; use u for repository updates");
            return;
        }
        let mut args = vec!["-S".into(), "--aur".into()];
        if p.devel {
            args.push("--devel".into());
        }
        args.extend(["--".into(), p.name.clone()]);
        self.dialog = Some(Dialog {
            title: "UPDATE AUR PACKAGE".into(), scroll: 0,
            body: format!("Update {} to {}?\n\nOnly this AUR target and its required dependencies will be processed. Other AUR packages are not selected.", p.name, p.next.as_deref().unwrap_or(&p.version)),
            question: None, action: Some(args),
            yes: true, input: String::new(),
        });
    }
    fn remove(&mut self) {
        if self.active() {
            self.note("An operation is already running");
        } else if let Some(p) = self.current() {
            self.removal = Some(super::removal::Removal::new(p.name.clone()));
        }
    }
    fn removal_key(&mut self, key: KeyEvent) {
        let removal = self.removal.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => self.removal = None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                removal.cursor = (removal.cursor + 4) % 5;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                removal.cursor = (removal.cursor + 1) % 5;
            }
            KeyCode::Char(' ') => removal.toggle(),
            KeyCode::Enter => {
                let removal = self.removal.take().unwrap();
                let descriptions = (0..5)
                    .filter(|i| removal.checked[*i])
                    .map(|i| super::removal::Removal::label(i, self.settings.language).1)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.dialog = Some(Dialog {
                    title: "REMOVE PACKAGE".into(), scroll: 0,
                    body: format!("Remove {}?\n\n{}\n\nThe final removal plan will be shown before any packages are removed.", removal.package,
                        if descriptions.is_empty() { super::i18n::translate("Remove only the selected package.", self.settings.language) } else { descriptions }),
                    action: Some(removal.args()), question: None, yes: true, input: String::new(),
                });
            }
            _ => {}
        }
    }
    fn install(&mut self) {
        if self.active() {
            self.note("An operation is already running");
            return;
        }
        if let Some(p) = self.current() {
            let target = p
                .remote
                .as_ref()
                .and_then(|r| r.target.clone())
                .unwrap_or_else(|| format!("{}/{}", p.source, p.name));
            let args = vec!["-S".into(), "--".into(), target];
            self.dialog=Some(Dialog{scroll:0,title:"INSTALL PACKAGE".into(),body:format!("Install {} {} from {}?\n\nDependencies and the final plan are checked by paru.",p.name,p.version,p.source),question:None,action:Some(args),yes:true,input:String::new()});
        }
    }
    fn toggle_proxy(&mut self) {
        let Some(p) = self.current() else {
            return;
        };
        if !p.is_aur() {
            self.note("Proxy rules are available for confirmed AUR package bases");
            return;
        }
        let base = p.base.clone();
        if self.settings.proxy_url.is_empty() {
            self.page = 2;
            self.setting_selected = 0;
            self.note("Set a proxy URL first, then press p on an AUR package");
            return;
        }
        let mut settings = self.settings.clone();
        let enabled = if settings.proxy_bases.remove(&base) {
            false
        } else {
            settings.proxy_bases.insert(base.clone());
            true
        };
        match settings.save() {
            Ok(()) => {
                self.settings = settings;
                self.note(format!(
                    "{base}: proxy {} · saved for future operations",
                    if enabled { "ON" } else { "OFF" }
                ));
            }
            Err(e) => self.note(e.to_string()),
        }
    }
    fn open_aur(&mut self) {
        let Some(url) = self
            .current()
            .and_then(|p| p.remote.as_ref())
            .and_then(|r| r.aur_url.clone())
        else {
            return;
        };
        self.open_web_url(url, "Cannot open AUR page: ");
    }
    fn open_web_url(&self, url: String, error_prefix: &'static str) {
        let sender = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let url = url::Url::parse(&url)?;
                anyhow::ensure!(
                    matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
                    "Invalid web URL"
                );
                let status = std::process::Command::new("xdg-open")
                    .arg(url.as_str())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()?;
                anyhow::ensure!(status.success(), "Browser could not open the page");
                Ok(())
            })();
            if let Err(e) = result {
                let _ = sender.send(Event::Warning(format!("{error_prefix}{e}")));
            }
        });
    }
    fn pkgbuild_click(&mut self, position: (u16, u16)) {
        let Some(view) = self.pkgbuild_view.as_mut() else {
            return;
        };
        if self.pkgbuild_panels[0].contains(position.into()) {
            view.focus = 0;
            return;
        }
        if !self.pkgbuild_panels[1].contains(position.into()) {
            return;
        }
        view.focus = 1;
        if let Some(url) = self.pkgbuild_link_at(position).map(str::to_owned) {
            self.open_web_url(url, "Cannot open comment link: ");
        }
    }
    fn pkgbuild_link_at(&self, position: (u16, u16)) -> Option<&str> {
        if !self.pkgbuild_comment_inner.contains(position.into()) {
            return None;
        }
        let view = self.pkgbuild_view.as_ref()?;
        let row = view.scroll[1] as usize
            + position.1.saturating_sub(self.pkgbuild_comment_inner.y) as usize;
        let col = position.0.saturating_sub(self.pkgbuild_comment_inner.x);
        self.pkgbuild_comment_cache
            .as_ref()?
            .links
            .iter()
            .find(|hit| hit.row == row && hit.start <= col && col < hit.end)
            .map(|hit| hit.url.as_str())
    }
    fn view_pkgbuild(&mut self) {
        let Some(package) = self.current() else {
            return;
        };
        let Some((name, base)) = pkgbuild_target(self.page, package) else {
            self.note("PKGBUILD is available for AUR packages only");
            return;
        };
        let id = self.pkgbuild_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.pkgbuild_view = Some(PkgbuildView {
            id,
            base: base.clone().unwrap_or_else(|| name.clone()),
            pkgbuild: None,
            pkgbuild_error: None,
            comments: vec![],
            comments_loading: false,
            comments_error: None,
            focus: 0,
            scroll: [0, 0],
            scroll_max: [0, 0],
        });
        self.pkgbuild_comment_cache = None;
        super::search::view_pkgbuild(id, name, base, self.tx.clone());
    }
    fn pkgbuild_key(&mut self, key: KeyEvent) {
        let view = self.pkgbuild_view.as_mut().unwrap();
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.pkgbuild_generation.fetch_add(1, Ordering::Relaxed);
                self.pkgbuild_view = None;
                self.pkgbuild_comment_cache = None;
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                view.focus = 1 - view.focus;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.scroll[view.focus] = view.scroll[view.focus].saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.scroll[view.focus] = view.scroll[view.focus]
                    .saturating_add(1)
                    .min(view.scroll_max[view.focus]);
            }
            KeyCode::PageUp => {
                view.scroll[view.focus] = view.scroll[view.focus].saturating_sub(8);
            }
            KeyCode::PageDown => {
                view.scroll[view.focus] = view.scroll[view.focus]
                    .saturating_add(8)
                    .min(view.scroll_max[view.focus]);
            }
            KeyCode::Home => view.scroll[view.focus] = 0,
            KeyCode::End => view.scroll[view.focus] = view.scroll_max[view.focus],
            _ => {}
        }
    }
    fn dialog_key(&mut self, key: KeyEvent) -> Result<()> {
        let dialog = self.dialog.as_mut().unwrap();
        let readonly = dialog.question.is_none() && dialog.action.is_none();
        if readonly {
            match key.code {
                KeyCode::Up => dialog.scroll = dialog.scroll.saturating_sub(1),
                KeyCode::Down => dialog.scroll = dialog.scroll.saturating_add(1),
                KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(8),
                KeyCode::PageDown => dialog.scroll = dialog.scroll.saturating_add(8),
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.dialog = None,
                _ => {}
            }
            return Ok(());
        }
        let is_text = dialog
            .question
            .as_ref()
            .is_some_and(|q| q.default.is_none());
        if key.code == KeyCode::PageDown {
            dialog.scroll = dialog.scroll.saturating_add(8).min(self.dialog_scroll_max);
            return Ok(());
        }
        if key.code == KeyCode::PageUp {
            dialog.scroll = dialog.scroll.saturating_sub(8);
            return Ok(());
        }
        match key.code {
            KeyCode::Esc => {
                if dialog.question.is_some() {
                    if let Some(s) = &mut self.session {
                        s.answer(String::new(), true)?;
                    }
                }
                self.dialog = None;
            }
            KeyCode::Enter => {
                let dialog = self.dialog.take().unwrap();
                if dialog.question.is_some() {
                    if let Some(s) = &mut self.session {
                        s.answer(
                            if is_text {
                                dialog.input
                            } else if dialog.yes {
                                "yes".into()
                            } else {
                                "no".into()
                            },
                            false,
                        )?;
                    }
                } else if dialog.yes {
                    match Session::start(dialog.action.unwrap(), &self.settings) {
                        Ok(s) => {
                            self.session = Some(s);
                            self.reported = false;
                        }
                        Err(e) => self.note(format!("Cannot start: {e:#}")),
                    }
                }
            }
            KeyCode::Char('u') if is_text && key.modifiers.contains(KeyModifiers::CONTROL) => {
                dialog.input.clear()
            }
            KeyCode::Char(c) if is_text && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                dialog.input.push(c)
            }
            KeyCode::Backspace if is_text => {
                dialog.input.pop();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab if !is_text => dialog.yes = !dialog.yes,
            KeyCode::Char('y') if !is_text => dialog.yes = true,
            KeyCode::Char('n') if !is_text => dialog.yes = false,
            _ => {}
        }
        Ok(())
    }
    fn mouse_scroll_at(&mut self, kind: MouseEventKind, position: Option<(u16, u16)>) {
        let delta: i16 = match kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        if let Some(view) = &mut self.pkgbuild_view {
            if let Some(position) = position {
                if self.pkgbuild_panels[0].contains(position.into()) {
                    view.focus = 0;
                } else if self.pkgbuild_panels[1].contains(position.into()) {
                    view.focus = 1;
                }
            }
            view.scroll[view.focus] = view.scroll[view.focus]
                .saturating_add_signed(delta)
                .min(view.scroll_max[view.focus]);
        } else if let Some(dialog) = &mut self.dialog {
            if dialog.question.is_none() && dialog.action.is_none() {
                dialog.scroll = dialog.scroll.saturating_add_signed(delta);
            }
        } else if self.help_open {
            self.help_scroll = self.help_scroll.saturating_add_signed(delta);
        } else if self.page == 3 {
            self.activity_scroll = self.activity_scroll.saturating_add_signed(delta);
        } else if matches!(self.page, 0 | 1 | 4) && self.focus == 1 {
            self.detail_scroll = self.detail_scroll.saturating_add_signed(delta);
        } else if matches!(self.page, 0 | 4) && self.focus == 2 {
            if let Some(session) = &mut self.session {
                let offset = session.parser.screen().scrollback();
                session
                    .parser
                    .set_scrollback(offset.saturating_add_signed((-delta).into()));
            }
        }
    }
    fn key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if self.pkgbuild_view.is_some() {
            self.pkgbuild_key(key);
            return Ok(false);
        }
        if self.removal.is_some() {
            self.removal_key(key);
            return Ok(false);
        }
        if self.dialog.is_some() {
            self.dialog_key(key)?;
            return Ok(false);
        }
        if self.help_open
            && !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.help_open = false,
                KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(8),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(8),
                _ => {}
            }
            return Ok(false);
        }
        if self.editing_proxy {
            match key.code {
                KeyCode::Esc => {
                    self.editing_proxy = false;
                    self.proxy_error = None;
                }
                KeyCode::Enter => {
                    let mut settings = self.settings.clone();
                    match settings
                        .set_proxy_input(&self.proxy_input)
                        .and_then(|()| settings.save())
                    {
                        Ok(()) => {
                            self.settings = settings;
                            self.editing_proxy = false;
                            self.proxy_error = None;
                            self.note(
                                "Settings saved · proxy address accepted · ↑↓ select a setting",
                            );
                        }
                        Err(e) => {
                            self.proxy_error = Some(e.to_string());
                            self.note(format!("Proxy not saved: {e}"));
                        }
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.proxy_input.clear();
                    self.proxy_error = None;
                }
                KeyCode::Backspace => {
                    self.proxy_input.pop();
                    self.proxy_error = None;
                }
                KeyCode::Char(c) => {
                    self.proxy_input.push(c);
                    self.proxy_error = None;
                }
                _ => {}
            }
            return Ok(false);
        }
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    if self.page == 4 {
                        self.install_query = self.submitted_query.clone();
                    }
                }
                KeyCode::Enter => {
                    self.searching = false;
                    if self.page == 4 {
                        self.search_packages();
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if self.page == 4 {
                        self.install_query.clear();
                    } else {
                        self.query.clear();
                        self.reindex();
                    }
                }
                KeyCode::Backspace => {
                    if self.page == 4 {
                        self.install_query.pop();
                    } else {
                        self.query.pop();
                        self.reindex();
                    }
                }
                KeyCode::Char(c) => {
                    if self.page == 4 {
                        self.install_query.push(c);
                    } else {
                        self.query.push(c);
                        self.reindex();
                    }
                }
                _ => {}
            }
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Down => self.detail_scroll = self.detail_scroll.saturating_add(1),
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Char('c') => {
                    if let Some(s) = &mut self.session {
                        if s.code.is_none() {
                            s.pty.send(b"\x03")?;
                            return Ok(false);
                        }
                    }
                    return Ok(true);
                }
                _ => {}
            }
            return Ok(false);
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            if self.page == 2 {
                self.setting_selected = (self.setting_selected + if reverse { 4 } else { 1 }) % 5;
                self.detail_scroll = 0;
            } else if self.page == 0 {
                let current = if self.focus == 0 {
                    self.source
                } else {
                    self.focus + 1
                };
                let next = (current + if reverse { 3 } else { 1 }) % 4;
                if next < 2 {
                    self.source = next;
                    self.focus = 0;
                } else {
                    self.focus = next - 1;
                }
                self.detail_scroll = 0;
            } else if self.page == 1 {
                self.focus = (self.focus + 1) % 2;
                self.detail_scroll = 0;
            } else if self.page == 4 {
                self.focus = (self.focus + if reverse { 2 } else { 1 }) % 3;
                self.detail_scroll = 0;
            }
            return Ok(false);
        }
        if self.page == 2 && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            self.detail_scroll = self
                .detail_scroll
                .saturating_add_signed(if key.code == KeyCode::PageDown { 8 } else { -8 });
            return Ok(false);
        }
        let delta = match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(-1isize),
            KeyCode::Down | KeyCode::Char('j') => Some(1),
            KeyCode::PageUp => Some(-10),
            KeyCode::PageDown => Some(10),
            _ => None,
        };
        if matches!(self.page, 0 | 4) && self.focus == 2 {
            if let Some(s) = &mut self.session {
                if let Some(delta) = delta {
                    let offset = s.parser.screen().scrollback();
                    s.parser
                        .set_scrollback(offset.saturating_add_signed(-delta));
                }
                if key.code == KeyCode::Home {
                    s.parser.set_scrollback(5000);
                }
                if key.code == KeyCode::End {
                    s.parser.set_scrollback(0);
                }
            }
            if delta.is_some()
                || matches!(
                    key.code,
                    KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Enter
                        | KeyCode::Char('u' | 'a' | 'p' | 'P')
                )
            {
                return Ok(false);
            }
        } else if matches!(self.page, 0 | 1 | 4) && self.focus == 1 {
            if let Some(delta) = delta {
                self.detail_scroll = self.detail_scroll.saturating_add_signed(delta as i16);
                return Ok(false);
            }
            if key.code == KeyCode::Home {
                self.detail_scroll = 0;
                return Ok(false);
            }
            if matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('u' | 'a' | 'p' | 'P')
            ) {
                return Ok(false);
            }
        } else if self.page == 3 {
            if let Some(delta) = delta {
                self.activity_scroll = self.activity_scroll.saturating_add_signed(delta as i16);
                return Ok(false);
            }
        }
        match key.code {
            KeyCode::Char('?') => {
                self.help_open = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('q') => {
                if self.active() {
                    self.note(
                        "Operation running · Ctrl+C interrupts it; q cannot abandon an update",
                    );
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char(c @ '1'..='5') => {
                self.page = [0, 4, 1, 2, 3][(c as u8 - b'1') as usize];
                self.detail_scroll = 0;
                self.focus = 0;
                self.searching = false;
                self.reindex();
            }
            KeyCode::Char('/') if matches!(self.page, 1 | 4) => {
                self.searching = true;
                self.focus = 0;
            }
            KeyCode::Esc => {
                self.focus = 0;
                if self.page == 1 {
                    self.query.clear();
                    self.reindex();
                }
            }
            KeyCode::Char('r') if self.page == 4 => self.search_packages(),
            KeyCode::Char('r') if !self.active() => self.refresh(),
            KeyCode::Char('o') if self.page == 4 => self.open_aur(),
            KeyCode::Char('v') if matches!(self.page, 1 | 4) && self.focus == 0 => {
                self.view_pkgbuild()
            }
            KeyCode::Char('p') if matches!(self.page, 0 | 1 | 4) => self.toggle_proxy(),
            KeyCode::Char('a') if self.page == 0 => self.update(true),
            KeyCode::Char('u') if self.page == 0 => self.update(false),
            KeyCode::Enter if self.page == 0 => self.update_one(),
            KeyCode::Char('t') if self.page == 1 && self.focus == 0 => {
                let package = self.filtered.get(self.package_selected).copied();
                self.tree_mode = !self.tree_mode;
                self.reindex();
                self.package_selected = package
                    .and_then(|p| self.filtered.iter().position(|&i| i == p))
                    .unwrap_or(0);
                self.detail_scroll = 0;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l' | ' ') | KeyCode::Enter
                if self.page == 1 && self.focus == 0 && self.tree_mode =>
            {
                self.tree_key(key.code)
            }
            KeyCode::Enter if self.page == 1 => self.focus = 1,
            KeyCode::Delete | KeyCode::Char('d') if self.page == 1 && self.focus == 0 => {
                self.remove()
            }
            KeyCode::Enter if self.page == 4 => self.install(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Enter | KeyCode::Char(' ') if self.page == 2 => {
                if self.setting_selected == 0 {
                    self.editing_proxy = true;
                    self.proxy_error = None;
                    self.proxy_input = self.settings.proxy_url.clone();
                } else if self.setting_selected == 4 {
                    if self.active() {
                        self.note("An operation is already running");
                    } else {
                        self.dialog = Some(Dialog {
                            title: "CLEAN PACKAGE CACHE".into(),
                            scroll: 0,
                            body: "Start paru -Scc? pacman will separately ask about removing cached packages and unused repository databases; paru will then ask about AUR clones and saved diffs.".into(),
                            question: None,
                            action: Some(vec!["-Scc".into()]),
                            yes: true,
                            input: String::new(),
                        });
                    }
                } else {
                    let mut settings = self.settings.clone();
                    if self.setting_selected == 1 {
                        settings.git_proxy = !settings.git_proxy;
                    } else if self.setting_selected == 2 {
                        settings.language = if settings.language == Language::En {
                            Language::ZhCn
                        } else {
                            Language::En
                        };
                    } else if self.setting_selected == 3 {
                        settings.accent = settings.accent.next();
                    }
                    match settings.save() {
                        Ok(()) => {
                            self.settings = settings;
                            self.note(match self.setting_selected {
                                1 => "Git proxy setting saved",
                                2 => "Language saved",
                                _ => "Theme color saved",
                            });
                        }
                        Err(e) => self.note(e.to_string()),
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }
    fn move_selection(&mut self, delta: isize) {
        self.last_navigation = Instant::now();
        let (selected, len) = if self.page == 0 {
            (
                &mut self.selected[self.source],
                self.updates[self.source].len(),
            )
        } else if self.page == 1 {
            (&mut self.package_selected, self.filtered.len())
        } else if self.page == 4 {
            (&mut self.search_selected, self.search_results.len())
        } else if self.page == 2 {
            (&mut self.setting_selected, 5)
        } else {
            return;
        };
        *selected = selected
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        self.detail_scroll = 0;
    }
    fn draw(&mut self, f: &mut Frame) {
        super::theme::set_accent(self.settings.accent);
        self.selection_layer.borrow_mut().begin_frame();
        self.tab_layer.borrow_mut().begin_frame();
        self.raster_lists.borrow_mut().begin_frame();
        self.motion.borrow_mut().prune(Instant::now());
        let area = f.area();
        if area.width < 62 || area.height < 18 {
            f.render_widget(self.paragraph("paru-tui\nPlease resize to at least 62 × 18.\nq exits when idle; updates show package build output."),area);
            return;
        }
        let outer = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .margin(1)
        .split(area);
        self.draw_header(f, outer[0]);
        match self.page {
            0 => {
                let rows =
                    Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                        .split(outer[1]);
                self.draw_updates(f, rows[0]);
                self.draw_build(f, rows[1]);
            }
            1 => {
                let rows =
                    Layout::vertical([Constraint::Min(4), Constraint::Length(3)]).split(outer[1]);
                self.draw_packages(f, rows[0]);
                f.render_widget(
                    self.paragraph(format!(
                        " / {}{}",
                        self.query,
                        if self.searching { "▏" } else { "" }
                    ))
                    .block(self.panel(" SEARCH ".into(), self.searching)),
                    rows[1],
                );
            }
            2 => self.draw_settings(f, outer[1]),
            3 => self.draw_activity(f, outer[1]),
            _ => self.draw_install(f, outer[1]),
        }
        let zh = self.settings.language == Language::ZhCn;
        let hints: Vec<(&str, &str)> = match self.page {
            0 | 4 if self.focus == 2 => vec![
                ("Tab", if zh { "切换" } else { "focus" }),
                ("↑↓", if zh { "滚动" } else { "scroll" }),
                ("End", if zh { "实时" } else { "live" }),
            ],
            4 if self.focus == 1 => vec![
                ("Tab", if zh { "切换" } else { "focus" }),
                ("↑↓", if zh { "详情" } else { "details" }),
                ("o", if zh { "AUR 网页" } else { "AUR page" }),
            ],
            0 | 1 if self.focus == 1 => vec![
                ("Tab", if zh { "切换" } else { "focus" }),
                ("↑↓", if zh { "详情" } else { "details" }),
            ],
            0 => vec![
                ("Tab", if zh { "切换" } else { "focus" }),
                ("Enter", if zh { "单包" } else { "package" }),
                ("u", if zh { "当前源" } else { "source" }),
                ("a", if zh { "全部" } else { "all" }),
            ],
            1 if self.tree_mode => vec![
                ("Tab", if zh { "详情" } else { "details" }),
                ("←→", if zh { "收起/展开" } else { "fold/open" }),
                ("/", if zh { "搜索" } else { "search" }),
                ("t", if zh { "平铺" } else { "flat" }),
                ("d", if zh { "删除" } else { "remove" }),
                ("v", "PKGBUILD"),
            ],
            1 => vec![
                ("t", if zh { "依赖树" } else { "tree" }),
                ("Tab", if zh { "切换" } else { "focus" }),
                ("/", if zh { "搜索" } else { "search" }),
                ("Enter", if zh { "详情" } else { "details" }),
                ("d/Del", if zh { "删除" } else { "remove" }),
                ("v", "PKGBUILD"),
            ],
            4 if self.searching => vec![
                ("Enter", if zh { "搜索" } else { "search" }),
                ("Esc", if zh { "返回" } else { "back" }),
            ],
            4 => vec![
                ("/", if zh { "搜索" } else { "search" }),
                ("Enter", if zh { "安装" } else { "install" }),
                ("Tab", if zh { "切换" } else { "focus" }),
                ("v", "PKGBUILD"),
                ("p", if zh { "代理" } else { "proxy" }),
                ("o", if zh { "AUR 网页" } else { "AUR page" }),
            ],
            2 if self.editing_proxy => vec![
                ("Enter", if zh { "保存" } else { "save" }),
                ("Esc", if zh { "取消" } else { "cancel" }),
                ("Ctrl+U", if zh { "清空" } else { "clear" }),
            ],
            2 => vec![
                ("↑↓", if zh { "选择" } else { "select" }),
                ("Enter", if zh { "编辑/切换" } else { "edit/toggle" }),
                ("PgUp/PgDn", if zh { "详情" } else { "details" }),
            ],
            _ => vec![
                ("↑↓", if zh { "滚动" } else { "scroll" }),
                ("q", if zh { "退出" } else { "quit" }),
            ],
        };
        let mut spans = vec![];
        for (key, description) in hints {
            spans.push(Span::styled(format!(" {key} "), style(accent())));
            spans.push(Span::styled(format!("{description}  "), style(DIM)));
        }
        if !self.editing_proxy && !self.searching {
            spans.push(Span::styled(" ? ", style(accent())));
            spans.push(Span::styled(if zh { "帮助" } else { "help" }, style(DIM)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), outer[2]);
        if self.help_open
            || self.dialog.is_some()
            || self.removal.is_some()
            || self.pkgbuild_view.is_some()
        {
            self.selection_layer.borrow_mut().begin_frame();
            self.tab_layer.borrow_mut().begin_frame();
            self.raster_lists.borrow_mut().dim();
            dim_backdrop(f);
        }
        if self.pkgbuild_view.is_some() {
            self.draw_pkgbuild_view(f, area);
            return;
        }
        self.draw_toast(f, area);
        if self.help_open {
            self.draw_help(f, area);
        }
        if let Some(removal) = &self.removal {
            self.draw_removal(f, area, removal);
        }
        if let Some(dialog) = &self.dialog {
            if dialog.question.as_ref().is_some_and(|q| q.secret) {
                self.draw_auth(f, area, dialog);
                return;
            }
            let is_text = dialog
                .question
                .as_ref()
                .is_some_and(|q| q.default.is_none());
            let readonly = dialog.question.is_none() && dialog.action.is_none();
            let zh = self.settings.language == Language::ZhCn;
            let choice = if readonly {
                Line::default()
            } else if is_text {
                Line::from(Span::styled(format!("> {}▏", dialog.input), style(TEXT)))
            } else {
                confirmation_choice(dialog.yes, self.settings.language)
            };
            let width = (area.width * 3 / 4).min(area.width.saturating_sub(2));
            let content_width = width.saturating_sub(6);
            let body = Paragraph::new(dialog_lines(dialog, self.settings.language))
                .wrap(Wrap { trim: false });
            let command = dialog.action.as_ref().map(|args| {
                Paragraph::new(shell_line(args))
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .style(Style::default().bg(COMMAND_BG))
                            .padding(Padding::new(2, 2, 1, 1)),
                    )
            });
            let command_height = command
                .as_ref()
                .map(|p| p.line_count(content_width).min(6) as u16)
                .unwrap_or(0);
            let footer = Paragraph::new(if readonly {
                vec![key_hints(&[
                    ("↑↓", if zh { "滚动" } else { "scroll" }),
                    ("PgUp/PgDn", if zh { "翻页" } else { "page" }),
                    ("Esc", if zh { "关闭" } else { "close" }),
                ])]
            } else {
                vec![
                    choice,
                    key_hints(&[
                        ("Enter", if zh { "确认" } else { "confirm" }),
                        ("Esc", if zh { "取消" } else { "cancel" }),
                        ("PgUp/PgDn", if zh { "查看计划" } else { "read plan" }),
                    ]),
                ]
            })
            .wrap(Wrap { trim: false });
            let footer_height = footer.line_count(content_width).min(u16::MAX as usize) as u16;
            let command_gap = u16::from(command.is_some());
            let desired = body.line_count(content_width).saturating_add(
                footer_height as usize + command_height as usize + command_gap as usize + 5,
            );
            let height = desired.min(area.height.saturating_sub(4) as usize) as u16;
            let rect = Rect::new(
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
                width,
                height,
            );
            self.raster_lists.borrow_mut().occlude(rect);
            f.render_widget(Clear, rect);
            let block = self
                .content_panel(format!(" {} ", dialog.title), true)
                .padding(Padding::new(2, 2, 1, 1));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let parts = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(command_height),
                Constraint::Length(command_gap),
                Constraint::Length(footer_height),
            ])
            .split(inner);
            let total = body.line_count(content_width);
            self.dialog_scroll_max = total
                .saturating_sub(parts[0].height as usize)
                .min(u16::MAX as usize) as u16;
            let offset = dialog.scroll.min(self.dialog_scroll_max);
            f.render_widget(body.scroll((offset, 0)), parts[0]);
            draw_scroll_position(
                f,
                rect,
                offset as usize,
                parts[0].height as usize,
                total,
                None,
                true,
            );
            if let Some(command) = command {
                f.render_widget(command, parts[2]);
            }
            f.render_widget(footer, parts[4]);
        }
    }
    fn draw_pkgbuild_view(&mut self, f: &mut Frame, area: Rect) {
        let zh = self.settings.language == Language::ZhCn;
        let width = area.width.saturating_sub(4);
        let height = area.height.saturating_sub(4);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        self.raster_lists.borrow_mut().occlude(rect);
        f.render_widget(Clear, rect);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(rect);
        let cols = Layout::horizontal([
            Constraint::Percentage(54),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(rows[0]);
        self.pkgbuild_panels = [cols[0], cols[2]];
        let view = self.pkgbuild_view.as_ref().unwrap();
        let left = self
            .content_panel(format!(" [PKGBUILD] {} ", view.base), view.focus == 0)
            .padding(Padding::new(1, 1, 1, 1));
        let right_title = format!(
            " {} ({}) ",
            if zh { "AUR 评论" } else { "AUR COMMENTS" },
            view.comments.len(),
        );
        let right = self
            .content_panel(right_title, view.focus == 1)
            .padding(Padding::new(1, 1, 1, 1));
        let left_inner = left.inner(cols[0]);
        let right_inner = right.inner(cols[2]);
        self.pkgbuild_comment_inner = right_inner;
        let pkgbuild_lines = if let Some(text) = &view.pkgbuild {
            super::syntax::pkgbuild(text)
        } else if let Some(error) = &view.pkgbuild_error {
            vec![Line::from(Span::styled(
                super::i18n::translate(error, self.settings.language),
                style(RED),
            ))]
        } else {
            vec![Line::from(Span::styled(
                if zh {
                    "正在读取 PKGBUILD…"
                } else {
                    "Loading PKGBUILD…"
                },
                style(DIM),
            ))]
        };
        if self.pkgbuild_comment_cache.as_ref().is_none_or(|cache| {
            cache.id != view.id
                || cache.width != right_inner.width
                || cache.language != self.settings.language
        }) {
            let mut comments = super::comments::Layout::new();
            comments.append(&view.comments, right_inner.width, self.settings.language);
            if view.comments_loading {
                if !comments.lines.is_empty() {
                    comments.lines.push(Line::default());
                }
                comments.lines.push(Line::from(Span::styled(
                    if zh {
                        "正在加载更多评论…"
                    } else {
                        "Loading more comments…"
                    },
                    style(DIM),
                )));
            } else if let Some(error) = &view.comments_error {
                if !comments.lines.is_empty() {
                    comments.lines.push(Line::default());
                }
                comments.lines.push(Line::from(Span::styled(
                    super::i18n::translate(error, self.settings.language),
                    style(RED),
                )));
            } else if comments.lines.is_empty() {
                comments.lines.push(Line::from(Span::styled(
                    if view.pkgbuild.is_some() {
                        if zh {
                            "暂无评论。"
                        } else {
                            "No comments yet."
                        }
                    } else if view.pkgbuild_error.is_some() {
                        if zh {
                            "评论未加载。"
                        } else {
                            "Comments were not loaded."
                        }
                    } else if zh {
                        "等待 PKGBUILD…"
                    } else {
                        "Waiting for PKGBUILD…"
                    },
                    style(DIM),
                )));
            }
            self.pkgbuild_comment_cache = Some(CommentCache {
                id: view.id,
                width: right_inner.width,
                language: self.settings.language,
                lines: comments.lines,
                links: comments.links,
            });
        }
        let comment_cache = self.pkgbuild_comment_cache.as_ref().unwrap();
        let left_paragraph = Paragraph::new(pkgbuild_lines).wrap(Wrap { trim: false });
        let left_total = left_paragraph.line_count(left_inner.width);
        let right_total = comment_cache.lines.len();
        let max = [
            left_total
                .saturating_sub(left_inner.height as usize)
                .min(u16::MAX as usize) as u16,
            right_total
                .saturating_sub(right_inner.height as usize)
                .min(u16::MAX as usize) as u16,
        ];
        let scroll = [view.scroll[0].min(max[0]), view.scroll[1].min(max[1])];
        f.render_widget(left_paragraph.scroll((scroll[0], 0)).block(left), cols[0]);
        let visible_comments = comment_cache
            .lines
            .iter()
            .skip(scroll[1] as usize)
            .take(right_inner.height as usize)
            .cloned()
            .collect::<Vec<_>>();
        f.render_widget(Paragraph::new(visible_comments).block(right), cols[2]);
        draw_scroll_position(
            f,
            cols[0],
            scroll[0] as usize,
            left_inner.height as usize,
            left_total,
            None,
            view.focus == 0,
        );
        draw_scroll_position(
            f,
            cols[2],
            scroll[1] as usize,
            right_inner.height as usize,
            right_total,
            None,
            view.focus == 1,
        );
        f.render_widget(
            Paragraph::new(key_hints(&[
                ("Tab / ←→", if zh { "切换栏" } else { "switch pane" }),
                ("↑↓ / 滚轮", if zh { "滚动" } else { "scroll" }),
                ("PgUp/PgDn", if zh { "翻页" } else { "page" }),
                ("Click", if zh { "打开链接" } else { "open link" }),
                ("Esc", if zh { "关闭" } else { "close" }),
            ])),
            rows[1],
        );
        if let Some(view) = self.pkgbuild_view.as_mut() {
            view.scroll_max = max;
            view.scroll = scroll;
        }
    }
    fn draw_removal(&self, f: &mut Frame, area: Rect, removal: &super::removal::Removal) {
        let zh = self.settings.language == Language::ZhCn;
        let width = area.width.saturating_sub(4).min(84);
        let description = Paragraph::new(
            super::removal::Removal::label(removal.cursor, self.settings.language).2,
        )
        .style(style(if matches!(removal.cursor, 1..=3) {
            RED
        } else {
            DIM
        }))
        .wrap(Wrap { trim: false });
        let footer = Paragraph::new(key_hints(&[
            ("↑↓", if zh { "选择" } else { "select" }),
            ("Space", if zh { "勾选" } else { "toggle" }),
            ("Enter", if zh { "下一步" } else { "next" }),
            ("Esc", if zh { "取消" } else { "cancel" }),
        ]))
        .wrap(Wrap { trim: false });
        let content_width = width.saturating_sub(6);
        let footer_height = footer.line_count(content_width) as u16;
        let height = (13 + description.line_count(content_width) as u16 + footer_height)
            .min(area.height.saturating_sub(2));
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        self.raster_lists.borrow_mut().occlude(rect);
        f.render_widget(Clear, rect);
        let block = self
            .content_panel(
                if zh {
                    " 删除方式 "
                } else {
                    " REMOVAL OPTIONS "
                }
                .into(),
                true,
            )
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let parts = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(footer_height),
        ])
        .split(inner);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    fit_text(
                        &clean(&removal.package),
                        content_width.saturating_sub(5) as usize,
                    ),
                    style(TEXT),
                ),
                Span::styled("  -R", style(accent())),
            ])),
            parts[0],
        );
        let rows = (0..5)
            .map(|i| {
                let (flag, label, _) = super::removal::Removal::label(i, self.settings.language);
                Line::from(vec![
                    Span::styled(
                        if removal.cursor == i { "▸ " } else { "  " },
                        style(accent()),
                    ),
                    Span::styled(
                        if removal.checked[i] { "[✓] " } else { "[ ] " },
                        style(if removal.checked[i] { GREEN } else { DIM }),
                    ),
                    Span::styled(format!("{flag:<11}"), style(accent())),
                    Span::styled(label, style(TEXT)),
                ])
                .style(Style::default().bg(if removal.cursor == i {
                    selection()
                } else {
                    Color::Reset
                }))
            })
            .collect::<Vec<_>>();
        f.render_widget(Paragraph::new(rows), parts[2]);
        f.render_widget(description, parts[4]);
        f.render_widget(footer, parts[6]);
    }
    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(style(BORDER));
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ◆ ", style(accent())),
                Span::styled("PARU", style(TEXT).add_modifier(Modifier::BOLD)),
                Span::styled("  │", style(BORDER)),
            ])),
            Rect::new(inner.x, inner.y, 10, 1),
        );
        let tabs = Rect::new(inner.x + 11, inner.y, inner.width.saturating_sub(11), 1);
        let step = tabs.width / 5;
        let width = step.saturating_sub(1).min(18);
        let inset = (step - width) / 2;
        let tab_index = [0, 4, 1, 2, 3]
            .iter()
            .position(|p| *p == self.page)
            .unwrap_or(0);
        let target = (tab_index as u16 * step + inset) as f32;
        let position = self
            .motion
            .borrow_mut()
            .position("header-pill", target, Instant::now());
        if self.tab_layer.borrow().enabled() {
            self.tab_layer.borrow_mut().place_tab(tabs, position, width);
        } else {
            f.render_widget(
                Block::default().style(Style::default().bg(selection())),
                Rect::new(tabs.x + position.round() as u16, tabs.y, width, 1),
            );
        }
        let labels = if self.settings.language == Language::ZhCn {
            ["更新", "安装", "列表", "设置", "活动"]
        } else {
            if step < 12 {
                ["Update", "Install", "List", "Prefs", "Log"]
            } else {
                ["Updates", "Install", "List", "Settings", "Activity"]
            }
        };
        for (i, label) in labels.iter().enumerate() {
            let text = Line::from(vec![
                Span::styled(format!("{} ", i + 1), style(accent())),
                Span::styled(*label, style(if tab_index == i { TEXT } else { DIM })),
            ]);
            f.render_widget(
                Paragraph::new(text).alignment(ratatui::layout::Alignment::Center),
                Rect::new(tabs.x + i as u16 * step + inset, tabs.y, width, 1),
            );
        }
    }
    fn draw_updates(&mut self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([
            Constraint::Percentage(32),
            Constraint::Percentage(32),
            Constraint::Percentage(36),
        ])
        .split(area);
        for (source, title) in [" PACMAN / REPOSITORIES ", " AUR / COMMUNITY "]
            .iter()
            .enumerate()
        {
            self.draw_list(
                f,
                cols[source],
                title,
                &self.catalog.installed,
                &self.updates[source],
                self.selected[source],
                self.source == source && self.focus == 0,
            );
        }
        self.draw_detail(f, cols[2]);
    }
    fn draw_packages(&mut self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(area);
        self.draw_list(
            f,
            cols[0],
            if self.tree_mode {
                match (
                    self.settings.language == Language::ZhCn,
                    self.query.is_empty(),
                ) {
                    (true, true) => " 依赖树 ",
                    (true, false) => " 依赖树 · 匹配的软件包 ",
                    (false, true) => " DEPENDENCY TREE ",
                    (false, false) => " TREE · MATCHED PACKAGES ",
                }
            } else {
                " INSTALLED "
            },
            self.packages(),
            &self.filtered,
            self.package_selected,
            self.focus == 0 && !self.searching,
        );
        self.draw_detail(f, cols[1]);
    }
    fn draw_install(&mut self, f: &mut Frame, area: Rect) {
        let regions = Layout::vertical([Constraint::Length(3), Constraint::Min(6)]).split(area);
        let title = if self.search_pending {
            " SEARCH · AUR… "
        } else {
            " SEARCH PACKAGES "
        };
        f.render_widget(
            self.paragraph(format!(
                " / {}{}",
                self.install_query,
                if self.searching { "▏" } else { "" }
            ))
            .block(self.panel(title.into(), self.searching)),
            regions[0],
        );
        let rows = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(regions[1]);
        let cols = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[0]);
        self.draw_list(
            f,
            cols[0],
            " SEARCH RESULTS ",
            &self.search_results,
            &self.search_indices,
            self.search_selected,
            self.focus == 0 && !self.searching,
        );
        self.draw_detail(f, cols[1]);
        self.draw_build(f, rows[1]);
    }
    #[allow(clippy::too_many_arguments)] // A borrowed list view; only visible rows are rendered.
    fn draw_list(
        &self,
        f: &mut Frame,
        area: Rect,
        title: &str,
        packages: &[Package],
        indices: &[usize],
        selected: usize,
        active: bool,
    ) {
        let block = self.panel(format!("{title} {} ", indices.len()), active);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if indices.is_empty() {
            f.render_widget(
                self.paragraph(if self.page == 4 && self.submitted_query.is_empty() {
                    "Press / to search by name or description"
                } else if self.page == 4 && self.search_pending {
                    "Searching packages…"
                } else if self.loading {
                    " Scanning…"
                } else {
                    " No matching packages"
                })
                .style(style(DIM)),
                inner,
            );
            return;
        }
        let query = if self.page == 1 {
            self.query.as_str()
        } else if self.page == 4 {
            self.submitted_query.as_str()
        } else {
            ""
        };
        let width = inner.width as usize;
        let updates = self.page == 0;
        let show_old = updates && width >= 36;
        let version_width = if show_old {
            (width / 4).clamp(9, 22)
        } else {
            (width / 3).clamp(6, 18)
        };
        let tree = self.page == 1 && self.tree_mode;
        let source_width = if !updates && !tree && width >= 36 {
            9
        } else {
            0
        };
        let prefix_width = if updates || self.page == 4 { 4 } else { 2 };
        let mut name_width = width.saturating_sub(
            prefix_width
                + 1
                + version_width
                + if show_old {
                    version_width + 3
                } else {
                    source_width
                },
        );
        let zh = self.settings.language == Language::ZhCn;
        let mut heading = format!(
            "{}{} {}{}",
            " ".repeat(prefix_width),
            fit_text(if zh { "软件包" } else { "PACKAGE" }, name_width),
            if show_old {
                format!(
                    "{}   ",
                    fit_text(if zh { "当前" } else { "CURRENT" }, version_width)
                )
            } else {
                String::new()
            },
            fit_text(
                if updates {
                    if zh {
                        "新版本"
                    } else {
                        "NEW"
                    }
                } else if zh {
                    "版本"
                } else {
                    "VERSION"
                },
                version_width
            )
        );
        if source_width > 0 {
            heading.push_str(&fit_text(
                if zh { " 来源" } else { " SOURCE" },
                source_width,
            ));
        }
        let install_source_width = if width >= 38 { 10 } else { 6 };
        let date_width = if width >= 38 { 10 } else { 5 };
        let install_name_width = width.saturating_sub(6 + install_source_width + date_width);
        let badge_width = (if zh { 8 } else { 13 }).min(install_name_width / 2);
        if self.page == 4 {
            name_width = install_name_width.saturating_sub(badge_width + 2);
            heading = format!(
                "    {} {} {}",
                fit_text(
                    if zh { "软件包" } else { "PACKAGE" },
                    install_name_width.saturating_sub(2)
                ),
                fit_text(if zh { "来源" } else { "SOURCE" }, install_source_width),
                fit_text(if zh { "更新日期" } else { "UPDATED" }, date_width)
            );
        }
        let rows = inner.height.saturating_sub(1) as usize;
        let view =
            self.motion
                .borrow_mut()
                .list(title, selected, indices.len(), rows, Instant::now());
        let raster = self.raster_lists.borrow().enabled();
        let start = if raster {
            view.scroll.floor() as usize
        } else {
            view.start
        };
        let pixel_selection = self.selection_layer.borrow().enabled();
        if active && pixel_selection {
            self.selection_layer.borrow_mut().place(
                Rect::new(inner.x, inner.y + 1, inner.width, rows as u16),
                view.cursor - if raster { view.scroll } else { start as f32 },
            );
        }
        let mut lines = vec![Line::from(Span::styled(heading, style(DIM)))];
        for (n, i) in indices
            .iter()
            .skip(start)
            .take(rows + usize::from(raster))
            .enumerate()
        {
            let p = &packages[*i];
            let installed = self.page == 4 && p.installed.is_some();
            let badges = if self.page == 4 {
                package_badges(p, self.settings.language)
            } else {
                String::new()
            };
            let selected = n + start == selected;
            let background =
                if active && !pixel_selection && n + start == view.cursor.round() as usize {
                    selection()
                } else {
                    Color::Reset
                };
            let proxy = self.page == 0 && p.is_aur() && self.settings.proxy_bases.contains(&p.base);
            let mut spans = vec![Span::styled(
                if (!active && selected)
                    || (active && !pixel_selection && n + start == view.cursor.round() as usize)
                {
                    "▸ "
                } else {
                    "  "
                },
                style(if active { accent() } else { DIM }),
            )];
            if updates {
                spans.push(Span::styled(
                    if proxy {
                        "P "
                    } else if p.ignored {
                        "! "
                    } else {
                        "  "
                    },
                    style(YELLOW),
                ));
            }
            if self.page == 4 {
                spans.push(Span::styled(
                    if installed { "✓ " } else { "  " },
                    style(GREEN),
                ));
            }
            let branch = if tree {
                self.tree.rows[n + start].prefix(name_width.saturating_sub(10))
            } else {
                String::new()
            };
            let branch_width = unicode_width::UnicodeWidthStr::width(branch.as_str());
            if tree {
                spans.push(Span::styled(branch, style(DIM)));
            }
            spans.extend(
                highlight(
                    &fit_text(&clean(&p.name), name_width.saturating_sub(branch_width)),
                    query,
                    style(if installed {
                        GREEN
                    } else if !badges.is_empty() {
                        RED
                    } else if active && !raster && n + start == view.cursor.round() as usize {
                        accent()
                    } else {
                        TEXT
                    }),
                )
                .spans,
            );
            if self.page == 4 {
                spans.push(Span::styled(fit_text(&badges, badge_width), style(RED)));
                let date = p
                    .remote
                    .as_ref()
                    .and_then(|r| r.updated)
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format(if date_width == 10 {
                                "%Y-%m-%d"
                            } else {
                                "%m-%d"
                            })
                            .to_string()
                    })
                    .unwrap_or_else(|| "—".into());
                spans.push(Span::styled(
                    format!(
                        " {} {}",
                        fit_text(&p.source, install_source_width),
                        fit_text(&date, date_width)
                    ),
                    style(DIM),
                ));
            } else {
                spans.push(Span::raw(" "));
                if show_old {
                    spans.extend(
                        highlight(
                            &fit_text(&clean(&p.version), version_width),
                            query,
                            style(DIM),
                        )
                        .spans,
                    );
                    spans.push(Span::styled(" → ", style(DIM)));
                }
                spans.extend(
                    highlight(
                        &fit_text(
                            &clean(if updates {
                                p.next.as_deref().unwrap_or(&p.version)
                            } else {
                                &p.version
                            }),
                            version_width,
                        ),
                        query,
                        style(if updates { GREEN } else { DIM }),
                    )
                    .spans,
                );
                if source_width > 0 {
                    spans.push(Span::styled(
                        fit_text(&format!(" {}", clean(&p.source)), source_width),
                        style(DIM),
                    ));
                }
            }
            lines.push(Line::from(spans).style(Style::default().bg(background)));
        }
        if raster {
            let body = lines.split_off(1);
            self.raster_lists.borrow_mut().place(
                usize::from(title.contains("AUR")),
                Rect::new(inner.x, inner.y + 1, inner.width, rows as u16),
                body,
                view.scroll.fract(),
            );
            if active {
                self.raster_lists.borrow_mut().highlight(
                    usize::from(title.contains("AUR")),
                    view.cursor - view.scroll,
                    prefix_width as u16,
                    name_width as u16,
                );
            }
        }
        f.render_widget(Paragraph::new(lines), inner);
        draw_scroll_position(f, area, start, rows, indices.len(), Some(selected), active);
    }
    fn draw_detail(&mut self, f: &mut Frame, area: Rect) {
        let block = self.content_panel(" INSPECTOR ".into(), self.focus == 1);
        let mut inner = block.inner(area);
        f.render_widget(block, area);
        if self.page == 4 {
            if let Some(remote) = self.current().and_then(|p| p.remote.as_ref()) {
                let mut warnings = Vec::new();
                let zh = self.settings.language == Language::ZhCn;
                if remote.orphaned {
                    warnings.push(if zh {
                        "无人维护（孤儿包）"
                    } else {
                        "Unmaintained (orphaned)"
                    });
                }
                if remote.out_of_date.is_some() {
                    warnings.push(if zh {
                        "已标记过期"
                    } else {
                        "Flagged out-of-date"
                    });
                }
                if !warnings.is_empty() {
                    let warning = Paragraph::new(warnings.join("\n"))
                        .style(style(RED))
                        .wrap(Wrap { trim: false });
                    let height = (warning.line_count(inner.width.saturating_sub(4)) as u16 + 2)
                        .min(inner.height.saturating_sub(1));
                    f.render_widget(
                        warning.block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(style(RED))
                                .padding(Padding::horizontal(1)),
                        ),
                        Rect::new(inner.x, inner.y, inner.width, height),
                    );
                    inner.y += height;
                    inner.height = inner.height.saturating_sub(height);
                }
            }
        }
        let field = |label: &str, value: &str, query: &str, language: Language| {
            detail_field(label, value, query, language, inner.width)
        };
        let mut lines = vec![];
        let query = if self.page == 1 {
            self.query.as_str()
        } else if self.page == 4 {
            self.submitted_query.as_str()
        } else {
            ""
        };
        let language = self.settings.language;
        let section = |name: &str| {
            vec![
                Line::default(),
                Line::from(Span::styled(
                    super::i18n::translate(name, language),
                    style(accent()),
                )),
            ]
        };
        if let Some(p) = self.current() {
            lines.push(highlight(
                &clean(&p.name),
                query,
                style(accent()).add_modifier(Modifier::BOLD),
            ));
            lines.push(highlight(&clean(&p.description), query, style(TEXT)));
            if let Some(url) = p.remote.as_ref().and_then(|r| r.aur_url.as_ref()) {
                lines.extend(field("AUR", url, query, language));
            }
            lines.extend(section("VERSION & SOURCE"));
            for (label, value) in [
                ("Version", p.version.as_str()),
                ("Source", p.source.as_str()),
                ("Package base", p.base.as_str()),
            ] {
                lines.extend(field(label, value, query, language));
            }
            if let Some(next) = &p.next {
                let mut available = field("Available", next, query, language);
                for line in &mut available {
                    for span in line.spans.iter_mut().skip(1) {
                        span.style = span.style.fg(GREEN);
                    }
                }
                lines.extend(available);
            }
            let fields = self
                .inspections
                .get(&self.inspection_key(p))
                .or_else(|| p.remote.as_ref().map(|r| &r.fields));
            let groups: &[(&str, &[&str])] = &[
                (
                    "AUR METADATA",
                    &[
                        "Maintainer",
                        "Last modified",
                        "First submitted",
                        "Votes",
                        "Popularity",
                    ],
                ),
                (
                    "INSTALLATION",
                    &[
                        "Architecture",
                        "Install date",
                        "Install reason",
                        "Groups",
                        "Date source",
                        "Download size",
                    ],
                ),
                (
                    "DEPENDENCIES",
                    &[
                        "Required by",
                        "Optional dependencies",
                        "Build dependencies",
                        "Check dependencies",
                        "Optional for",
                        "Provides",
                        "Conflicts with",
                        "Replaces",
                    ],
                ),
                (
                    "BUILD & LINKS",
                    &[
                        "Licenses",
                        "Packager",
                        "Build date",
                        "Validated by",
                        "Install script",
                        "Backup files",
                    ],
                ),
            ];
            for (group, labels) in groups {
                if *group == "AUR METADATA" && !(self.page == 4 && p.is_aur()) {
                    continue;
                }
                lines.extend(section(if *group == "INSTALLATION" && self.page == 4 {
                    "PACKAGE INFO"
                } else {
                    group
                }));
                if *group == "INSTALLATION" {
                    lines.extend(field(
                        "Installed",
                        p.installed.as_deref().unwrap_or("Not installed"),
                        query,
                        language,
                    ));
                    if !p.is_aur() || self.page != 4 {
                        lines.extend(field(
                            "Disk size",
                            &format!("{:.1} MiB", p.size as f64 / 1048576.0),
                            query,
                            language,
                        ));
                    }
                } else if *group == "DEPENDENCIES" {
                    lines.extend(field(
                        "Depends on",
                        &fields
                            .and_then(|fields| {
                                fields
                                    .iter()
                                    .find(|(k, _)| k == "Depends on")
                                    .map(|(_, v)| v.clone())
                            })
                            .unwrap_or_else(|| {
                                if self.page == 4 && p.is_aur() {
                                    "Details not loaded".into()
                                } else {
                                    p.dependencies.join("  ")
                                }
                            }),
                        query,
                        language,
                    ));
                } else if *group == "BUILD & LINKS" {
                    lines.extend(field("URL", &p.url, query, language));
                    if p.is_aur() {
                        lines.extend(field(
                            "Network",
                            if self.settings.proxy_bases.contains(&p.base) {
                                "PROXY (package base)"
                            } else {
                                "DIRECT (Git follows settings)"
                            },
                            query,
                            language,
                        ));
                    }
                }
                if matches!(self.page, 1 | 4) {
                    if let Some(fields) = fields {
                        for label in *labels {
                            if let Some((_, value)) = fields.iter().find(|(name, _)| name == label)
                            {
                                lines.extend(field(label, value, query, language));
                            }
                        }
                    }
                }
            }
            if (self.page == 1 && fields.is_none())
                || (self.page == 4
                    && p.is_aur()
                    && self.detail_pending.as_deref() == Some(self.inspection_key(p).as_str()))
            {
                lines.push(Line::from(Span::styled(
                    super::i18n::translate("Loading package details…", language),
                    style(DIM),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                super::i18n::translate("Select a package to inspect.", language),
                style(DIM),
            )));
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = paragraph.line_count(inner.width);
        self.detail_scroll = self.detail_scroll.min(
            total
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(paragraph.scroll((self.detail_scroll, 0)), inner);
        draw_scroll_position(
            f,
            area,
            self.detail_scroll as usize,
            inner.height as usize,
            total,
            None,
            self.focus == 1,
        );
    }
    fn draw_auth(&self, f: &mut Frame, area: Rect, dialog: &Dialog) {
        let zh = self.settings.language == Language::ZhCn;
        let width = area.width.saturating_sub(6).min(62);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - 10) / 2,
            width,
            10,
        );
        self.raster_lists.borrow_mut().occlude(rect);
        f.render_widget(Clear, rect);
        let block = self
            .content_panel(
                if zh {
                    " 身份验证 "
                } else {
                    " AUTHENTICATION "
                }
                .into(),
                true,
            )
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let label = if zh { "密码  " } else { "Password  " };
        let mask = "•".repeat(dialog.input.chars().count());
        let visible = fit_text_end(
            &mask,
            (inner.width as usize).saturating_sub(unicode_width::UnicodeWidthStr::width(label) + 1),
        );
        let lines = vec![
            Line::from(Span::styled(
                if zh {
                    "此操作需要管理员权限"
                } else {
                    "Administrator authentication required"
                },
                style(DIM),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled(label, style(accent())),
                Span::styled(format!("{visible}▏"), style(TEXT)),
            ]),
            Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                style(BORDER),
            )),
            Line::default(),
            key_hints(&[
                ("Enter", if zh { "验证" } else { "authenticate" }),
                ("Esc", if zh { "取消" } else { "cancel" }),
                ("Ctrl+U", if zh { "清空" } else { "clear" }),
            ]),
        ];
        f.render_widget(Paragraph::new(lines), inner);
    }
    fn draw_help(&mut self, f: &mut Frame, area: Rect) {
        let zh = self.settings.language == Language::ZhCn;
        let body = if zh {
            "导航\n1–5  更新 / 安装 / 列表 / 设置 / 活动\nTab / Shift+Tab  下一 / 上一个来源或面板\n↑↓ / j k  选择软件包    PgUp/PgDn  翻页\nEsc  回到列表并清除过滤\n\n更新\nEnter  更新选中的 AUR 包\nu  更新当前来源    a  更新全部\np  切换 AUR 包的代理规则    r  重新扫描\n\n查看与安装\n列表：/  过滤    t  平铺 / 依赖树    Tab  详情\n依赖树：←→ / h l  收起 / 展开    Enter / Space  切换\n搜索：匹配包作为根，展开查看其依赖    ↩  循环引用\nd / Del  删除    Space  勾选删除选项\n安装：/  输入关键词    Enter  搜索 / 安装所选包\nv  查看 PKGBUILD 与评论    o  打开 AUR 页面\np  切换 AUR 代理\n\n设置\nCtrl+↑↓  滚动详情\n设置：↑↓ / Tab  选择    Enter  编辑/切换\n代理输入：Enter  保存    Esc  取消    Ctrl+U  清空\n\n输出与任务\n输出区：↑↓ / PgUp/PgDn  滚动\nHome  最早输出    End  实时输出\nCtrl+C  中断任务；空闲时退出    q  空闲时退出\n\n确认框\nTab / ←→ / y n  选择    Enter  确认    Esc  取消\nPgUp/PgDn  阅读长内容\n\n? / Esc  关闭帮助    ↑↓ / PgUp/PgDn  滚动帮助"
        } else {
            "NAVIGATION\n1–5  Updates / Install / List / Settings / Activity\nTab / Shift+Tab  Next / previous source or panel\n↑↓ / j k  Select package    PgUp/PgDn  Page\nEsc  Return to list and clear filter\n\nUPDATES\nEnter  Update selected AUR package\nu  Update source    a  Update all\np  Toggle AUR proxy rule    r  Refresh\n\nPACKAGES & INSTALL\nList: /  Filter    t  Flat / tree    Tab  Details\nTree: ←→ / h l  Fold / open    Enter / Space  Toggle\nSearch: matching packages become roots    ↩  Cycle reference\nd / Del  Remove    Space  Toggle removal options\nInstall: /  Enter query    Enter  Search / install selection\nv  View PKGBUILD and comments    o  Open AUR page\np  Toggle AUR proxy\n\nSETTINGS\nCtrl+↑↓  Scroll details\nSettings: ↑↓ / Tab  Select    Enter  Edit / toggle\nProxy: Enter  Save    Esc  Cancel    Ctrl+U  Clear\n\nOUTPUT & TASKS\nOutput: ↑↓ / PgUp/PgDn  Scroll\nHome  Oldest output    End  Live output\nCtrl+C  Interrupt task; exit when idle    q  Quit when idle\n\nCONFIRMATIONS\nTab / ←→ / y n  Select    Enter  Confirm    Esc  Cancel\nPgUp/PgDn  Read long content\n\n? / Esc  Close help    ↑↓ / PgUp/PgDn  Scroll help"
        };
        let width = area.width.saturating_sub(6).min(82);
        let height = area.height.saturating_sub(4).min(30);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        self.raster_lists.borrow_mut().occlude(rect);
        f.render_widget(Clear, rect);
        let block = self
            .content_panel(
                if zh {
                    " 快捷键 "
                } else {
                    " KEYBOARD SHORTCUTS "
                }
                .into(),
                true,
            )
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(rect);
        let paragraph = Paragraph::new(help_lines(body)).wrap(Wrap { trim: false });
        let total = paragraph.line_count(inner.width);
        self.help_scroll = self.help_scroll.min(
            total
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(paragraph.scroll((self.help_scroll, 0)).block(block), rect);
        draw_scroll_position(
            f,
            rect,
            self.help_scroll as usize,
            inner.height as usize,
            total,
            None,
            true,
        );
    }
    fn draw_settings(&mut self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        let proxy = if self.editing_proxy {
            format!("{}▏", self.proxy_input)
        } else if self.settings.proxy_url.is_empty() {
            "not configured".into()
        } else {
            masked_proxy(&self.settings.proxy_url)
        };
        let selected = |index| {
            if self.setting_selected == index {
                "▸"
            } else {
                " "
            }
        };
        let lines = vec![
            Line::from(vec![
                Span::styled(format!(" {} Proxy URL: ", selected(0)), style(accent())),
                Span::styled(proxy, style(TEXT)),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!(" {} Git network proxy: ", selected(1)),
                    style(accent()),
                ),
                Span::styled(
                    if self.settings.git_proxy { "ON" } else { "OFF" },
                    style(TEXT),
                ),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!(" {} Language / 语言: ", selected(2)),
                    style(accent()),
                ),
                Span::styled(
                    if self.settings.language == Language::En {
                        "English"
                    } else {
                        "简体中文"
                    },
                    style(TEXT),
                ),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(format!(" {} Theme color: ", selected(3)), style(accent())),
                Span::styled(
                    self.settings.accent.name(self.settings.language),
                    style(TEXT),
                ),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!(
                        " {} {}: ",
                        selected(4),
                        if self.settings.language == Language::ZhCn {
                            "清理缓存"
                        } else {
                            "Clean package cache"
                        }
                    ),
                    style(accent()),
                ),
                Span::styled("paru -Scc", style(TEXT)),
            ]),
        ];
        f.render_widget(
            self.paragraph(lines).wrap(Wrap { trim: false }).block(
                self.content_panel(" SETTINGS ".into(), true)
                    .padding(Padding::new(2, 2, 1, 1)),
            ),
            cols[0],
        );
        let zh = self.settings.language == Language::ZhCn;
        let heading = |text: &str| {
            Line::from(Span::styled(
                text.to_owned(),
                style(accent()).add_modifier(Modifier::BOLD),
            ))
        };
        let text = |text: &str| Line::from(Span::styled(text.to_owned(), style(TEXT)));
        let hint = |text: &str| Line::from(Span::styled(text.to_owned(), style(DIM)));
        let mut details = Vec::new();
        match self.setting_selected {
            0 => {
                details.push(heading(if zh { "代理地址" } else { "Proxy URL" }));
                details.push(text(if zh {
                    "供已标记的 AUR 软件包及 Git 网络代理使用。"
                } else {
                    "Used by marked AUR packages and the Git network proxy."
                }));
                details.push(Line::default());
                details.push(hint(if zh { "主机:端口 默认使用 HTTP；也支持完整的 HTTP(S)、SOCKS5 / SOCKS5H 地址。" } else { "host:port defaults to HTTP. Full HTTP(S), SOCKS5 and SOCKS5H URLs are supported." }));
                details.push(text("127.0.0.1:7890"));
                if self.editing_proxy {
                    details.push(Line::default());
                    details.push(key_hints(&[
                        ("Enter", if zh { "保存" } else { "save" }),
                        ("Esc", if zh { "取消" } else { "cancel" }),
                        ("Ctrl+U", if zh { "清空" } else { "clear" }),
                    ]));
                }
                if let Some(error) = &self.proxy_error {
                    details.push(Line::from(Span::styled(
                        super::i18n::translate(error, self.settings.language),
                        style(RED),
                    )));
                }
                details.push(Line::default());
                details.push(heading(if zh {
                    "以下为采用代理的软件包："
                } else {
                    "Packages using a proxy:"
                }));
                if self.settings.proxy_bases.is_empty() {
                    details.push(hint(if zh {
                        "暂无。在更新页选中 AUR 包，按 p 添加。"
                    } else {
                        "None yet. Select an AUR package in Updates, Install or List and press p / P."
                    }));
                } else {
                    details.extend(
                        self.settings
                            .proxy_bases
                            .iter()
                            .map(|base| text(&format!("• {base}"))),
                    );
                }
            }
            1 => {
                details.push(heading(if zh {
                    "Git 网络代理"
                } else {
                    "Git network proxy"
                }));
                details.push(text(if zh {
                    "让 Git 远端提交检查及更新、安装任务中的 Git 操作通过所填写的代理地址连接网络。"
                } else {
                    "Routes Git operations launched by paru-tui through the configured proxy URL."
                }));
                details.push(Line::default());
                details.push(text("clone / fetch / ls-remote"));
                details.push(hint(if zh { "包括扫描更新时的 git ls-remote，以及 AUR 构建仓库和 Git 源码的克隆、拉取。代理请求需使用 HTTP(S) 远程地址。" } else { "Covers git ls-remote during scanning, plus cloning and fetching AUR build repositories and Git sources. Proxied remotes must use HTTP(S)." }));
                details.push(Line::default());
                details.push(text(if zh {
                    "开启：上述 Git 请求统一使用代理。"
                } else {
                    "ON: these Git requests use the proxy."
                }));
                details.push(text(if zh { "关闭：未标记的软件包直连；已标记的 AUR 包仍按单包规则使用代理。" } else { "OFF: unmarked packages connect directly; marked AUR packages still follow their proxy rules." }));
            }
            2 => {
                details.push(heading(if zh {
                    "界面语言"
                } else {
                    "Interface language"
                }));
                details.push(text("English / 简体中文"));
                details.push(Line::default());
                details.push(text(if zh {
                    "切换页面、通知、确认弹窗与帮助文案的语言。"
                } else {
                    "Changes the language of pages, notifications, confirmations and help."
                }));
                details.push(hint(if zh {
                    "软件包名称、命令参数及构建文件保留原文。"
                } else {
                    "Package names, command arguments and build files retain their original text."
                }));
            }
            3 => {
                details.push(heading(if zh { "主题色" } else { "Theme color" }));
                details.push(text(if zh {
                    "按 Enter 或空格循环选择，立即生效并保存。"
                } else {
                    "Press Enter or Space to cycle presets, apply and save."
                }));
                details.push(Line::default());
                for preset in super::settings::Accent::ALL {
                    details.push(Line::from(vec![
                        Span::styled(
                            if preset == self.settings.accent {
                                "▸ "
                            } else {
                                "  "
                            },
                            style(accent()),
                        ),
                        Span::styled("●  ", style(super::theme::preset_color(preset))),
                        Span::styled(preset.name(self.settings.language), style(TEXT)),
                    ]));
                }
                details.push(Line::default());
                details.push(hint(if zh { "焦点、按键提示与选中背景跟随主题色；成功与错误保持绿色和红色。" } else { "Focus, key hints and selection follow the theme; success and errors stay green and red." }));
            }
            _ => {
                details.push(heading(if zh {
                    "清理软件包缓存"
                } else {
                    "Clean package cache"
                }));
                details.push(text("paru -Scc"));
                details.push(Line::default());
                details.push(text(if zh {
                    "清空 pacman 软件包缓存并清理不再使用的仓库数据库。paru 随后还会询问是否删除全部 AUR 克隆和已保存的差异。"
                } else {
                    "Clear the pacman package cache and unused repository databases. paru then asks whether to remove all AUR clones and saved diffs."
                }));
                details.push(Line::default());
                details.push(hint(if zh {
                    "按 Enter 或空格查看命令并确认；之后由 paru 执行清理。"
                } else {
                    "Press Enter or Space to review the command and confirm before paru runs it."
                }));
            }
        }
        let block = self
            .content_panel(
                if zh {
                    " 设置详情 "
                } else {
                    " SETTING DETAILS "
                }
                .into(),
                false,
            )
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(cols[1]);
        let paragraph = Paragraph::new(details).wrap(Wrap { trim: false });
        self.detail_scroll = self.detail_scroll.min(
            paragraph
                .line_count(inner.width)
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        let total = paragraph.line_count(inner.width);
        f.render_widget(
            paragraph.scroll((self.detail_scroll, 0)).block(block),
            cols[1],
        );
        draw_scroll_position(
            f,
            cols[1],
            self.detail_scroll as usize,
            inner.height as usize,
            total,
            None,
            false,
        );
    }
    fn draw_activity(&mut self, f: &mut Frame, area: Rect) {
        let block = self.content_panel(" SESSION ACTIVITY ".into(), true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let prefix_width = 17.min(inner.width);
        let body_width = inner.width.saturating_sub(prefix_width);
        if body_width == 0 || inner.height == 0 {
            return;
        }
        let entries: Vec<_> = self
            .activity
            .iter()
            .map(|entry| {
                let (timestamp, message) = entry.split_once("  ").unwrap_or(("--:--:--", entry));
                let (level, color) = log_level(message);
                let body = Paragraph::new(super::i18n::translate(
                    &clean_multiline(message),
                    self.settings.language,
                ))
                .style(style(if level == "ERROR" { RED } else { TEXT }))
                .wrap(Wrap { trim: false });
                let height = body.line_count(body_width).max(1);
                (timestamp.to_owned(), level, color, body, height)
            })
            .collect();
        let total: usize = entries.iter().map(|entry| entry.4).sum();
        self.activity_scroll = self.activity_scroll.min(
            total
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        let mut skip = self.activity_scroll as usize;
        let mut y = inner.y;
        for (timestamp, level, color, body, height) in entries {
            if skip >= height {
                skip -= height;
                continue;
            }
            let visible = (height - skip).min((inner.bottom() - y) as usize) as u16;
            if visible == 0 {
                break;
            }
            if skip == 0 {
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("{timestamp}  "), style(DIM)),
                        Span::styled(format!("{level:<5}  "), style(color)),
                    ])),
                    Rect::new(inner.x, y, prefix_width, 1),
                );
            }
            f.render_widget(
                body.scroll((skip.min(u16::MAX as usize) as u16, 0)),
                Rect::new(inner.x + prefix_width, y, body_width, visible),
            );
            y += visible;
            skip = 0;
        }
        draw_scroll_position(
            f,
            area,
            self.activity_scroll as usize,
            inner.height as usize,
            total,
            None,
            true,
        );
    }
    fn draw_toast(&self, f: &mut Frame, area: Rect) {
        let Some(toast) = &self.toast else {
            return;
        };
        let expansion = toast.expansion();
        if expansion <= 0.0 {
            return;
        }
        let width = area.width.saturating_sub(4).min(62);
        let paragraph = self
            .paragraph(clean_multiline(&toast.message))
            .wrap(Wrap { trim: false });
        let rows = paragraph.line_count(width.saturating_sub(6)).clamp(1, 6);
        let height = (rows as u16 + 4).min(area.height.saturating_sub(2));
        // Render at a fixed size, then translate the surface and clip at the viewport.
        // Text never reflows or disappears midway through the slide.
        let surface = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(surface);
        let color = if toast.error { RED } else { GREEN };
        let label = if toast.error { " ERROR " } else { " NOTICE " };
        self.paragraph(clean_multiline(&toast.message))
            .style(style(color).bg(Color::Reset))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(Span::styled(
                        super::i18n::translate(label, self.settings.language),
                        style(color),
                    ))
                    .border_style(style(color))
                    .padding(Padding::new(2, 2, 1, 1)),
            )
            .render(surface, &mut buffer);
        let x = area
            .right()
            .saturating_sub(1 + (width as f32 * expansion).round() as u16);
        let visible = width.min(area.right().saturating_sub(x));
        self.raster_lists
            .borrow_mut()
            .occlude(Rect::new(x, area.y + 1, visible, height));
        self.tab_layer
            .borrow_mut()
            .occlude_right(Rect::new(x, area.y + 1, visible, height));
        self.selection_layer
            .borrow_mut()
            .occlude_right(Rect::new(x, area.y + 1, visible, height));
        for row in 0..height {
            for col in 0..visible {
                let mut cell = buffer[(col, row)].clone();
                if unicode_width::UnicodeWidthStr::width(cell.symbol()) > (visible - col) as usize {
                    cell.set_symbol(" ");
                }
                f.buffer_mut()[(x + col, area.y + 1 + row)] = cell;
            }
        }
    }

    fn draw_build(&mut self, f: &mut Frame, area: Rect) {
        let downloading = self
            .session
            .as_ref()
            .is_some_and(|s| !s.building && s.downloads.visible());
        let title = if downloading {
            "DOWNLOADS"
        } else {
            "BUILD OUTPUT"
        };
        let block = self.content_panel(
            format!(
                " {title} · {} ",
                self.session
                    .as_ref()
                    .filter(|s| s.building)
                    .map(|s| s.build_name.as_str())
                    .unwrap_or(if downloading { "pacman" } else { "idle" })
            ),
            self.focus == 2,
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(s) = &mut self.session else {
            f.render_widget(
                self.paragraph("Build output appears here while a package is built.")
                    .style(style(DIM)),
                inner,
            );
            return;
        };
        if downloading {
            s.downloads
                .draw(f, inner, self.settings.language == Language::ZhCn);
            return;
        }
        let size = (inner.height.max(2), inner.width.max(2));
        if s.parser.screen().size() != size {
            s.parser.set_size(size.0, size.1);
            let _ = s.pty.resize(size.0, size.1);
        }
        let screen = s.parser.screen();
        let lines = terminal_lines(screen, inner.height, inner.width);
        f.render_widget(Paragraph::new(lines), inner);
        let (row, col) = screen.cursor_position();
        if self.focus == 2
            && s.building
            && s.code.is_none()
            && !screen.hide_cursor()
            && screen.scrollback() == 0
            && row < inner.height
            && col < inner.width
        {
            f.set_cursor_position((inner.x + col, inner.y + row));
        }
    }
}

fn terminal_lines(screen: &vt100::Screen, height: u16, width: u16) -> Vec<Line<'static>> {
    (0..height)
        .map(|row| {
            let mut spans = Vec::<Span>::new();
            let mut text = String::new();
            let mut current = None;
            for col in 0..width {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let cell_style = terminal_style(cell);
                if current.is_some_and(|style| style != cell_style) {
                    spans.push(Span::styled(std::mem::take(&mut text), current.unwrap()));
                }
                current = Some(cell_style);
                if cell.has_contents() {
                    text.push_str(&cell.contents());
                } else {
                    text.push(' ');
                }
            }
            if let Some(style) = current {
                spans.push(Span::styled(text, style));
            }
            Line::from(spans)
        })
        .collect()
}

fn terminal_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(terminal_color(cell.fgcolor()))
        .bg(terminal_color(cell.bgcolor()));
    for (active, modifier) in [
        (cell.bold(), Modifier::BOLD),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
    ] {
        if active {
            style = style.add_modifier(modifier);
        }
    }
    style
}

fn terminal_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn dim_backdrop(f: &mut Frame) {
    for cell in &mut f.buffer_mut().content {
        cell.set_fg(mix(cell.fg, BORDER, 0.70));
        if cell.bg != Color::Reset {
            cell.set_bg(mix(cell.bg, COMMAND_BG, 0.70));
        }
        cell.set_style(Style::default().remove_modifier(Modifier::BOLD | Modifier::REVERSED));
    }
}
fn draw_scroll_position(
    f: &mut Frame,
    area: Rect,
    offset: usize,
    visible: usize,
    total: usize,
    selected: Option<usize>,
    active: bool,
) {
    if total == 0 || area.height < 3 || area.width < 8 {
        return;
    }
    if total <= visible && selected.is_none() {
        return;
    }
    let color = if active { accent() } else { DIM };
    let track = area.height.saturating_sub(2) as usize;
    if total > visible && track > 0 {
        let thumb = (track * visible / total).max(1).min(track);
        let top = offset.min(total - visible) * (track - thumb) / (total - visible).max(1);
        for row in 0..track {
            let selected = row >= top && row < top + thumb;
            f.buffer_mut()[(area.right() - 1, area.y + 1 + row as u16)]
                .set_symbol(if selected { "┃" } else { "│" })
                .set_fg(if selected { color } else { BORDER });
        }
    }
    let label = if let Some(selected) = selected {
        format!(" {}/{} ", selected + 1, total)
    } else {
        format!(
            " {}–{}/{} ",
            offset + 1,
            (offset + visible).min(total),
            total
        )
    };
    let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
    if width + 4 < area.width {
        f.render_widget(
            Paragraph::new(Span::styled(label, style(color))),
            Rect::new(area.right() - width - 2, area.bottom() - 1, width, 1),
        );
    }
}
fn key_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, description)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(format!("{key} "), style(accent())));
        spans.push(Span::styled(description.to_string(), style(DIM)));
    }
    Line::from(spans)
}
fn log_level(text: &str) -> (&'static str, Color) {
    let lower = text.to_lowercase();
    if ["error", "failed", "cannot", "invalid", "not saved"]
        .iter()
        .any(|word| lower.contains(word))
    {
        ("ERROR", RED)
    } else if ["warning", "unavailable", "cached", "fallback"]
        .iter()
        .any(|word| lower.contains(word))
    {
        ("WARN", YELLOW)
    } else {
        ("INFO", accent())
    }
}
fn pkgbuild_target(page: usize, package: &Package) -> Option<(String, Option<String>)> {
    if package.is_aur() {
        Some((package.name.clone(), Some(package.base.clone())))
    } else if page == 1 && package.source == "foreign" {
        Some((package.name.clone(), None))
    } else {
        None
    }
}
fn question_title(q: &Question) -> &'static str {
    if q.text.starts_with("Remove the prepared packages?") {
        return "REMOVAL PLAN";
    }
    if q.text.contains("files from cache?")
        || q.text.contains("unused repositories?")
        || q.text.contains("AUR packages from cache?")
        || q.text.contains("saved diffs?")
    {
        return "CLEAN PACKAGE CACHE";
    }
    if q.secret {
        "AUTHENTICATION"
    } else if q.text.starts_with("Review changes") {
        "REVIEW CHANGES"
    } else if q.text.starts_with("Review build files") {
        "REVIEW BUILD FILES"
    } else if q.text.starts_with("Select provider") || q.text.contains("providers available") {
        "SELECT PROVIDER"
    } else if q.text.starts_with("Select group") {
        "SELECT GROUP PACKAGES"
    } else if !q.plan.is_empty() {
        "TRANSACTION PLAN"
    } else if q.text.contains("signing key") {
        "SIGNING KEY"
    } else if q.text.contains("conflict") {
        "RESOLVE CONFLICT"
    } else if q.default.is_none() {
        "INPUT REQUIRED"
    } else {
        "CONFIRM OPERATION"
    }
}
fn fit_text_end(text: &str, width: usize) -> String {
    let chars: Vec<_> = text.chars().collect();
    chars[chars.len().saturating_sub(width)..].iter().collect()
}
fn shell_line(args: &[String]) -> Line<'static> {
    let mut spans = vec![
        Span::styled("$ ", style(GREEN)),
        Span::styled("paru", style(TEXT).add_modifier(Modifier::BOLD)),
    ];
    for (index, arg) in args.iter().enumerate() {
        let quoted = if arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=./:@".contains(c))
        {
            arg.clone()
        } else {
            format!("'{}'", arg.replace('\'', "'\"'\"'"))
        };
        spans.push(Span::styled(
            format!(" {quoted}"),
            style(if index == 0 { accent() } else { DIM }),
        ));
    }
    Line::from(spans)
}
fn confirmation_choice(yes: bool, language: Language) -> Line<'static> {
    let zh = language == Language::ZhCn;
    let option = |label: &str, selected: bool, color| {
        Span::styled(
            if selected {
                format!("[ {label} ]")
            } else {
                format!("  {label}  ")
            },
            style(color).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        )
    };
    Line::from(vec![
        option(if zh { "是" } else { "YES" }, yes, GREEN),
        Span::raw("    "),
        option(if zh { "否" } else { "NO" }, !yes, RED),
    ])
}
fn dialog_lines(dialog: &Dialog, language: Language) -> Vec<Line<'static>> {
    let mut lines = localized_dialog_lines(&dialog.body, language);
    // Build files retain their original diff colors and text.
    if dialog.body.starts_with("Review changes for ")
        || dialog.body.starts_with("Review build files for ")
    {
        return lines;
    }
    let plan: HashMap<String, &super::bridge::PlanItem> = dialog
        .question
        .as_ref()
        .map(|q| {
            q.plan
                .iter()
                .map(|p| (format!("{}/{}  {}", p.source, p.name, p.version), p))
                .collect()
        })
        .unwrap_or_default();
    if plan.is_empty() {
        return lines;
    }
    for (index, (line, raw)) in lines.iter_mut().zip(dialog.body.lines()).enumerate() {
        if let Some(p) = plan.get(raw) {
            *line = Line::from(vec![
                Span::styled(format!("{}/", p.source), style(DIM)),
                Span::styled(
                    p.name.clone(),
                    style(if p.source == "REMOVE" { RED } else { TEXT })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", p.version),
                    style(if p.source == "REMOVE" { RED } else { GREEN }),
                ),
            ]);
        } else if let Some((label, value)) = raw.split_once(':').filter(|(label, _)| {
            matches!(
                *label,
                "Download size"
                    | "Installed size"
                    | "Net change"
                    | "Packages to remove"
                    | "Freed disk space"
            )
        }) {
            *line = Line::from(vec![
                Span::styled(
                    super::i18n::translate(&format!("{label}:"), language),
                    style(DIM),
                ),
                Span::styled(
                    value.to_owned(),
                    style(if label == "Download size" {
                        accent()
                    } else {
                        TEXT
                    })
                    .add_modifier(Modifier::BOLD),
                ),
            ]);
        } else if index == 0 {
            for span in &mut line.spans {
                span.style = style(accent()).add_modifier(Modifier::BOLD);
            }
        } else {
            for span in &mut line.spans {
                span.style = style(DIM);
            }
        }
    }
    lines
}
fn localized_dialog_lines(body: &str, language: Language) -> Vec<Line<'static>> {
    let review =
        body.starts_with("Review changes for ") || body.starts_with("Review build files for ");
    let mut lines = review_lines(body.trim_end());
    let count = lines.len();
    for (index, line) in lines.iter_mut().enumerate() {
        if !review || index == 0 || index + 1 == count {
            for span in &mut line.spans {
                span.content = super::i18n::translate(&span.content, language).into();
            }
        }
    }
    lines
}
fn fit_text(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return format!("{text}{}", " ".repeat(width - text.width()));
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width - 1 {
            break;
        }
        used += size;
        result.push(c);
    }
    result.push('…');
    result.push_str(&" ".repeat(width - used - 1));
    result
}
fn help_lines(body: &str) -> Vec<Line<'static>> {
    body.lines()
        .map(|line| {
            if line.is_empty() {
                return Line::default();
            }
            if !line.contains("  ") {
                return Line::from(Span::styled(
                    line.to_owned(),
                    style(accent()).add_modifier(Modifier::BOLD),
                ));
            }
            // Help source uses two spaces between key/description groups.
            let mut spans = Vec::new();
            for (i, part) in line.split("  ").filter(|part| !part.is_empty()).enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    part.to_owned(),
                    style(if i % 2 == 0 { accent() } else { DIM }),
                ));
            }
            Line::from(spans)
        })
        .collect()
}
fn detail_field(
    label: &str,
    value: &str,
    query: &str,
    language: Language,
    width: u16,
) -> Vec<Line<'static>> {
    let optional = label == "Optional dependencies";
    let network = label == "Network";
    let label = super::i18n::translate(
        if label == "Optional dependencies" && language == Language::En {
            "Optional deps"
        } else {
            label
        },
        language,
    );
    let value = if value.is_empty() { "None" } else { value };
    let value = if matches!(value, "None" | "Explicit" | "Depend") {
        super::i18n::translate(value, language)
    } else {
        clean_multiline(value)
    };
    // Wrap only the value column. Keep explicit line breaks (optional deps, backups).
    let prefix = 17.min(width.saturating_sub(4) as usize);
    let body_width = width.saturating_sub(prefix as u16).max(1);
    let body = Paragraph::new(
        value
            .lines()
            .map(|line| {
                let split = if optional {
                    line.find(": ")
                } else if network {
                    line.find(" (")
                } else {
                    None
                };
                if let Some(split) = split {
                    let mut main = highlight(&line[..split], query, style(TEXT));
                    let note = super::i18n::translate(&line[split..], language);
                    main.spans.extend(highlight(&note, query, style(DIM)).spans);
                    main
                } else {
                    highlight(line, query, style(TEXT))
                }
            })
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false });
    let height = body.line_count(body_width).max(1).min(u16::MAX as usize) as u16;
    let mut buffer = Buffer::empty(Rect::new(0, 0, body_width, height));
    body.render(buffer.area, &mut buffer);
    (0..height)
        .map(|y| {
            let mut spans = vec![Span::styled(
                if y == 0 {
                    format!(
                        "{}{}",
                        fit_text(&label, prefix.saturating_sub(2)),
                        " ".repeat(prefix.min(2))
                    )
                } else {
                    " ".repeat(prefix)
                },
                style(DIM),
            )];
            let mut x = 0;
            while x < body_width {
                let cell = &buffer[(x, y)];
                spans.push(Span::styled(cell.symbol().to_owned(), cell.style()));
                x += unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            }
            Line::from(spans)
        })
        .collect()
}
fn review_lines(body: &str) -> Vec<Line<'static>> {
    let diff = body.starts_with("Review changes for ");
    clean_multiline(body)
        .lines()
        .map(|line| {
            let color = if !diff {
                TEXT
            } else if line.starts_with("+++")
                || line.starts_with("---")
                || line.starts_with("diff --git")
                || line.starts_with("index ")
            {
                BLUE
            } else if line.starts_with("@@") {
                accent()
            } else if line.starts_with('+') {
                GREEN
            } else if line.starts_with('-') {
                RED
            } else {
                TEXT
            };
            Line::from(Span::styled(line.to_owned(), style(color)))
        })
        .collect()
}
fn highlight(text: &str, query: &str, base: Style) -> Line<'static> {
    if query.is_empty() {
        return Line::from(Span::styled(text.to_owned(), base));
    }
    let pattern = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .unwrap();
    let mut spans = Vec::new();
    let mut end = 0;
    for found in pattern.find_iter(text) {
        spans.push(Span::styled(text[end..found.start()].to_owned(), base));
        spans.push(Span::styled(
            found.as_str().to_owned(),
            base.fg(Color::Black)
                .bg(YELLOW)
                .add_modifier(Modifier::BOLD),
        ));
        end = found.end();
    }
    spans.push(Span::styled(text[end..].to_owned(), base));
    Line::from(spans)
}
fn masked_proxy(s: &str) -> String {
    if let Ok(mut url) = url::Url::parse(s) {
        if url.password().is_some() {
            let _ = url.set_password(Some("••••"));
        }
        url.to_string()
    } else {
        "invalid URL".into()
    }
}
fn clean_multiline(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}
pub fn run() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!("An interactive terminal is required");
    }
    let mut app = App::new()?;
    *app.selection_layer.borrow_mut() = super::selection::SelectionLayer::detect();
    *app.tab_layer.borrow_mut() = super::selection::SelectionLayer::tabs();
    *app.raster_lists.borrow_mut() = super::raster::Lists::new(app.selection_layer.borrow().cell());
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    enable_raw_mode()?;
    let mut guard = Guard(Vec::new());
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
        guard
            .0
            .push(signal_hook::flag::register(signal, shutdown.clone())?);
    }
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    app.refresh();
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("Terminal session interrupted");
        }
        app.tick()?;
        let pixel_selection = app.selection_layer.borrow().enabled();
        if pixel_selection {
            execute!(
                terminal.backend_mut(),
                crossterm::terminal::BeginSynchronizedUpdate
            )?;
        }
        terminal.draw(|f| app.draw(f))?;
        app.selection_layer
            .borrow_mut()
            .present(terminal.backend_mut())?;
        app.tab_layer.borrow_mut().present(terminal.backend_mut())?;
        app.raster_lists
            .borrow_mut()
            .present(terminal.backend_mut())?;
        if pixel_selection {
            execute!(
                terminal.backend_mut(),
                crossterm::terminal::EndSynchronizedUpdate
            )?;
        }
        let frame_ms = if app.toast.as_ref().is_some_and(Toast::animating)
            || app.motion.borrow().active(Instant::now())
        {
            16
        } else {
            100
        };
        if event::poll(Duration::from_millis(frame_ms))? {
            match event::read()? {
                TermEvent::Key(key) => {
                    if app.key(key)? {
                        break;
                    }
                }
                TermEvent::Paste(text) => {
                    if let Some(dialog) = &mut app.dialog {
                        if dialog
                            .question
                            .as_ref()
                            .is_some_and(|q| q.default.is_none())
                        {
                            dialog
                                .input
                                .extend(text.chars().filter(|c| !c.is_control()));
                        }
                    } else if app.editing_proxy {
                        app.proxy_input.push_str(&text);
                    } else if app.searching {
                        if app.page == 4 {
                            app.install_query
                                .push_str(&clean_multiline(&text).replace('\n', " "));
                        } else {
                            app.query.push_str(&text);
                            app.reindex();
                        }
                    }
                }
                TermEvent::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.pkgbuild_click((mouse.column, mouse.row));
                    }
                    _ => app.mouse_scroll_at(mouse.kind, Some((mouse.column, mouse.row))),
                },
                TermEvent::Resize(_, _) => {
                    app.selection_layer.borrow_mut().resize();
                    app.tab_layer.borrow_mut().resize();
                    app.raster_lists
                        .borrow_mut()
                        .resize(app.selection_layer.borrow().cell());
                }
                _ => {}
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_terminal_preserves_ansi_colors_and_attributes() {
        let mut parser = vt100::Parser::new(2, 40, 0);
        parser.process(b"plain \x1b[1;31merror\x1b[0m \x1b[38;2;12;34;56mcustom\x1b[0m");
        let lines = terminal_lines(parser.screen(), 2, 40);
        let error = lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains("error"))
            .unwrap();
        assert_eq!(error.style.fg, Some(Color::Indexed(1)));
        assert!(error.style.add_modifier.contains(Modifier::BOLD));
        let custom = lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains("custom"))
            .unwrap();
        assert_eq!(custom.style.fg, Some(Color::Rgb(12, 34, 56)));
    }

    #[test]
    fn removal_requires_options_then_command_confirmation_and_can_cancel_both() {
        let mut app = App::new().unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.catalog.installed = vec![search_result("core")];
        app.key(key(KeyCode::Char('3'))).unwrap();
        assert_eq!(app.page, 1);
        app.key(key(KeyCode::Delete)).unwrap();
        assert!(app.removal.is_some());
        assert!(app.dialog.is_none() && app.session.is_none());
        assert_eq!(
            app.removal.as_ref().unwrap().args(),
            ["-Rssc", "--", "example"]
        );
        app.key(key(KeyCode::Down)).unwrap();
        app.key(key(KeyCode::Char(' '))).unwrap();
        app.key(key(KeyCode::Enter)).unwrap();
        assert!(app.removal.is_none());
        assert_eq!(
            app.dialog.as_ref().unwrap().action.as_ref().unwrap(),
            &["-Rsc", "--", "example"]
        );
        assert!(app.session.is_none());
        app.key(key(KeyCode::Esc)).unwrap();
        app.key(key(KeyCode::Char('d'))).unwrap();
        assert_eq!(
            app.removal.as_ref().unwrap().args(),
            ["-Rssc", "--", "example"]
        );
        app.key(key(KeyCode::Esc)).unwrap();
        assert!(app.removal.is_none() && app.dialog.is_none() && app.session.is_none());
        app.key(key(KeyCode::Tab)).unwrap();
        app.key(key(KeyCode::Delete)).unwrap();
        assert!(
            app.removal.is_none(),
            "inspector focus must not start deletion"
        );
        app.key(key(KeyCode::Char('2'))).unwrap();
        assert_eq!(app.page, 4);
        assert!(!app.searching);
        app.key(key(KeyCode::Char('/'))).unwrap();
        assert!(app.searching);
    }
    #[test]
    fn install_badges_stay_red_and_dates_keep_right_padding() {
        let mut app = App::new().unwrap();
        app.page = 4;
        app.search_results = vec![search_result("aur")];
        app.search_indices = vec![0];
        for language in [Language::En, Language::ZhCn] {
            app.settings.language = language;
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 8)).unwrap();
            terminal
                .draw(|f| {
                    app.draw_list(
                        f,
                        f.area(),
                        " RESULTS ",
                        &app.search_results,
                        &app.search_indices,
                        0,
                        true,
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = (0..80)
                .map(|x| buffer[(x, 2)].symbol())
                .collect::<String>()
                .replace(' ', "");
            assert!(text.contains(if language == Language::En {
                "[old][orphan]"
            } else {
                "[旧][孤]"
            }));
            assert_eq!(buffer[(5, 2)].fg, RED);
            assert_eq!(buffer[(77, 2)].symbol(), " ");
            assert_eq!(buffer[(78, 2)].symbol(), " ");
        }
        let mut official = search_result("core");
        assert!(package_badges(&official, Language::ZhCn).is_empty());
        official.source = "aur".into(); // A binary repository named aur is not AUR.
        assert!(package_badges(&official, Language::ZhCn).is_empty());
    }
    #[test]
    fn pkgbuild_view_is_read_only_and_scrollable_with_mouse() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.pkgbuild_view = Some(PkgbuildView {
            id: 1,
            base: "demo".into(),
            pkgbuild: Some("pkgname=demo\n".to_owned() + &"line\n".repeat(80)),
            pkgbuild_error: None,
            comments: vec![AurComment {
                id: "comment-1".into(),
                header: "maintainer commented on 2026-01-01".into(),
                pinned: true,
                lines: std::iter::once(catalog::CommentLine {
                    spans: vec![catalog::CommentSpan {
                        text: "read docs".into(),
                        url: Some("https://example.org/docs".into()),
                    }],
                    code: false,
                })
                .chain(std::iter::once(catalog::CommentLine::plain(
                    "makepkg -si",
                    true,
                )))
                .chain((0..80).map(|_| catalog::CommentLine::plain("comment body", false)))
                .collect(),
            }],
            comments_loading: false,
            comments_error: None,
            focus: 0,
            scroll: [0, 0],
            scroll_max: [0, 0],
        });
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("pkgname=demo"));
        assert!(text.contains("makepkg -si"));
        assert!(text.contains("[PKGBUILD] demo"));
        assert!(text.contains("AUR COMMENTS (1)"));
        assert!(buffer.content.iter().any(|cell| cell.bg == COMMAND_BG));
        assert!(!text.contains("YES") && !text.contains("[ 是 ]"));
        let hit = app
            .pkgbuild_comment_cache
            .as_ref()
            .unwrap()
            .links
            .first()
            .unwrap();
        let inner = app.pkgbuild_comment_inner;
        assert_eq!(
            app.pkgbuild_link_at((inner.x + hit.start, inner.y + hit.row as u16)),
            Some("https://example.org/docs")
        );
        app.pkgbuild_key(key(KeyCode::Down));
        assert_eq!(app.pkgbuild_view.as_ref().unwrap().scroll[0], 1);
        app.mouse_scroll_at(MouseEventKind::ScrollDown, None);
        assert_eq!(app.pkgbuild_view.as_ref().unwrap().scroll[0], 4);
        app.mouse_scroll_at(MouseEventKind::ScrollUp, None);
        assert_eq!(app.pkgbuild_view.as_ref().unwrap().scroll[0], 1);
        let right = app.pkgbuild_panels[1];
        app.mouse_scroll_at(MouseEventKind::ScrollDown, Some((right.x + 2, right.y + 2)));
        let view = app.pkgbuild_view.as_ref().unwrap();
        assert_eq!(view.focus, 1);
        assert_eq!(view.scroll[1], 3);
        app.pkgbuild_key(key(KeyCode::Esc));
        assert!(app.pkgbuild_view.is_none());
    }
    #[test]
    fn cache_cleanup_in_settings_requires_command_confirmation() {
        let mut app = App::new().unwrap();
        app.page = 2;
        app.setting_selected = 3;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.setting_selected, 4);
        app.key(key(KeyCode::Enter)).unwrap();
        let dialog = app.dialog.as_ref().unwrap();
        assert_eq!(dialog.action.as_ref().unwrap(), &["-Scc"]);
        assert!(dialog.yes);
        assert!(app.session.is_none());
        app.key(key(KeyCode::Esc)).unwrap();
        assert!(app.dialog.is_none() && app.session.is_none());
    }
    #[test]
    fn removal_selector_fits_small_terminals_in_both_languages() {
        let mut app = App::new().unwrap();
        for language in [Language::En, Language::ZhCn] {
            app.settings.language = language;
            for (width, height) in [(62, 18), (100, 30)] {
                app.removal = Some(super::super::removal::Removal::new(
                    "example-package".into(),
                ));
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("--unneeded"));
                assert!(text.contains("Space") && text.contains("Enter") && text.contains("Esc"));
            }
        }
    }
    fn search_result(source: &str) -> Package {
        Package {
            devel: false,
            name: "example".into(),
            base: "example-base".into(),
            source: source.into(),
            version: "2-1".into(),
            installed: None,
            next: None,
            description: "Example package".into(),
            url: "https://example.org".into(),
            size: 0,
            dependencies: vec![],
            ignored: false,
            search: "example".into(),
            remote: Some(catalog::Remote {
                target: None,
                updated: Some(1_789_516_800),
                aur_url: (source == "aur")
                    .then(|| "https://aur.archlinux.org/packages/example".into()),
                orphaned: true,
                out_of_date: Some(1_789_516_800),
                fields: vec![],
            }),
        }
    }
    #[test]
    fn foreign_installed_package_can_resolve_pkgbuild_and_keep_aur_source() {
        let mut package = search_result("foreign");
        package.remote = None;
        package.installed = Some(package.version.clone());
        assert_eq!(pkgbuild_target(1, &package), Some(("example".into(), None)));
        assert_eq!(pkgbuild_target(4, &package), None);
        assert_eq!(
            super::super::i18n::translate(
                "PKGBUILD is available for AUR packages only",
                Language::ZhCn,
            ),
            "只有 AUR 软件包可以查看 PKGBUILD"
        );

        let mut app = App::new().unwrap();
        app.page = 1;
        app.catalog.installed = vec![package.clone()];
        app.reindex();
        app.tx
            .send(Event::AurResolved {
                id: 1,
                name: package.name.clone(),
                base: "split-base".into(),
            })
            .unwrap();
        app.tx
            .send(Event::Loaded(Catalog {
                installed: vec![package],
                ..Catalog::default()
            }))
            .unwrap();
        app.tick().unwrap();
        let resolved = &app.catalog.installed[0];
        assert_eq!(resolved.source, "aur");
        assert_eq!(resolved.base, "split-base");
        assert_eq!(
            pkgbuild_target(1, resolved),
            Some(("example".into(), Some("split-base".into())))
        );
    }
    #[test]
    fn dependency_tree_navigation_details_and_removal_target_the_visible_node() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.catalog.installed = ["app", "lib", "shared"]
            .into_iter()
            .map(|name| {
                let mut p = search_result("core");
                p.name = name.into();
                p.search = name.into();
                p
            })
            .collect();
        app.catalog.dependencies =
            super::super::tree::Graph::new(vec![vec![1, 2], vec![2], vec![]]);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for code in [
            KeyCode::Char('3'),
            KeyCode::Char('t'),
            KeyCode::Right,
            KeyCode::Right,
        ] {
            app.key(key(code)).unwrap();
        }
        assert_eq!(app.current().unwrap().name, "lib");
        app.key(key(KeyCode::Enter)).unwrap();
        app.key(key(KeyCode::Down)).unwrap();
        assert_eq!(app.current().unwrap().name, "shared");
        app.key(key(KeyCode::Char('d'))).unwrap();
        app.key(key(KeyCode::Enter)).unwrap();
        assert_eq!(
            app.dialog.as_ref().unwrap().action.as_ref().unwrap(),
            &["-Rssc", "--", "shared"]
        );
        assert!(app.session.is_none());
        app.key(key(KeyCode::Esc)).unwrap();
        app.key(key(KeyCode::Left)).unwrap();
        assert_eq!(app.current().unwrap().name, "lib");
        app.key(key(KeyCode::Left)).unwrap();
        assert_eq!(app.filtered, [0, 1, 2]);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                app.draw_list(
                    f,
                    Rect::new(0, 0, 60, 20),
                    " TREE ",
                    app.packages(),
                    &app.filtered,
                    app.package_selected,
                    true,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let heading: String = (0..60).map(|x| buffer[(x, 1)].symbol()).collect();
        assert!(heading.contains("VERSION") && !heading.contains("SOURCE"));
        let branch = buffer.content.iter().find(|c| c.symbol() == "├").unwrap();
        assert_eq!(branch.fg, DIM);
        app.query = "lib".into();
        app.reindex();
        app.package_selected = 0;
        assert_eq!(app.filtered, [1]);
        app.key(key(KeyCode::Right)).unwrap();
        assert_eq!(
            app.filtered,
            [1, 2],
            "search roots still expose nonmatching dependencies"
        );
        app.key(key(KeyCode::Tab)).unwrap();
        app.key(key(KeyCode::Char('d'))).unwrap();
        assert!(app.removal.is_none(), "inspector focus must not delete");
    }
    #[test]
    fn git_commit_updates_enable_devel_in_both_update_paths() {
        let mut app = App::new().unwrap();
        let mut package = search_result("aur");
        package.devel = true;
        package.next = Some("latest-commit".into());
        app.catalog.installed = vec![package];
        app.reindex();
        app.source = 1;
        app.update_one();
        assert!(app
            .dialog
            .as_ref()
            .unwrap()
            .action
            .as_ref()
            .unwrap()
            .contains(&"--devel".into()));
        app.update(false);
        assert!(app
            .dialog
            .as_ref()
            .unwrap()
            .action
            .as_ref()
            .unwrap()
            .contains(&"--devel".into()));
        app.update(true);
        assert!(app
            .dialog
            .as_ref()
            .unwrap()
            .action
            .as_ref()
            .unwrap()
            .contains(&"--devel".into()));
        app.source = 0;
        app.update(false);
        assert!(!app
            .dialog
            .as_ref()
            .unwrap()
            .action
            .as_ref()
            .unwrap()
            .contains(&"--devel".into()));
    }
    #[test]
    fn install_targets_the_selected_source_and_installed_enter_only_inspects() {
        let mut app = App::new().unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.catalog.installed = vec![search_result("aur")];
        app.key(key(KeyCode::Char('3'))).unwrap();
        app.key(key(KeyCode::Enter)).unwrap();
        assert_eq!(app.focus, 1);
        assert!(app.dialog.is_none());
        app.page = 4;
        app.focus = 0;
        for source in ["core", "aur"] {
            app.search_results = vec![search_result(source)];
            app.key(key(KeyCode::Enter)).unwrap();
            assert_eq!(
                app.dialog.as_ref().unwrap().action.as_ref().unwrap(),
                &[
                    "-S".to_owned(),
                    "--".to_owned(),
                    format!("{source}/example")
                ]
            );
            app.key(key(KeyCode::Esc)).unwrap();
        }
        app.search_results = vec![search_result("aur")];
        app.search_results[0].remote.as_mut().unwrap().target = Some("__aur__/example".into());
        app.key(key(KeyCode::Enter)).unwrap();
        assert_eq!(
            app.dialog
                .as_ref()
                .unwrap()
                .action
                .as_ref()
                .unwrap()
                .last()
                .unwrap(),
            "__aur__/example"
        );
        assert!(app.session.is_none());
    }
    #[test]
    fn stale_search_and_detail_events_cannot_overwrite_current_results() {
        let mut app = App::new().unwrap();
        app.search_id = 2;
        app.detail_pending = Some("search:2:aur/example".into());
        app.detail_requested.insert("search:2:aur/example".into());
        for id in [2, 1] {
            app.tx
                .send(Event::Search {
                    id,
                    packages: vec![search_result(if id == 2 { "core" } else { "aur" })],
                    done: true,
                    error: None,
                })
                .unwrap();
        }
        app.tx
            .send(Event::Inspection("search:1:aur/example".into(), vec![]))
            .unwrap();
        app.tick().unwrap();
        assert_eq!(app.search_results[0].source, "core");
        assert_eq!(app.search_indices, [0]);
        assert_eq!(app.detail_pending.as_deref(), Some("search:2:aur/example"));
        assert!(app.inspections.is_empty());
    }
    #[test]
    fn aur_warnings_stay_at_the_top_while_inspector_scrolls() {
        let mut app = App::new().unwrap();
        app.page = 4;
        app.settings.language = Language::ZhCn;
        app.search_results = vec![search_result("aur")];
        app.search_indices = vec![0];
        app.submitted_query = "example".into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 36)).unwrap();
        for scroll in [0, 15] {
            app.detail_scroll = scroll;
            terminal.draw(|f| app.draw(f)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .replace(' ', "");
            assert!(text.contains("无人维护（孤儿包）"));
            assert!(text.contains("已标记过期"));
            assert!(buffer
                .content
                .iter()
                .any(|c| c.symbol() == "孤" && c.fg == RED));
            assert!(!text.contains("仓库目录"));
        }
        assert_eq!(
            super::super::i18n::translate("Install example 2-1 from aur?", Language::ZhCn),
            "从 aur 安装 example 2-1？"
        );
    }
    #[test]
    fn supplemental_detail_text_and_help_have_semantic_colors() {
        let lines = detail_field(
            "Optional dependencies",
            "example: additional features",
            "",
            Language::En,
            70,
        );
        assert_eq!(lines[0].spans[1].style.fg, Some(TEXT));
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content == "a" && s.style.fg == Some(DIM)));
        let network = detail_field(
            "Network",
            "DIRECT (Git follows settings)",
            "",
            Language::En,
            70,
        );
        assert!(network[0]
            .spans
            .iter()
            .any(|s| s.content == "(" && s.style.fg == Some(DIM)));
        let help = help_lines("NAVIGATION\nTab  Next panel    Enter  Confirm");
        assert_eq!(help[0].spans[0].style.fg, Some(accent()));
        assert_eq!(help[1].spans[2].style.fg, Some(DIM));
        assert_eq!(help[1].spans[4].content, "Enter");
        assert_eq!(help[1].spans[4].style.fg, Some(accent()));
    }
    #[test]
    fn detail_values_wrap_under_value_column_and_keep_dependency_breaks() {
        for language in [Language::En, Language::ZhCn] {
            let value =
                "first: 一个很长的可选依赖说明 with more words\nsecond: another description";
            let lines = detail_field("Optional dependencies", value, "second", language, 38);
            assert!(lines.len() >= 4);
            for line in lines.iter().skip(1) {
                assert_eq!(line.spans[0].content, " ".repeat(17));
            }
            let rows: Vec<_> = lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            assert!(rows
                .iter()
                .any(|row| row.starts_with(&format!("{}second:", " ".repeat(17)))));
            assert!(!rows.iter().any(|row| row.contains('·')));
            assert!(lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.bg == Some(YELLOW)));
            let url = detail_field(
                "URL",
                "https://example.org/a/very/long/path/without/whitespace",
                "",
                language,
                38,
            );
            assert!(url.len() > 1);
            assert!(url.iter().all(|line| line.width() <= 38));
        }
    }
    #[test]
    fn modal_backdrop_dims_content_and_preserves_terminal_background() {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(20, 6)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(
                    Paragraph::new("background").style(style(TEXT).add_modifier(Modifier::BOLD)),
                    f.area(),
                );
                dim_backdrop(f);
                f.render_widget(
                    Paragraph::new("dialog").style(style(accent())),
                    Rect::new(2, 2, 10, 1),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, mix(TEXT, BORDER, 0.70));
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert!(!buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(2, 2)].fg, accent());
        assert_eq!(buffer[(2, 2)].bg, Color::Reset);
    }
    #[test]
    fn animated_selection_immediately_updates_operation_target() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.toast = None;
        app.catalog.installed = (0..1000)
            .map(|i| Package {
                devel: false,
                remote: None,
                name: format!("example-{i:04}"),
                base: format!("example-{i:04}"),
                source: "aur".into(),
                version: "1".into(),
                installed: Some("1".into()),
                next: Some("2".into()),
                description: String::new(),
                url: String::new(),
                size: 0,
                dependencies: Vec::new(),
                ignored: false,
                search: String::new(),
            })
            .collect();
        app.reindex();
        app.source = 1;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 36)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("2/1000"));
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.symbol() == "┃"));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.dialog
                .as_ref()
                .unwrap()
                .action
                .as_ref()
                .unwrap()
                .last()
                .unwrap(),
            "example-0001"
        );
    }
    #[test]
    fn search_highlights_unicode_without_changing_package_text() {
        let line = highlight("Foo-中文-foo", "FOO", style(TEXT));
        assert_eq!(
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "Foo-中文-foo"
        );
        assert_eq!(
            line.spans
                .iter()
                .filter(|s| s.style.bg == Some(YELLOW))
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            ["Foo", "foo"]
        );
        let line = highlight("包中文名", "中文", style(TEXT));
        assert!(line
            .spans
            .iter()
            .any(|s| s.content == "中文" && s.style.bg == Some(YELLOW)));
    }
    #[test]
    fn enter_only_targets_the_selected_aur_package() {
        let mut app = App::new().unwrap();
        app.catalog.installed.push(Package {
            devel: false,
            remote: None,
            name: "example-git".into(),
            base: "example".into(),
            source: "aur".into(),
            version: "1".into(),
            installed: Some("1".into()),
            next: Some("2".into()),
            description: String::new(),
            url: String::new(),
            size: 0,
            dependencies: Vec::new(),
            ignored: false,
            search: "example-git".into(),
        });
        app.query = "unrelated package search".into();
        app.reindex();
        app.source = 1;
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.dialog.as_ref().unwrap().action.as_ref().unwrap(),
            &["-S", "--aur", "--", "example-git"]
        );
        assert!(app.dialog.as_ref().unwrap().yes);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.session.is_none());
        app.key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE))
            .unwrap();
        assert!(app.dialog.is_none());
    }
    #[test]
    fn authentication_input_is_masked() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.dialog = Some(Dialog {
            title: "Authentication".into(),
            scroll: 0,
            body: "sudo password:".into(),
            question: Some(Question {
                text: "sudo password:".into(),
                secret: true,
                notice: false,
                build: None,
                download: None,
                default: None,
                plan: vec![],
            }),
            action: None,
            yes: false,
            input: "fixture-secret".into(),
        });
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!text.contains("fixture-secret"));
        assert!(text.contains("••••"));
        let buffer = terminal.backend().buffer();
        let top = (0..30)
            .find(|&y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .contains("AUTHENTICATION")
            })
            .unwrap();
        let left = (0..100)
            .find(|&x| buffer[(x, top)].symbol() == "╭")
            .unwrap();
        let bottom = (top + 1..30)
            .find(|&y| buffer[(left, y)].symbol() == "╰")
            .unwrap();
        assert_eq!(
            bottom - top + 1,
            10,
            "password dialog uses its dedicated compact layout"
        );
        app.dialog.as_mut().unwrap().body = "A long transaction plan\n".repeat(100);
        app.dialog
            .as_mut()
            .unwrap()
            .question
            .as_mut()
            .unwrap()
            .secret = false;
        app.dialog
            .as_mut()
            .unwrap()
            .question
            .as_mut()
            .unwrap()
            .default = Some(true);
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(12, 2)].symbol(), "╭");
        assert_eq!(buffer[(12, 27)].symbol(), "╰");
    }
    #[test]
    fn focused_output_does_not_trigger_updates_or_search() {
        let mut app = App::new().unwrap();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.key(key(KeyCode::Tab)).unwrap();
        assert_eq!((app.focus, app.source), (0, 1));
        app.key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.focus, 1);
        app.key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.focus, 2);
        for c in [
            KeyCode::Enter,
            KeyCode::Char('a'),
            KeyCode::Char('u'),
            KeyCode::Char('/'),
        ] {
            app.key(key(c)).unwrap();
        }
        assert!(app.dialog.is_none());
        assert!(!app.searching);
        app.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, 0);
        app.query = "does-not-match".into();
        app.key(key(KeyCode::Char('3'))).unwrap();
        app.key(key(KeyCode::Char('/'))).unwrap();
        assert!(app.searching);
    }
    #[test]
    fn tab_cycles_sources_and_panels_in_both_directions() {
        let mut app = App::new().unwrap();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        for expected in [(0, 1), (1, 1), (2, 1), (0, 0)] {
            app.key(key(KeyCode::Tab)).unwrap();
            assert_eq!((app.focus, app.source), expected);
        }
        for expected in [(2, 0), (1, 0), (0, 1), (0, 0)] {
            app.key(key(KeyCode::BackTab)).unwrap();
            assert_eq!((app.focus, app.source), expected);
        }
        app.key(key(KeyCode::Right)).unwrap();
        assert_eq!(app.source, 0);
        app.key(key(KeyCode::Char('3'))).unwrap();
        for expected in [1, 0, 1, 0] {
            app.key(key(KeyCode::Tab)).unwrap();
            assert_eq!(app.focus, expected);
        }
        app.key(key(KeyCode::Char('2'))).unwrap();
        app.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(app.page, 4);
        for expected in [1, 2, 0] {
            app.key(key(KeyCode::Tab)).unwrap();
            assert_eq!(app.focus, expected);
        }
        for expected in [2, 1, 0] {
            app.key(key(KeyCode::BackTab)).unwrap();
            assert_eq!(app.focus, expected);
        }
    }
    #[test]
    fn help_is_read_only_and_preserves_focus() {
        let mut app = App::new().unwrap();
        app.source = 1;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('?'))).unwrap();
        assert!(app.help_open);
        for c in [KeyCode::Enter, KeyCode::Char('a'), KeyCode::Tab] {
            app.key(key(c)).unwrap();
        }
        assert!(app.dialog.is_none());
        assert_eq!((app.focus, app.source), (0, 1));
        app.key(key(KeyCode::Esc)).unwrap();
        assert!(!app.help_open);
        assert_eq!(app.source, 1);
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(fit_text("中文软件包", 7).as_str()),
            7
        );
    }
    #[test]
    fn review_diff_colors_only_diff_content() {
        let lines=review_lines("Review changes for example:\n--- a/PKGBUILD\n+++ b/PKGBUILD\n@@ -1 +1 @@\n-old\n+new\n context");
        assert_eq!(lines[1].spans[0].style.fg, Some(BLUE));
        assert_eq!(lines[2].spans[0].style.fg, Some(BLUE));
        assert_eq!(lines[3].spans[0].style.fg, Some(accent()));
        assert_eq!(lines[4].spans[0].style.fg, Some(RED));
        assert_eq!(lines[5].spans[0].style.fg, Some(GREEN));
        assert_eq!(
            review_lines("Ordinary prompt\n+not a diff")[1].spans[0]
                .style
                .fg,
            Some(TEXT)
        );
    }
    #[test]
    fn notices_shrink_and_simplified_pages_have_no_status_or_system_panel() {
        let mut app = App::new().unwrap();
        app.note("Settings saved");
        app.toast.as_mut().unwrap().since = Instant::now() - Duration::from_secs(1);
        assert_eq!(app.toast.as_ref().unwrap().expansion(), 1.0);
        app.toast.as_mut().unwrap().since = Instant::now() - Duration::from_secs(5);
        assert_eq!(app.toast.as_ref().unwrap().expansion(), 0.0);
        app.settings.language = Language::En;
        for page in [0, 2, 3] {
            app.page = page;
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(!text.contains(" SYSTEM "));
            assert!(!text.contains(" STATUS "));
            assert!(!text.contains(" SEARCH "));
            if page == 0 {
                assert!(text.contains("BUILD OUTPUT"));
            }
            if page == 2 {
                assert!(!text.contains("settings.toml"));
                assert!(!text.contains("Existing operations"));
            }
        }
    }
    #[test]
    fn backend_progress_does_not_create_toasts() {
        for message in [
            "Download · linux.pkg.tar.zst · Init",
            "Download · linux.pkg.tar.zst · Completed { result: Success }",
            "Installing · linux · 50% · 1/21",
            "Checking dependencies",
        ] {
            assert!(Toast::backend(message.into()).is_none());
        }
        assert!(
            Toast::backend("ERROR: could not download package".into())
                .unwrap()
                .error
        );
        assert!(Toast::backend("Download failed".into()).unwrap().error);
    }
    #[test]
    fn localized_dialogs_keep_shell_and_build_content_intact() {
        let lines = localized_dialog_lines(
            "Review changes for test:\n\n-Version\n+Version\n\nAccept these changes?",
            Language::ZhCn,
        );
        assert!(lines[0].spans[0].content.contains("审阅"));
        assert_eq!(lines[3].spans[0].content, "+Version");
        assert!(lines.last().unwrap().spans[0].content.contains("接受"));
        let command = shell_line(&["-Syu".into(), "--".into(), "a b".into()]);
        let text: String = command.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "$ paru -Syu -- 'a b'");
        assert_eq!(command.spans[2].style.fg, Some(accent()));
        assert_eq!(command.spans[3].style.fg, Some(DIM));
        assert_eq!(log_level("Operation failed with exit code 1").0, "ERROR");
        assert_eq!(log_level("Warning: repository unavailable").0, "WARN");
        assert_eq!(log_level("Scan complete").0, "INFO");
        assert_eq!(
            super::super::i18n::translate("sudo: 3 incorrect password attempts", Language::ZhCn),
            "sudo 身份验证失败：密码错误 3 次"
        );
    }
    #[test]
    fn wrapped_logs_keep_body_indent_when_scrolled() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.page = 3;
        app.activity.clear();
        app.activity.push_back(format!(
            "12:34:56  Error: {}\nexplicit continuation",
            "long detail ".repeat(80)
        ));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        // Panel starts at (1, 4), with two cells of horizontal padding.
        assert_eq!(buffer[(4, 5)].symbol(), "1");
        assert_eq!(buffer[(14, 5)].symbol(), "E");
        for x in 4..21 {
            assert_eq!(buffer[(x, 6)].symbol(), " ");
        }
        assert_ne!(buffer[(21, 6)].symbol(), " ");
        app.activity_scroll = 1;
        terminal.draw(|f| app.draw(f)).unwrap();
        for x in 4..21 {
            assert_eq!(terminal.backend().buffer()[(x, 5)].symbol(), " ");
        }
        assert_ne!(terminal.backend().buffer()[(21, 5)].symbol(), " ");
    }
    #[test]
    fn confirmation_labels_stay_in_place_when_toggled() {
        for language in [Language::En, Language::ZhCn] {
            let mut positions = vec![];
            for yes in [true, false] {
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(40, 1)).unwrap();
                terminal
                    .draw(|f| {
                        f.render_widget(
                            Paragraph::new(confirmation_choice(yes, language)),
                            f.area(),
                        )
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let labels = if language == Language::En {
                    ["Y", "N"]
                } else {
                    ["是", "否"]
                };
                positions.push(
                    labels
                        .map(|label| (0..40).find(|&x| buffer[(x, 0)].symbol() == label).unwrap()),
                );
            }
            assert_eq!(positions[0], positions[1]);
        }
    }
    #[test]
    fn transaction_text_distinguishes_summary_packages_and_removals() {
        let dialog = Dialog {
            title: "TRANSACTION PLAN".into(), scroll: 0,
            body: "Install the prepared transaction?\nDownload size: 10.00 MiB\n\ncore/example  2.0\nREMOVE/old  1.0".into(),
            question: Some(Question { text: String::new(), secret: false, notice: false, build: None, download: None, default: Some(true),
                plan: vec![super::super::bridge::PlanItem { name: "example".into(), version: "2.0".into(), source: "core".into() }, super::super::bridge::PlanItem { name: "old".into(), version: "1.0".into(), source: "REMOVE".into() }] }),
            action: None, yes: true, input: String::new(),
        };
        let lines = dialog_lines(&dialog, Language::ZhCn);
        assert!(lines[0].spans[0].content.contains("事务"));
        assert_eq!(lines[1].spans[0].style.fg, Some(DIM));
        assert_eq!(lines[1].spans[1].style.fg, Some(accent()));
        assert_eq!(lines[3].spans[1].content, "example");
        assert_eq!(lines[3].spans[2].style.fg, Some(GREEN));
        assert_eq!(lines[4].spans[1].style.fg, Some(RED));
    }
    #[test]
    fn settings_details_follow_selection_and_scroll_rules() {
        let mut app = App::new().unwrap();
        app.settings.language = Language::En;
        app.settings.proxy_bases = (0..60).map(|i| format!("example-{i:02}")).collect();
        app.page = 2;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        let screen = |terminal: &Terminal<ratatui::backend::TestBackend>| {
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(screen(&terminal).contains("Packages using a proxy:"));
        app.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
            .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(app.detail_scroll > 0);
        assert_eq!(app.setting_selected, 0);
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(screen(&terminal).contains("clone / fetch / ls-remote"));
        assert!(!screen(&terminal).contains("Packages using a proxy:"));
        assert_eq!(app.detail_scroll, 0);
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(screen(&terminal).contains("Interface language"));
    }
    #[test]
    fn layouts_fit() {
        for (w, h) in [(62, 18), (100, 30), (150, 42)] {
            let mut a = App::new().unwrap();
            for page in 0..5 {
                a.page = page;
                if page == 4 {
                    a.search_results = vec![search_result("aur")];
                    a.search_indices = vec![0];
                }
                let mut t = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                t.draw(|f| a.draw(f)).unwrap();
            }
        }
    }
}
