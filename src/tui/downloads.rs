use super::bridge::DownloadEvent;
use super::theme::{accent, BORDER, GREEN, RED};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Downloads {
    files: BTreeMap<String, Transfer>,
    expected: u64,
}
struct Transfer {
    downloaded: u64,
    total: u64,
    done: bool,
    failed: bool,
    started: Instant,
    updated: Instant,
    samples: VecDeque<(Instant, u64)>,
}
impl Transfer {
    fn new(now: Instant) -> Self {
        Self {
            downloaded: 0,
            total: 0,
            done: false,
            failed: false,
            started: now,
            updated: now,
            samples: VecDeque::new(),
        }
    }

    fn rate(&self, now: Instant) -> f64 {
        if self.done || now.duration_since(self.updated) > Duration::from_secs(3) {
            return 0.0;
        }
        let (Some(&(first_at, first_bytes)), Some(&(last_at, last_bytes))) =
            (self.samples.front(), self.samples.back())
        else {
            return 0.0;
        };
        let elapsed = last_at.duration_since(first_at).as_secs_f64();
        if elapsed < 0.1 {
            return 0.0;
        }
        last_bytes.saturating_sub(first_bytes) as f64 / elapsed
    }

    fn sample(&mut self, now: Instant, downloaded: u64) {
        if downloaded < self.downloaded || self.done {
            self.samples.clear();
        }
        self.samples.push_back((now, downloaded));
        // A short rolling window follows changes in speed without jumping on each callback.
        while self.samples.len() > 2
            && now.duration_since(self.samples[1].0) >= Duration::from_secs(4)
        {
            self.samples.pop_front();
        }
    }
}
impl Downloads {
    pub fn update(&mut self, event: DownloadEvent) {
        self.update_at(event, Instant::now());
    }

    fn update_at(&mut self, event: DownloadEvent, now: Instant) {
        if let DownloadEvent::Start { total } = event {
            self.files.clear();
            self.expected = total;
            return;
        }
        let file = match &event {
            DownloadEvent::Init { file }
            | DownloadEvent::Progress { file, .. }
            | DownloadEvent::Retry { file, .. }
            | DownloadEvent::Completed { file, .. } => file,
            DownloadEvent::Start { .. } => unreachable!(),
        };
        // Package signatures are tiny auxiliary requests, not separate progress rows.
        if file.ends_with(".sig") {
            return;
        }
        let transfer = self
            .files
            .entry(file.clone())
            .or_insert_with(|| Transfer::new(now));
        transfer.updated = now;
        match event {
            DownloadEvent::Init { .. } => *transfer = Transfer::new(now),
            DownloadEvent::Progress {
                downloaded, total, ..
            } => {
                transfer.sample(now, downloaded);
                transfer.downloaded = downloaded;
                transfer.total = total;
                transfer.done = false;
                transfer.failed = false;
            }
            DownloadEvent::Retry { resume, .. } => {
                transfer.samples.clear();
                if !resume {
                    transfer.downloaded = 0;
                }
                transfer.started = now;
                transfer.done = false;
                transfer.failed = false;
            }
            DownloadEvent::Completed { total, failed, .. } => {
                transfer.samples.clear();
                transfer.total = total.max(transfer.total);
                if !failed {
                    transfer.downloaded = transfer.total;
                }
                transfer.done = true;
                transfer.failed = failed;
            }
            DownloadEvent::Start { .. } => unreachable!(),
        }
    }
    pub fn visible(&self) -> bool {
        !self.files.is_empty() || self.expected > 0
    }
    pub fn draw(&self, f: &mut Frame, area: Rect, chinese: bool) {
        let now = Instant::now();
        let downloaded: u64 = self.files.values().map(|t| t.downloaded).sum();
        let total = self
            .expected
            .max(self.files.values().map(|t| t.total).sum());
        let completed = self.files.values().filter(|t| t.done && !t.failed).count();
        let active = self.files.values().filter(|t| !t.done).count();
        let rate: f64 = self.files.values().map(|t| t.rate(now)).sum();
        let summary = if chinese {
            format!("已完成 {completed} · 正在下载 {active}")
        } else {
            format!("{completed} completed · {active} downloading")
        };
        let mut lines = vec![Line::from(Span::styled(
            summary,
            Style::default().fg(accent()),
        ))];
        lines.push(progress_line(
            downloaded,
            total,
            rate,
            active > 0,
            area.width as usize,
        ));
        lines.push(Line::default());
        // Keep a bounded viewport: active/failed downloads first, then latest completions.
        let mut rows: Vec<_> = self.files.iter().collect();
        rows.sort_by_key(|(_, t)| {
            (
                t.done && !t.failed,
                std::cmp::Reverse(if t.done { t.updated } else { t.started }),
            )
        });
        let limit = area.height.saturating_sub(3) as usize / 2;
        for (file, t) in rows.iter().take(limit) {
            let (mark, color) = if t.failed {
                ("×", RED)
            } else if t.done {
                ("✓", GREEN)
            } else {
                ("↓", accent())
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{mark} "), Style::default().fg(color)),
                Span::raw(file.to_string()),
            ]));
            lines.push(progress_line(
                t.downloaded,
                t.total,
                t.rate(now),
                !t.done,
                area.width as usize,
            ));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}
fn size(bytes: f64) -> String {
    if bytes >= 1_073_741_824.0 {
        format!("{:.1} GiB", bytes / 1_073_741_824.0)
    } else if bytes >= 1_048_576.0 {
        format!("{:.1} MiB", bytes / 1_048_576.0)
    } else {
        format!("{:.0} KiB", bytes / 1024.0)
    }
}
fn eta_seconds(downloaded: u64, total: u64, rate: f64) -> Option<u64> {
    if total == 0 || rate <= 0.0 {
        return None;
    }
    Some((total.saturating_sub(downloaded) as f64 / rate).ceil() as u64)
}

fn format_eta(seconds: u64) -> String {
    if seconds >= 360_000 {
        ">99h".into()
    } else if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn progress_line(
    downloaded: u64,
    total: u64,
    rate: f64,
    active: bool,
    width: usize,
) -> Line<'static> {
    let fraction = if total == 0 {
        0.0
    } else {
        (downloaded as f64 / total as f64).clamp(0.0, 1.0)
    };
    let total_text = if total == 0 {
        "?".into()
    } else {
        size(total as f64)
    };
    let suffix = format!(
        " {:3.0}% {}/{}  {}/s",
        fraction * 100.0,
        size(downloaded as f64),
        total_text,
        size(rate)
    );
    let eta = if active {
        Some(format!(
            "  ETA {}",
            eta_seconds(downloaded, total, rate).map_or_else(|| "--:--".into(), format_eta)
        ))
    } else {
        None
    };
    let eta = eta.filter(|eta| width >= 2 + suffix.len() + eta.len() + 8);
    let length = width
        .saturating_sub(suffix.len() + eta.as_ref().map_or(0, String::len) + 2)
        .min(48);
    let filled = (fraction * length as f64).round() as usize;
    Line::from(
        vec![
            Span::raw("  "),
            Span::styled("━".repeat(filled), Style::default().fg(accent())),
            Span::styled("━".repeat(length - filled), Style::default().fg(BORDER)),
            Span::raw(suffix),
        ]
        .into_iter()
        .chain(eta.map(|eta| Span::styled(eta, Style::default().fg(accent()))))
        .collect::<Vec<_>>(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_uses_recent_bytes_for_parallel_and_resumed_downloads() {
        let mut downloads = Downloads::default();
        let start = Instant::now();
        downloads.update_at(DownloadEvent::Start { total: 4000 }, start);
        for (file, downloaded, total) in [("a", 1000, 2000), ("b", 500, 2000)] {
            downloads.update_at(
                DownloadEvent::Progress {
                    file: file.into(),
                    downloaded,
                    total,
                },
                start,
            );
        }
        let later = start + Duration::from_secs(1);
        downloads.update_at(
            DownloadEvent::Progress {
                file: "a".into(),
                downloaded: 1500,
                total: 2000,
            },
            later,
        );
        downloads.update_at(
            DownloadEvent::Progress {
                file: "b".into(),
                downloaded: 750,
                total: 2000,
            },
            later,
        );
        assert_eq!(downloads.files["a"].rate(later), 500.0);
        assert_eq!(downloads.files["b"].rate(later), 250.0);
        assert_eq!(eta_seconds(2250, 4000, 750.0), Some(3));
        assert_eq!(eta_seconds(1500, 2000, 500.0), Some(1));

        downloads.update_at(
            DownloadEvent::Retry {
                file: "a".into(),
                resume: true,
            },
            later,
        );
        assert_eq!(downloads.files["a"].rate(later), 0.0);
        downloads.update_at(
            DownloadEvent::Progress {
                file: "a".into(),
                downloaded: 1500,
                total: 2000,
            },
            later + Duration::from_secs(1),
        );
        assert_eq!(
            downloads.files["a"].rate(later + Duration::from_secs(1)),
            0.0
        );
        downloads.update_at(
            DownloadEvent::Progress {
                file: "a".into(),
                downloaded: 1600,
                total: 2000,
            },
            later + Duration::from_secs(2),
        );
        assert_eq!(
            downloads.files["a"].rate(later + Duration::from_secs(2)),
            100.0
        );
        assert_eq!(
            downloads.files["a"].rate(later + Duration::from_secs(6)),
            0.0
        );
    }

    #[test]
    fn progress_line_shows_eta_when_it_fits() {
        let line = progress_line(1024, 2048, 256.0, true, 80);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("ETA 00:04"));
        let narrow = progress_line(1024, 2048, 256.0, true, 35);
        let text: String = narrow
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!text.contains("ETA"));
        assert_eq!(format_eta(3661), "1:01:01");
    }

    #[test]
    fn parallel_downloads_retry_and_completion_preserve_totals() {
        let mut d = Downloads::default();
        d.update(DownloadEvent::Start { total: 300 });
        for (file, downloaded, total) in [("a", 50, 100), ("b", 20, 200)] {
            d.update(DownloadEvent::Progress {
                file: file.into(),
                downloaded,
                total,
            });
        }
        d.update(DownloadEvent::Retry {
            file: "a".into(),
            resume: false,
        });
        assert_eq!(d.files["a"].downloaded, 0);
        assert_eq!(d.files["b"].downloaded, 20);
        d.update(DownloadEvent::Completed {
            file: "a".into(),
            total: 100,
            failed: false,
        });
        assert_eq!(d.files["a"].downloaded, 100);
        d.update(DownloadEvent::Completed {
            file: "b".into(),
            total: 200,
            failed: true,
        });
        assert_eq!(d.files["b"].downloaded, 20);
        assert!(d.files["b"].failed);
        d.update(DownloadEvent::Init {
            file: "a.sig".into(),
        });
        assert_eq!(d.files.len(), 2);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| d.draw(f, f.area(), false)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("1 completed"));
        assert!(text.contains('×'));
        assert!(text.contains('━'));
        d.update(DownloadEvent::Start { total: 500 });
        assert!(d.files.is_empty());
        assert_eq!(d.expected, 500);
    }
}
