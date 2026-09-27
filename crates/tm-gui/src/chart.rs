//! Time-series line charts drawn with cairo, plus a stacked usage bar.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk};

/// GNOME palette colors, readable on light and dark backgrounds.
pub mod color {
    pub const BLUE: (f64, f64, f64) = (0.21, 0.52, 0.89);
    pub const PURPLE: (f64, f64, f64) = (0.57, 0.25, 0.67);
    pub const GREEN: (f64, f64, f64) = (0.18, 0.76, 0.49);
    pub const ORANGE: (f64, f64, f64) = (0.90, 0.38, 0.0);
    pub const YELLOW: (f64, f64, f64) = (0.90, 0.65, 0.04);
    pub const TEAL: (f64, f64, f64) = (0.13, 0.63, 0.62);
}

#[derive(Clone)]
pub struct Series {
    /// `(unix seconds, value)`
    pub points: Vec<(f64, f64)>,
    pub color: (f64, f64, f64),
    pub fill: bool,
}

#[derive(Default)]
struct State {
    series: Vec<Series>,
    window: (f64, f64),
    /// Fixed y maximum (percentages); `None` scales to the data.
    y_max: Option<f64>,
    y_format: Option<Box<dyn Fn(f64) -> String>>,
    grid: bool,
}

#[derive(Clone)]
pub struct Chart {
    pub area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl Chart {
    pub fn new(height: i32, grid: bool) -> Self {
        let area = gtk::DrawingArea::builder().content_height(height).hexpand(true).build();
        let state = Rc::new(RefCell::new(State { grid, ..Default::default() }));
        let s = state.clone();
        area.set_draw_func(move |area, cr, w, h| draw(area, cr, w as f64, h as f64, &s.borrow()));
        if grid {
            area.add_css_class("chart");
        }
        Chart { area, state }
    }

    pub fn set_y_max(&self, max: Option<f64>) {
        self.state.borrow_mut().y_max = max;
    }

    pub fn set_y_format(&self, f: impl Fn(f64) -> String + 'static) {
        self.state.borrow_mut().y_format = Some(Box::new(f));
    }

    pub fn set_data(&self, series: Vec<Series>, window: (f64, f64)) {
        {
            let mut s = self.state.borrow_mut();
            s.series = series;
            s.window = window;
        }
        self.area.queue_draw();
    }
}

fn nice_max(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = 10f64.powf(v.log10().floor());
    let m = v / exp;
    let nice = if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * exp
}

fn draw(area: &gtk::DrawingArea, cr: &cairo::Context, w: f64, h: f64, s: &State) {
    let fg: gdk::RGBA = area.color();
    let (fr, fg_, fb) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);
    let label_w = if s.grid && s.y_format.is_some() { 56.0 } else { 0.0 };
    let plot_w = (w - label_w).max(1.0);
    let top = if s.grid { 6.0 } else { 1.0 };
    let plot_h = (h - top - 1.0).max(1.0);

    let data_max = s.series.iter().flat_map(|se| se.points.iter().map(|p| p.1)).fold(0.0, f64::max);
    let y_max = s.y_max.unwrap_or_else(|| nice_max(data_max * 1.1));
    let (t0, t1) = s.window;
    let span = (t1 - t0).max(1e-6);
    let x = |t: f64| (t - t0) / span * plot_w;
    let y = |v: f64| top + plot_h - (v / y_max).clamp(0.0, 1.0) * plot_h;

    if s.grid {
        cr.set_line_width(1.0);
        for i in 0..=4 {
            let gy = (top + plot_h * i as f64 / 4.0).round() + 0.5;
            cr.set_source_rgba(fr, fg_, fb, if i == 4 { 0.25 } else { 0.10 });
            cr.move_to(0.0, gy);
            cr.line_to(plot_w, gy);
            let _ = cr.stroke();
        }
        if let Some(fmt) = &s.y_format {
            cr.set_source_rgba(fr, fg_, fb, 0.55);
            cr.set_font_size(10.0);
            for (i, v) in [(0, y_max), (2, y_max / 2.0)] {
                let gy = top + plot_h * i as f64 / 4.0;
                cr.move_to(plot_w + 6.0, gy + 10.0);
                let _ = cr.show_text(&fmt(v));
            }
        }
    }

    for se in &s.series {
        let pts: Vec<(f64, f64)> =
            se.points.iter().filter(|p| p.0 >= t0 - span * 0.05).map(|p| (x(p.0), y(p.1))).collect();
        if pts.len() < 2 {
            continue;
        }
        let (r, g, b) = se.color;
        cr.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            cr.line_to(p.0, p.1);
        }
        cr.set_source_rgba(r, g, b, 1.0);
        cr.set_line_width(if s.grid { 1.6 } else { 1.2 });
        cr.set_line_join(cairo::LineJoin::Round);
        if se.fill {
            let _ = cr.stroke_preserve();
            cr.line_to(pts[pts.len() - 1].0, top + plot_h);
            cr.line_to(pts[0].0, top + plot_h);
            cr.close_path();
            cr.set_source_rgba(r, g, b, 0.16);
            let _ = cr.fill();
        } else {
            let _ = cr.stroke();
        }
    }
}

type Segments = Rc<RefCell<Vec<(f64, (f64, f64, f64))>>>;

/// A horizontal bar of colored segments (e.g. used / cache / free memory).
#[derive(Clone)]
pub struct StackBar {
    pub area: gtk::DrawingArea,
    segments: Segments,
}

impl StackBar {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().content_height(14).hexpand(true).build();
        let segments: Segments = Rc::default();
        let s = segments.clone();
        area.set_draw_func(move |area, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            let fg = area.color();
            rounded(cr, 0.0, 0.0, w, h, h / 2.0);
            cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.10);
            let _ = cr.fill_preserve();
            cr.clip();
            let mut x = 0.0;
            for (v, (r, g, b)) in s.borrow().iter() {
                let sw = v.clamp(0.0, 1.0) * w;
                cr.rectangle(x, 0.0, sw, h);
                cr.set_source_rgb(*r, *g, *b);
                let _ = cr.fill();
                x += sw;
            }
        });
        StackBar { area, segments }
    }

    /// Segments as fractions of the whole (the remainder stays empty).
    pub fn set(&self, segments: Vec<(f64, (f64, f64, f64))>) {
        *self.segments.borrow_mut() = segments;
        self.area.queue_draw();
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::nice_max;

    #[test]
    fn nice_maxima() {
        assert_eq!(nice_max(0.0), 1.0);
        assert_eq!(nice_max(0.7), 1.0);
        assert_eq!(nice_max(1500.0), 2000.0);
        assert_eq!(nice_max(4.2e6), 5e6);
        assert_eq!(nice_max(9.1), 10.0);
    }
}
