//! Drafts captions and zooms from recorded input so the editor starts with
//! a callout at every click, drag and typing burst — you just fill in names.

use crate::project::{Anchor, Caption, Project, Zoom};
use crate::record::InputLog;

const DRAG_PX: f32 = 30.0;
const TYPING_GAP: f64 = 1.2;

pub fn draft_captions(project: &mut Project, log: &InputLog) {
    let off = project.cursor_offset;
    let mut events: Vec<Caption> = Vec::new();

    for click in &log.clicks {
        let release = log.releases.iter().find(|r| r.t >= click.t);
        let dragged = release.is_some_and(|r| (r.x - click.x).hypot(r.y - click.y) > DRAG_PX);
        let (action, start, end) = match release {
            Some(r) if dragged => ("Drag", click.t - 0.4, r.t + 0.8),
            _ => ("Click", click.t - 0.7, click.t + 1.3),
        };
        events.push(caption(action, start + off, end + off));
    }

    let mut burst: Option<(f64, f64)> = None;
    for &t in &log.keystrokes {
        burst = match burst {
            Some((s, e)) if t - e <= TYPING_GAP => Some((s, t)),
            Some((s, e)) => {
                events.push(caption("Type", s - 0.3 + off, e + 0.8 + off));
                Some((t, t))
            }
            None => Some((t, t)),
        };
    }
    if let Some((s, e)) = burst {
        events.push(caption("Type", s - 0.3 + off, e + 0.8 + off));
    }

    events.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Callouts never overlap: each one hands over to the next.
    for i in 1..events.len() {
        let next_start = events[i].start;
        let prev = &mut events[i - 1];
        if prev.end > next_start - 0.05 {
            prev.end = (next_start - 0.05).max(prev.start + 0.4);
        }
    }
    let duration = project.duration;
    events.retain_mut(|c| {
        c.start = c.start.max(0.0);
        c.end = c.end.min(duration);
        c.end - c.start > 0.2
    });
    project.captions.extend(events);
    project.sort();
}

/// Adds a gentle zoom around each cluster of captions.
pub fn draft_zooms(project: &mut Project, scale: f32) {
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for c in &project.captions {
        match spans.last_mut() {
            Some(last) if c.start - last.1 < 1.5 => last.1 = last.1.max(c.end),
            _ => spans.push((c.start, c.end)),
        }
    }
    for (s, e) in spans {
        project.zooms.push(Zoom {
            start: (s - 0.4).max(0.0),
            end: (e + 0.4).min(project.duration),
            scale,
            focus: Anchor::Cursor,
        });
    }
    project.sort();
}

fn caption(action: &str, start: f64, end: f64) -> Caption {
    Caption {
        start,
        end,
        action: action.into(),
        target: String::new(),
        description: String::new(),
        anchor: Anchor::Cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Click;
    use crate::video::VideoInfo;

    #[test]
    fn clicks_drags_and_typing_become_captions() {
        let info = VideoInfo { width: 100, height: 100, fps: 30.0, duration: 30.0, has_audio: false };
        let mut p = Project::new("x.mkv".into(), &info);
        let log = InputLog {
            clicks: vec![Click { t: 2.0, x: 0.0, y: 0.0 }, Click { t: 6.0, x: 0.0, y: 0.0 }],
            releases: vec![Click { t: 2.1, x: 1.0, y: 0.0 }, Click { t: 7.0, x: 200.0, y: 0.0 }],
            keystrokes: vec![10.0, 10.3, 10.6, 15.0],
            ..Default::default()
        };
        draft_captions(&mut p, &log);
        let actions: Vec<_> = p.captions.iter().map(|c| c.action.as_str()).collect();
        assert_eq!(actions, ["Click", "Drag", "Type", "Type"]);
        for w in p.captions.windows(2) {
            assert!(w[0].end <= w[1].start);
        }
    }
}
