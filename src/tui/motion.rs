use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Tween {
    from: f32,
    target: f32,
    since: Instant,
    duration: f32,
}
impl Tween {
    fn new(value: f32, now: Instant, duration: f32) -> Self {
        Self {
            from: value,
            target: value,
            since: now,
            duration,
        }
    }
    fn value(&self, now: Instant) -> f32 {
        let t = (now.duration_since(self.since).as_secs_f32() / self.duration).clamp(0.0, 1.0);
        self.from + (self.target - self.from) * (1.0 - (1.0 - t).powi(3))
    }
    fn set(&mut self, target: f32, now: Instant) {
        if self.target != target {
            self.from = self.value(now);
            self.target = target;
            self.since = now;
        }
    }
    fn active(&self, now: Instant) -> bool {
        self.from != self.target && now.duration_since(self.since).as_secs_f32() < self.duration
    }
}
struct ListMotion {
    cursor: Tween,
    scroll: Tween,
    len: usize,
    rows: usize,
    seen: Instant,
}
#[derive(Default)]
pub struct Motion {
    focus: HashMap<String, (Tween, Instant)>,
    lists: HashMap<String, ListMotion>,
}
pub struct ListView {
    pub start: usize,
    pub scroll: f32,
    pub cursor: f32,
}
impl Motion {
    pub fn focus(&mut self, key: &str, active: bool, now: Instant) -> f32 {
        let value = if active { 1.0 } else { 0.0 };
        self.position(key, value, now)
    }
    pub fn position(&mut self, key: &str, value: f32, now: Instant) -> f32 {
        let (tween, seen) = self
            .focus
            .entry(key.into())
            .or_insert_with(|| (Tween::new(value, now, 0.11), now));
        *seen = now;
        tween.set(value, now);
        tween.value(now)
    }
    pub fn list(
        &mut self,
        key: &str,
        selected: usize,
        len: usize,
        rows: usize,
        now: Instant,
    ) -> ListView {
        let selected = selected.min(len.saturating_sub(1));
        let initial = viewport(0, selected, len, rows);
        let state = self.lists.entry(key.into()).or_insert_with(|| ListMotion {
            cursor: Tween::new(selected as f32, now, 0.14),
            scroll: Tween::new(initial as f32, now, 0.14),
            len,
            rows,
            seen: now,
        });
        if state.len != len || state.rows != rows {
            state.cursor = Tween::new(selected as f32, now, 0.14);
            state.scroll = Tween::new(initial as f32, now, 0.14);
            state.len = len;
            state.rows = rows;
        }
        state.seen = now;
        state.cursor.set(selected as f32, now);
        let target = viewport(state.scroll.target as usize, selected, len, rows);
        state.scroll.set(target as f32, now);
        ListView {
            start: (state.scroll.value(now).round() as usize).min(len.saturating_sub(rows)),
            scroll: state
                .scroll
                .value(now)
                .clamp(0.0, len.saturating_sub(rows) as f32),
            cursor: state.cursor.value(now),
        }
    }
    pub fn reset_lists(&mut self) {
        self.lists.clear();
    }
    pub fn prune(&mut self, now: Instant) {
        self.focus
            .retain(|_, (_, seen)| now.duration_since(*seen) < Duration::from_secs(2));
        self.lists
            .retain(|_, state| now.duration_since(state.seen) < Duration::from_secs(2));
    }
    pub fn active(&self, now: Instant) -> bool {
        self.focus.values().any(|(t, _)| t.active(now))
            || self
                .lists
                .values()
                .any(|s| s.cursor.active(now) || s.scroll.active(now))
    }
}
fn viewport(current: usize, selected: usize, len: usize, rows: usize) -> usize {
    let margin = 3.min(rows.saturating_sub(1) / 2);
    let desired = if selected < current + margin {
        selected.saturating_sub(margin)
    } else if selected >= current + rows.saturating_sub(margin) {
        (selected + margin + 1).saturating_sub(rows)
    } else {
        current
    };
    desired.min(len.saturating_sub(rows))
}
