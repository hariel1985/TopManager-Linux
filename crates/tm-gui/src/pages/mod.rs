pub mod apps;
pub mod performance;
pub mod power;
pub mod processes;

use gtk::prelude::*;

/// A title/value grid of statistics whose values can be updated by key.
pub struct StatGrid {
    pub grid: gtk::Grid,
    values: Vec<(&'static str, gtk::Label)>,
}

impl StatGrid {
    pub fn new(titles: &[&'static str], columns: i32) -> Self {
        let grid = gtk::Grid::builder().column_spacing(32).row_spacing(12).column_homogeneous(true).build();
        let mut values = Vec::new();
        for (i, title) in titles.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
            cell.append(&gtk::Label::builder().label(*title).xalign(0.0).css_classes(["stat-title"]).build());
            let v = gtk::Label::builder()
                .label("—")
                .xalign(0.0)
                .selectable(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["stat-value"])
                .build();
            cell.append(&v);
            grid.attach(&cell, i as i32 % columns, i as i32 / columns, 1, 1);
            values.push((*title, v));
        }
        StatGrid { grid, values }
    }

    pub fn set(&self, title: &str, value: impl AsRef<str>) {
        if let Some((_, l)) = self.values.iter().find(|(t, _)| *t == title) {
            if l.label() != value.as_ref() {
                l.set_label(value.as_ref());
            }
        }
    }
}
