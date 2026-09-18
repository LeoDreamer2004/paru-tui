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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn movement_eases_out_instead_of_using_constant_speed() {
        let now = Instant::now();
        let mut tween = Tween::new(0.0, now, 0.14);
        tween.set(1.0, now);
        let middle = tween.value(now + Duration::from_millis(70));
        assert!((middle - 0.875).abs() < 0.0001);
        assert_eq!(tween.value(now + Duration::from_millis(140)), 1.0);
    }
    #[test]
    fn rapid_input_retargets_from_current_position_and_settles() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.list("installed", 0, 1000, 20, now);
        motion.list("installed", 20, 1000, 20, now);
        let middle = motion.list("installed", 20, 1000, 20, now + Duration::from_millis(40));
        assert!(middle.cursor > 0.0 && middle.cursor < 20.0);
        let retarget = motion.list("installed", 4, 1000, 20, now + Duration::from_millis(40));
        assert_eq!(retarget.cursor, middle.cursor);
        let settled = motion.list("installed", 4, 1000, 20, now + Duration::from_millis(250));
        assert_eq!(settled.cursor, 4.0);
        assert!(!motion.active(now + Duration::from_millis(250)));
    }
    #[test]
    fn viewport_keeps_context_and_handles_resize_empty_and_end() {
        assert_eq!(viewport(0, 17, 100, 20), 1);
        assert_eq!(viewport(30, 31, 100, 20), 28);
        assert_eq!(viewport(80, 99, 100, 20), 80);
        let now = Instant::now();
        let mut motion = Motion::default();
        assert_eq!(motion.list("x", 99, 100, 20, now).start, 80);
        assert_eq!(motion.list("x", 0, 0, 0, now).start, 0);
        assert_eq!(motion.list("x", 99, 100, 10, now).start, 90);
    }
    #[test]
    fn cursor_and_viewport_share_the_same_motion_when_scrolling() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.list("packages", 16, 1000, 20, now);
        motion.list("packages", 17, 1000, 20, now);
        for ms in [16, 32, 48, 64, 96, 140] {
            let view = motion.list("packages", 17, 1000, 20, now + Duration::from_millis(ms));
            assert!((view.cursor - view.scroll - 16.0).abs() < 0.0001);
        }
    }
}
