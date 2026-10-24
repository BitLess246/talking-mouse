//! Turns a source frame plus the project into a finished output frame:
//! camera zoom, click ripples, chapter cards, cursor callouts, watermark.
//!
//! Every overlay is a pure function of `t`, so the editor preview can jump
//! anywhere and still match the export exactly.

use tiny_skia::{
    Color, FillRule, FilterQuality, IntRect, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use crate::project::{Anchor, Caption, Project};
use crate::text::{fonts, TextStyle};

/// Seconds for overlays to animate in/out.
const CALLOUT_IN: f64 = 0.22;
const CALLOUT_OUT: f64 = 0.18;
const CHAPTER_IN: f64 = 0.55;
const CHAPTER_OUT: f64 = 0.3;
const ZOOM_RAMP: f64 = 0.7;
const RIPPLE: f64 = 0.55;
/// Narration typing speed, characters per second.
const TYPE_RATE: f64 = 38.0;

/// The visible region of the source video, in source pixels.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Maps source-video pixels to output pixels for one frame.
#[derive(Clone, Copy)]
struct Camera {
    view: Viewport,
    k: f32,
}

impl Camera {
    fn map(&self, (x, y): (f32, f32)) -> (f32, f32) {
        ((x - self.view.x) * self.k, (y - self.view.y) * self.k)
    }
}

pub fn viewport_at(p: &Project, t: f64) -> Viewport {
    let (sw, sh) = (p.width as f32, p.height as f32);
    let mut best: Option<(f32, f32, Anchor)> = None;
    for z in &p.zooms {
        let env = envelope(t, z.start, z.end, ZOOM_RAMP, ZOOM_RAMP);
        if env > 0.0 && best.is_none_or(|(e, _, _)| env > e) {
            best = Some((env, z.scale.max(1.0), z.focus));
        }
    }
    let Some((env, scale, focus)) = best else {
        return Viewport { x: 0.0, y: 0.0, w: sw, h: sh };
    };
    let env = smootherstep(env);
    let zoom = 1.0 + (scale - 1.0) * env;
    let (w, h) = (sw / zoom, sh / zoom);
    // The camera trails the cursor more lazily than callouts do.
    let (fx, fy) = p.anchor_at(focus, t, 0.45);
    let cx = sw * 0.5 + (fx - sw * 0.5) * env;
    let cy = sh * 0.5 + (fy - sh * 0.5) * env;
    let x = (cx - w * 0.5).clamp(0.0, sw - w);
    let y = (cy - h * 0.5).clamp(0.0, sh - h);
    Viewport { x, y, w, h }
}

/// Renders the frame at `t`. `src` may be any resolution (the editor decodes
/// at preview size); the output is `out_w`×`out_h`.
pub fn compose(p: &Project, t: f64, src: &Pixmap, out_w: u32, out_h: u32) -> Pixmap {
    let mut out = Pixmap::new(out_w, out_h).expect("non-zero output size");
    out.fill(Color::BLACK);

    let view = viewport_at(p, t);
    let cam = Camera { view, k: out_w as f32 / view.w };
    // Source frame pixels -> source-video pixels -> output pixels.
    let f = src.width() as f32 / p.width as f32;
    let k = cam.k / f;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    let transform = Transform::from_row(k, 0.0, 0.0, k, -view.x * cam.k, -view.y * cam.k);
    out.draw_pixmap(0, 0, src.as_ref(), &paint, transform, None);

    let u = out_h as f32 / 1080.0 * p.style.ui_scale.max(0.1);

    if p.style.click_ripples {
        draw_ripples(&mut out, p, t, cam, u);
    }
    for (i, c) in p.chapters.iter().enumerate() {
        if t >= c.start && t < c.end + CHAPTER_OUT {
            draw_chapter(&mut out, p, i, t, u);
        }
    }
    for c in &p.captions {
        if t >= c.start && t < c.end + CALLOUT_OUT {
            draw_callout(&mut out, p, c, t, cam, u);
        }
    }
    if !p.style.watermark.trim().is_empty() {
        draw_watermark(&mut out, p, u);
    }
    out
}

/// Wraps tightly packed RGBA bytes (as decoded by ffmpeg) in a pixmap.
pub fn pixmap_from_rgba(data: Vec<u8>, w: u32, h: u32) -> Pixmap {
    let size = tiny_skia::IntSize::from_wh(w, h).expect("non-zero frame size");
    // Decoded video is opaque, so straight and premultiplied alpha agree.
    Pixmap::from_vec(data, size).expect("frame buffer matches its size")
}

// ---------------------------------------------------------------------------
// Callout

struct Sprite {
    pixmap: Pixmap,
    /// Offset of the box's top-left corner inside the sprite (shadow room).
    margin: f32,
    w: f32,
    h: f32,
}

fn callout_sprite(p: &Project, c: &Caption, u: f32) -> Option<Sprite> {
    let fonts = fonts();
    let ink = rgb(p.style.ink, 255);
    let title = TextStyle::new(&fonts.bold, 24.0 * u);
    let chevron = TextStyle::new(&fonts.bold, 19.0 * u);
    let desc = TextStyle::new(&fonts.medium, 18.0 * u);

    let action = c.action.trim();
    let target = c.target.trim();
    let gap = 10.0 * u;
    let mut title_w = 0.0;
    if !action.is_empty() {
        title_w += title.width(action);
    }
    if !action.is_empty() && !target.is_empty() {
        title_w += gap * 2.0 + chevron.width("›");
    }
    if !target.is_empty() {
        title_w += title.width(target);
    }

    let max_desc = 520.0 * u;
    let lines = if c.description.trim().is_empty() {
        Vec::new()
    } else {
        desc.wrap(c.description.trim(), max_desc.max(title_w))
    };
    if title_w == 0.0 && lines.is_empty() {
        return None;
    }

    let pad_x = 16.0 * u;
    let pad_y = if lines.is_empty() { 10.0 * u } else { 13.0 * u };
    let title_lh = 30.0 * u;
    let desc_lh = 25.0 * u;
    let content_w = lines.iter().map(|l| desc.width(l)).fold(title_w, f32::max);
    let w = (content_w + pad_x * 2.0).ceil();
    let title_h = if title_w > 0.0 { title_lh } else { 0.0 };
    let h =
        (pad_y * 2.0 + title_h + desc_lh * lines.len() as f32 + if lines.is_empty() { 0.0 } else { 2.0 * u }).ceil();

    let blur = 14.0 * u;
    let margin = (blur * 2.0 + 6.0 * u).ceil();
    let mut pm = Pixmap::new((w + margin * 2.0) as u32, (h + margin * 2.0) as u32)?;
    let radius = 7.0 * u;
    draw_shadow(&mut pm, margin, margin + 6.0 * u, w, h, radius, blur, [0, 0, 0, 80]);
    fill_rrect(&mut pm, margin, margin, w, h, radius, rgb(p.style.accent, 255));

    let x0 = margin + pad_x;
    let mut y = margin + pad_y;
    if title_w > 0.0 {
        let baseline = y + (title_lh + title.cap_height()) * 0.5;
        let mut x = x0;
        if !action.is_empty() {
            x = title.draw(&mut pm, action, x, baseline, ink);
        }
        if !action.is_empty() && !target.is_empty() {
            let cb = baseline - (title.cap_height() - chevron.cap_height()) * 0.5;
            x = chevron.draw(&mut pm, "›", x + gap, cb, rgb(p.style.ink, 140)) + gap;
        }
        if !target.is_empty() {
            title.draw(&mut pm, target, x, baseline, ink);
        }
        y += title_lh + 2.0 * u;
    }
    for line in &lines {
        let baseline = y + (desc_lh + desc.cap_height()) * 0.5;
        desc.draw(&mut pm, line, x0, baseline, rgb(p.style.ink, 235));
        y += desc_lh;
    }
    Some(Sprite { pixmap: pm, margin, w, h })
}

fn draw_callout(out: &mut Pixmap, p: &Project, c: &Caption, t: f64, cam: Camera, u: f32) {
    let Some(sprite) = callout_sprite(p, c, u) else { return };
    let env = envelope(t, c.start, c.end, CALLOUT_IN, CALLOUT_OUT);
    if env <= 0.0 {
        return;
    }
    let appearing = t < c.start + CALLOUT_IN;
    let (ax, ay) = cam.map(p.anchor_at(c.anchor, t, p.style.follow_lag));
    let (ow, oh) = (out.width() as f32, out.height() as f32);
    let (w, h) = (sprite.w, sprite.h);

    // Sit just above and to the left of the pointer, flipping when that would
    // leave the frame.
    let (dx, dy) = (22.0 * u, 16.0 * u);
    let mut bx = ax - dx - w;
    let mut by = ay - dy - h;
    let mut pivot = (bx + w, by + h);
    if bx < 12.0 * u {
        bx = ax + dx;
        pivot.0 = bx;
    }
    if by < 12.0 * u {
        by = ay + dy + 18.0 * u;
        pivot.1 = by;
    }
    let edge = 12.0 * u;
    bx = bx.clamp(edge, (ow - w - edge).max(edge));
    by = by.clamp(edge, (oh - h - edge).max(edge));

    let e = ease_out_back(env);
    let s = if appearing { 0.86 + 0.14 * e } else { 0.96 + 0.04 * env };
    let lift = (1.0 - env) * 6.0 * u;
    let m = sprite.margin;
    let transform = Transform::from_translate(bx - m, by - m + lift)
        .post_translate(-pivot.0, -pivot.1)
        .post_scale(s, s)
        .post_translate(pivot.0, pivot.1);
    let paint = PixmapPaint { opacity: env.min(1.0), quality: FilterQuality::Bilinear, ..Default::default() };
    out.draw_pixmap(0, 0, sprite.pixmap.as_ref(), &paint, transform, None);
}

// ---------------------------------------------------------------------------
// Chapter lower-third

fn draw_chapter(out: &mut Pixmap, p: &Project, index: usize, t: f64, u: f32) {
    let c = &p.chapters[index];
    let fonts = fonts();
    let number = p.chapter_number(index);
    let num_style = TextStyle::new(&fonts.heavy, 36.0 * u);
    let title = TextStyle::new(&fonts.heavy, 34.0 * u).tracking(0.3 * u);
    let sub = TextStyle::new(&fonts.mono, 14.0 * u).tracking(2.4 * u);

    let title_text = c.title.to_uppercase();
    let sub_text = c.subtitle.to_uppercase();
    let badge = 76.0 * u;
    let pad_l = 21.0 * u;
    let pad_r = 26.0 * u;
    let panel_w = pad_l + title.width(&title_text).max(sub.width(&sub_text)) + pad_r;
    let card_w = (badge + panel_w).ceil();
    let card_h = badge.ceil();

    let Some(mut card) = Pixmap::new(card_w as u32, card_h as u32) else { return };
    fill_rrect(&mut card, badge - 1.0, 0.0, panel_w + 1.0, badge, 0.0, rgb(p.style.panel, 236));
    fill_rrect(&mut card, 0.0, 0.0, badge, badge, 0.0, rgb(p.style.accent, 255));
    let nw = num_style.width(&number);
    let nb = (badge + num_style.cap_height()) * 0.5;
    num_style.draw(&mut card, &number, (badge - nw) * 0.5, nb, rgb(p.style.ink, 255));
    let x = badge + pad_l;
    if sub_text.is_empty() {
        title.draw(&mut card, &title_text, x, (badge + title.cap_height()) * 0.5, rgb(p.style.panel_text, 255));
    } else {
        title.draw(&mut card, &title_text, x, 40.0 * u, rgb(p.style.panel_text, 255));
        sub.draw(&mut card, &sub_text, x + 1.0 * u, 63.0 * u, rgb(p.style.panel_muted, 255));
    }

    let left = 56.0 * u;
    let top = out.height() as f32 - 78.0 * u - badge;
    let enter = ease_out_cubic(((t - c.start) / CHAPTER_IN).clamp(0.0, 1.0) as f32);
    let exit = if t > c.end { 1.0 - ((t - c.end) / CHAPTER_OUT).clamp(0.0, 1.0) as f32 } else { 1.0 };

    // Badge pops in first, then the panel wipes open from behind it.
    let wipe = ease_out_cubic((((t - c.start) / CHAPTER_IN) * 1.4 - 0.25).clamp(0.0, 1.0) as f32);
    let visible = (badge + (card_w - badge) * wipe).ceil().max(1.0) as u32;
    let shadow_blur = 18.0 * u;
    draw_shadow(
        out,
        left,
        top + 8.0 * u,
        visible as f32,
        card_h,
        0.0,
        shadow_blur,
        [0, 0, 0, (70.0 * exit * enter) as u8],
    );
    if let Some(clip) =
        IntRect::from_xywh(0, 0, visible.min(card.width()), card.height()).and_then(|r| card.clone_rect(r))
    {
        let slide = (1.0 - enter) * -24.0 * u;
        let paint = PixmapPaint { opacity: (enter * exit).min(1.0), ..Default::default() };
        out.draw_pixmap(0, 0, clip.as_ref(), &paint, Transform::from_translate(left + slide, top), None);
    }

    if !c.narration.trim().is_empty() {
        let typed = (((t - c.start - 0.35) * TYPE_RATE).max(0.0)) as usize;
        let shown: String = c.narration.trim().chars().take(typed).collect();
        if !shown.is_empty() {
            let style = TextStyle::new(&fonts.mono, 18.0 * u);
            let full_w = style.width(&shown);
            let (px, py) = (12.0 * u, 9.0 * u);
            let bh = 18.0 * u + py * 2.0;
            let by = top - 18.0 * u - bh;
            fill_rrect(out, left, by, full_w + px * 2.0, bh, 6.0 * u, rgb(p.style.panel, (215.0 * exit) as u8));
            let baseline = by + (bh + style.cap_height()) * 0.5;
            let end = style.draw(out, &shown, left + px, baseline, rgb(p.style.panel_text, (225.0 * exit) as u8));
            // Blinking caret while typing.
            let typing = shown.chars().count() < c.narration.trim().chars().count();
            if typing || (t * 2.0).fract() < 0.5 {
                fill_rrect(
                    out,
                    end + 2.0 * u,
                    baseline - style.cap_height(),
                    9.0 * u,
                    style.cap_height() + 3.0 * u,
                    0.0,
                    rgb(p.style.accent, (230.0 * exit) as u8),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Watermark and click ripples

fn draw_watermark(out: &mut Pixmap, p: &Project, u: f32) {
    let style = TextStyle::new(&fonts().mono, 14.0 * u).tracking(3.0 * u);
    let text = p.style.watermark.trim().to_uppercase();
    let h = 36.0 * u;
    let dot = 5.0 * u;
    let pad = 16.0 * u;
    let w = pad + dot * 2.0 + 10.0 * u + style.width(&text) + pad;
    let x = out.width() as f32 - 36.0 * u - w;
    let y = out.height() as f32 - 35.0 * u - h;
    fill_rrect(out, x, y, w, h, h * 0.5, rgb(p.style.panel, 235));
    if let Some(circle) = PathBuilder::from_circle(x + pad + dot, y + h * 0.5, dot) {
        fill_path(out, &circle, rgb(p.style.accent, 255));
    }
    let baseline = y + (h + style.cap_height()) * 0.5;
    style.draw(out, &text, x + pad + dot * 2.0 + 10.0 * u, baseline, rgb(p.style.panel_text, 255));
}

fn draw_ripples(out: &mut Pixmap, p: &Project, t: f64, cam: Camera, u: f32) {
    let scale = p.cursor_scale as f32;
    for click in &p.clicks {
        let age = t - (click.t + p.cursor_offset);
        if !(0.0..RIPPLE).contains(&age) {
            continue;
        }
        let k = ease_out_cubic((age / RIPPLE) as f32);
        let (x, y) = cam.map((click.x * scale, click.y * scale));
        let r = (8.0 + 30.0 * k) * u;
        let Some(circle) = PathBuilder::from_circle(x, y, r) else { continue };
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        let [cr, cg, cb] = p.style.accent;
        paint.set_color_rgba8(cr, cg, cb, (200.0 * (1.0 - k)) as u8);
        let stroke = Stroke { width: 3.5 * u * (1.0 - 0.5 * k), ..Default::default() };
        out.stroke_path(&circle, &paint, &stroke, Transform::identity(), None);
    }
}

// ---------------------------------------------------------------------------
// Drawing helpers

fn rgb(c: [u8; 3], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

fn rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    // Cubic approximation of a quarter circle.
    let c = r * (1.0 - 0.552_284_8);
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - c, y, x + w, y + c, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - c, x + w - c, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + c, y + h, x, y + h - c, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + c, x + c, y, x + r, y);
    pb.close();
    pb.finish()
}

fn fill_path(pm: &mut Pixmap, path: &Path, rgba: [u8; 4]) {
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn fill_rrect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, rgba: [u8; 4]) {
    if rgba[3] == 0 {
        return;
    }
    if let Some(path) = rrect_path(x, y, w, h, r) {
        fill_path(pm, &path, rgba);
    }
}

/// Soft drop shadow: a rounded rect's coverage, box-blurred three times
/// (≈ gaussian), tinted and blended under whatever is drawn next.
#[allow(clippy::too_many_arguments)]
fn draw_shadow(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, blur: f32, rgba: [u8; 4]) {
    if rgba[3] == 0 || w <= 0.0 || h <= 0.0 {
        return;
    }
    let pad = (blur * 1.5).ceil();
    let (mw, mh) = ((w + pad * 2.0).ceil() as u32, (h + pad * 2.0).ceil() as u32);
    let Some(mut mask) = Pixmap::new(mw, mh) else { return };
    fill_rrect(&mut mask, pad, pad, w, h, r, [255, 255, 255, 255]);
    let mut alpha: Vec<f32> = mask.data().chunks_exact(4).map(|px| px[3] as f32 / 255.0).collect();
    let radius = (blur / 2.0).round().max(1.0) as usize;
    for _ in 0..3 {
        box_blur(&mut alpha, mw as usize, mh as usize, radius);
    }
    let (ox, oy) = ((x - pad).round() as i32, (y - pad).round() as i32);
    for j in 0..mh as usize {
        for i in 0..mw as usize {
            let a = alpha[j * mw as usize + i];
            if a > 0.002 {
                crate::text::blend_pixel(pm, ox + i as i32, oy + j as i32, rgba, a);
            }
        }
    }
}

fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0.0f32; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &buf[y * w..(y + 1) * w];
        let mut acc: f32 = (0..=r).map(|i| row[i.min(w - 1)]).sum::<f32>() + row[0] * r as f32;
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            acc += row[(x + r + 1).min(w - 1)] - row[x.saturating_sub(r)];
        }
    }
    for x in 0..w {
        let at = |y: usize| tmp[y.min(h - 1) * w + x];
        let mut acc: f32 = (0..=r).map(at).sum::<f32>() + at(0) * r as f32;
        for y in 0..h {
            buf[y * w + x] = acc * norm;
            acc += at(y + r + 1) - at(y.saturating_sub(r));
        }
    }
}

// ---------------------------------------------------------------------------
// Easing

/// 0 → 1 over `fade_in` after `start`, 1 → 0 over `fade_out` after `end`.
fn envelope(t: f64, start: f64, end: f64, fade_in: f64, fade_out: f64) -> f32 {
    if t < start || t >= end + fade_out {
        return 0.0;
    }
    let fade_in = fade_in.min((end - start).max(1e-3));
    let a = ((t - start) / fade_in).min(1.0);
    let b = if t > end { 1.0 - (t - end) / fade_out } else { 1.0 };
    a.min(b).clamp(0.0, 1.0) as f32
}

fn ease_out_cubic(x: f32) -> f32 {
    1.0 - (1.0 - x).powi(3)
}

fn ease_out_back(x: f32) -> f32 {
    let (c1, c3) = (1.40158, 2.40158);
    1.0 + c3 * (x - 1.0).powi(3) + c1 * (x - 1.0).powi(2)
}

fn smootherstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_ramps() {
        assert_eq!(envelope(0.0, 1.0, 2.0, 0.5, 0.5), 0.0);
        assert!((envelope(1.25, 1.0, 2.0, 0.5, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(envelope(1.6, 1.0, 2.0, 0.5, 0.5), 1.0);
        assert!((envelope(2.25, 1.0, 2.0, 0.5, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(envelope(2.5, 1.0, 2.0, 0.5, 0.5), 0.0);
    }
}
