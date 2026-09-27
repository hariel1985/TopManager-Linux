//! Performance: CPU (with per-core view), memory, network and GPU charts.
//!
//! "Live" charts use samples the window records itself; the 5m–24h ranges
//! come from the daemon's persistent history.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tm_core::format::{format_bytes, format_bytes_per_second, format_uptime};
use tm_core::metrics::{HistoryRange, MetricsSample};
use tm_core::model::{CoreType, MemoryPressure, Snapshot};

use crate::chart::{color, Chart, Series, StackBar};
use crate::client::Client;
use crate::pages::StatGrid;

type Points = Vec<(f64, f64)>;

/// Live points kept per series (~6 minutes at the default 3 s interval).
const LIVE_LEN: usize = 120;

#[derive(Default)]
struct Live {
    last_t: f64,
    interval: f64,
    cpu: VecDeque<(f64, f64)>,
    mem: VecDeque<(f64, f64)>,
    down: VecDeque<(f64, f64)>,
    up: VecDeque<(f64, f64)>,
    cores: Vec<VecDeque<(f64, f64)>>,
    gpu: Vec<VecDeque<(f64, f64)>>,
}

fn push(buf: &mut VecDeque<(f64, f64)>, p: (f64, f64)) {
    if buf.len() == LIVE_LEN {
        buf.pop_front();
    }
    buf.push_back(p);
}

struct SidebarItem {
    row: gtk::ListBoxRow,
    value: gtk::Label,
    chart: Chart,
}

fn sidebar_item(title: &str, name: &str) -> SidebarItem {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).css_classes(["sidebar-row"]).build();
    b.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build());
    let value = gtk::Label::builder().xalign(0.0).css_classes(["caption", "numeric"]).build();
    b.append(&value);
    let chart = Chart::new(28, false);
    b.append(&chart.area);
    let row = gtk::ListBoxRow::builder().child(&b).name(name).build();
    SidebarItem { row, value, chart }
}

fn card(child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card", "chart-card"]).build();
    b.append(child);
    b
}

fn heading(title: &str, sub: &gtk::Label) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["title-1"]).build());
    sub.set_hexpand(true);
    sub.set_xalign(1.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.add_css_class("dim-label");
    b.append(sub);
    b
}

fn detail_page(children: &[&gtk::Widget]) -> gtk::ScrolledWindow {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    for c in children {
        b.append(*c);
    }
    gtk::ScrolledWindow::builder().child(&b).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()
}

pub struct PerformancePage {
    pub root: gtk::Box,
    client: Client,
    live: RefCell<Live>,
    range: Cell<HistoryRange>,
    history: RefCell<Vec<MetricsSample>>,
    history_at: Cell<f64>,
    detail: gtk::Stack,
    side_cpu: SidebarItem,
    side_mem: SidebarItem,
    side_net: SidebarItem,
    side_gpu: SidebarItem,
    cpu_sub: gtk::Label,
    cpu_chart: Chart,
    cpu_stats: StatGrid,
    cores_box: gtk::FlowBox,
    core_cells: RefCell<Vec<(gtk::Label, Chart)>>,
    mem_sub: gtk::Label,
    mem_chart: Chart,
    mem_bar: StackBar,
    mem_stats: StatGrid,
    net_chart: Chart,
    net_stats: StatGrid,
    net_ifaces: gtk::Grid,
    gpu_sub: gtk::Label,
    gpu_chart: Chart,
    gpu_stats: StatGrid,
    gpu_note: gtk::Label,
}

impl PerformancePage {
    pub fn new(client: Client) -> Rc<Self> {
        // ---- sidebar ----
        let sidebar = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let side_cpu = sidebar_item("CPU", "cpu");
        let side_mem = sidebar_item("Memory", "memory");
        let side_net = sidebar_item("Network", "network");
        let side_gpu = sidebar_item("GPU", "gpu");
        for s in [&side_cpu, &side_mem, &side_net, &side_gpu] {
            sidebar.append(&s.row);
        }
        side_cpu.chart.set_y_max(Some(100.0));
        side_mem.chart.set_y_max(Some(100.0));
        side_gpu.chart.set_y_max(Some(100.0));
        side_gpu.row.set_visible(false);
        let side_scroll = gtk::ScrolledWindow::builder()
            .child(&sidebar)
            .width_request(220)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        // ---- range selector ----
        let ranges = gtk::Box::builder().css_classes(["linked"]).halign(gtk::Align::End).build();
        let mut first: Option<gtk::ToggleButton> = None;
        let range_buttons: Vec<(HistoryRange, gtk::ToggleButton)> = [
            (HistoryRange::Live, "Live"),
            (HistoryRange::M5, "5 min"),
            (HistoryRange::M30, "30 min"),
            (HistoryRange::H1, "1 hour"),
            (HistoryRange::H24, "24 hours"),
        ]
        .into_iter()
        .map(|(r, label)| {
            let b = gtk::ToggleButton::builder().label(label).build();
            if let Some(f) = &first {
                b.set_group(Some(f));
            } else {
                b.set_active(true);
                first = Some(b.clone());
            }
            ranges.append(&b);
            (r, b)
        })
        .collect();

        // ---- CPU ----
        let cpu_sub = gtk::Label::new(None);
        let cpu_chart = Chart::new(220, true);
        cpu_chart.set_y_max(Some(100.0));
        cpu_chart.set_y_format(|v| format!("{v:.0}%"));
        let cpu_stats =
            StatGrid::new(&["Utilization", "User", "System", "I/O wait", "Cores", "Processes", "Threads", "Uptime"], 4);
        let cores_box = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(8)
            .column_spacing(8)
            .row_spacing(8)
            .build();
        let cores_title = gtk::Label::builder().label("Per core (live)").xalign(0.0).css_classes(["heading"]).build();
        let cpu_page = detail_page(&[
            heading("CPU", &cpu_sub).upcast_ref(),
            card(&cpu_chart.area).upcast_ref(),
            cpu_stats.grid.upcast_ref(),
            cores_title.upcast_ref(),
            cores_box.upcast_ref(),
        ]);

        // ---- Memory ----
        let mem_sub = gtk::Label::new(None);
        let mem_chart = Chart::new(220, true);
        mem_chart.set_y_max(Some(100.0));
        mem_chart.set_y_format(|v| format!("{v:.0}%"));
        let mem_bar = StackBar::new();
        let legend = gtk::Label::builder()
            .use_markup(true)
            .xalign(0.0)
            .css_classes(["caption"])
            .label("<span foreground=\"#9141ac\">■</span> Used   <span foreground=\"#e5a50a\">■</span> Cache and buffers   <span alpha=\"40%\">■</span> Free")
            .build();
        let mem_stats = StatGrid::new(
            &["Used", "Available", "Cache", "Buffers", "Shared", "Swap", "Compressed (zram)", "Pressure"],
            4,
        );
        let bar_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        bar_box.append(&mem_bar.area);
        bar_box.append(&legend);
        let mem_page = detail_page(&[
            heading("Memory", &mem_sub).upcast_ref(),
            card(&mem_chart.area).upcast_ref(),
            bar_box.upcast_ref(),
            mem_stats.grid.upcast_ref(),
        ]);

        // ---- Network ----
        let net_sub = gtk::Label::new(None);
        let net_chart = Chart::new(220, true);
        net_chart.set_y_format(format_bytes_per_second);
        let net_legend = gtk::Label::builder()
            .use_markup(true)
            .xalign(0.0)
            .css_classes(["caption"])
            .label("<span foreground=\"#2ec27e\">■</span> Download   <span foreground=\"#e66100\">■</span> Upload")
            .build();
        let net_stats = StatGrid::new(&["Download", "Upload", "Received", "Sent"], 4);
        let net_ifaces = gtk::Grid::builder().column_spacing(24).row_spacing(6).build();
        let ifaces_title = gtk::Label::builder().label("Interfaces").xalign(0.0).css_classes(["heading"]).build();
        let net_page = detail_page(&[
            heading("Network", &net_sub).upcast_ref(),
            card(&net_chart.area).upcast_ref(),
            net_legend.upcast_ref(),
            net_stats.grid.upcast_ref(),
            ifaces_title.upcast_ref(),
            net_ifaces.upcast_ref(),
        ]);

        // ---- GPU ----
        let gpu_sub = gtk::Label::new(None);
        let gpu_chart = Chart::new(220, true);
        gpu_chart.set_y_max(Some(100.0));
        gpu_chart.set_y_format(|v| format!("{v:.0}%"));
        let gpu_stats = StatGrid::new(&["Utilization", "Video memory", "Driver", "Memory type"], 4);
        let gpu_note = gtk::Label::builder().wrap(true).xalign(0.0).css_classes(["dim-label"]).build();
        let gpu_page = detail_page(&[
            heading("GPU", &gpu_sub).upcast_ref(),
            card(&gpu_chart.area).upcast_ref(),
            gpu_stats.grid.upcast_ref(),
            gpu_note.upcast_ref(),
        ]);

        let detail = gtk::Stack::builder().hexpand(true).transition_type(gtk::StackTransitionType::Crossfade).build();
        detail.add_named(&cpu_page, Some("cpu"));
        detail.add_named(&mem_page, Some("memory"));
        detail.add_named(&net_page, Some("network"));
        detail.add_named(&gpu_page, Some("gpu"));

        let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let range_bar = gtk::Box::builder().margin_top(12).margin_end(18).margin_start(18).build();
        range_bar.append(&ranges);
        ranges.set_hexpand(true);
        right.append(&range_bar);
        right.append(&detail);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&side_scroll);
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        root.append(&right);

        let page = Rc::new(PerformancePage {
            root,
            client,
            live: RefCell::new(Live { interval: 3.0, ..Default::default() }),
            range: Cell::new(HistoryRange::Live),
            history: RefCell::default(),
            history_at: Cell::new(0.0),
            detail,
            side_cpu,
            side_mem,
            side_net,
            side_gpu,
            cpu_sub,
            cpu_chart,
            cpu_stats,
            cores_box,
            core_cells: RefCell::default(),
            mem_sub,
            mem_chart,
            mem_bar,
            mem_stats,
            net_chart,
            net_stats,
            net_ifaces,
            gpu_sub,
            gpu_chart,
            gpu_stats,
            gpu_note,
        });

        sidebar.select_row(Some(&page.side_cpu.row));
        let w = Rc::downgrade(&page);
        sidebar.connect_row_selected(move |_, row| {
            if let (Some(p), Some(row)) = (w.upgrade(), row) {
                p.detail.set_visible_child_name(&row.widget_name());
                p.rerender();
            }
        });
        for (range, b) in range_buttons {
            let w = Rc::downgrade(&page);
            b.connect_toggled(move |b| {
                if let (true, Some(p)) = (b.is_active(), w.upgrade()) {
                    p.range.set(range);
                    p.history.borrow_mut().clear();
                    p.history_at.set(0.0);
                    p.rerender();
                }
            });
        }
        page
    }

    /// Record live points (called on every tick, whichever page is visible).
    pub fn record(&self, s: &Snapshot) {
        let mut l = self.live.borrow_mut();
        if s.timestamp <= l.last_t {
            return;
        }
        if l.last_t > 0.0 {
            l.interval = (s.timestamp - l.last_t).clamp(0.5, 60.0);
        }
        l.last_t = s.timestamp;
        let t = s.timestamp;
        push(&mut l.cpu, (t, s.cpu.global_usage));
        push(&mut l.mem, (t, s.memory.usage_percentage()));
        push(&mut l.down, (t, s.network.total_rx_rate));
        push(&mut l.up, (t, s.network.total_tx_rate));
        l.cores.resize_with(s.cpu.cores.len(), VecDeque::new);
        for (i, c) in s.cpu.cores.iter().enumerate() {
            push(&mut l.cores[i], (t, c.usage));
        }
        l.gpu.resize_with(s.gpus.len(), VecDeque::new);
        for (i, g) in s.gpus.iter().enumerate() {
            if let Some(u) = g.utilization {
                push(&mut l.gpu[i], (t, u));
            }
        }
    }

    fn rerender(self: &Rc<Self>) {
        let snap = self.client.data().snapshot.clone();
        if let Some(s) = snap {
            self.render(&s);
        }
    }

    fn live_window(&self) -> (f64, f64) {
        let l = self.live.borrow();
        (l.last_t - l.interval * (LIVE_LEN as f64 - 1.0), l.last_t)
    }

    /// History for the selected range, refetched every 30 s while visible.
    fn ensure_history(self: &Rc<Self>, now: f64) {
        let range = self.range.get();
        if range == HistoryRange::Live || now - self.history_at.get() < 30.0 {
            return;
        }
        self.history_at.set(now);
        let (client, w) = (self.client.clone(), Rc::downgrade(self));
        let name = serde_json::to_value(range).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        glib::spawn_future_local(async move {
            if let Ok(samples) = client.history(&name).await {
                if let Some(p) = w.upgrade() {
                    if p.range.get() == range {
                        *p.history.borrow_mut() = samples;
                        p.rerender();
                    }
                }
            }
        });
    }

    /// Main-chart series for the selected range: live buffer or history.
    fn series(
        &self,
        now: f64,
        live: impl Fn(&Live) -> Vec<Vec<(f64, f64)>>,
        hist: impl Fn(&MetricsSample) -> Vec<f64>,
    ) -> (Vec<Points>, (f64, f64)) {
        match self.range.get().seconds() {
            None => (live(&self.live.borrow()), self.live_window()),
            Some(secs) => {
                let h = self.history.borrow();
                let n = h.first().map(|s| hist(s).len()).unwrap_or(0);
                let mut out = vec![Vec::with_capacity(h.len()); n];
                for s in h.iter() {
                    for (i, v) in hist(s).into_iter().enumerate() {
                        out[i].push((s.t, v));
                    }
                }
                (out, (now - secs, now))
            }
        }
    }

    pub fn render(self: &Rc<Self>, s: &Snapshot) {
        let now = s.timestamp;
        self.ensure_history(now);
        let live_w = self.live_window();
        let l = self.live.borrow();
        let v = |b: &VecDeque<(f64, f64)>| b.iter().copied().collect::<Vec<_>>();

        // sidebar (always live)
        self.side_cpu.value.set_label(&format!("{:.0}%", s.cpu.global_usage));
        self.side_cpu.chart.set_data(vec![Series { points: v(&l.cpu), color: color::BLUE, fill: true }], live_w);
        self.side_mem.value.set_label(&format!(
            "{} / {} ({:.0}%)",
            format_bytes(s.memory.used),
            format_bytes(s.memory.total),
            s.memory.usage_percentage()
        ));
        self.side_mem.chart.set_data(vec![Series { points: v(&l.mem), color: color::PURPLE, fill: true }], live_w);
        self.side_net.value.set_label(&format!(
            "↓ {}  ↑ {}",
            format_bytes_per_second(s.network.total_rx_rate),
            format_bytes_per_second(s.network.total_tx_rate)
        ));
        self.side_net.chart.set_data(
            vec![
                Series { points: v(&l.down), color: color::GREEN, fill: true },
                Series { points: v(&l.up), color: color::ORANGE, fill: false },
            ],
            live_w,
        );
        let gpu = s.gpus.first();
        self.side_gpu.row.set_visible(gpu.is_some());
        if let Some(g) = gpu {
            self.side_gpu.value.set_label(&match (g.utilization, g.vram_used) {
                (Some(u), _) => format!("{u:.0}%"),
                (None, Some(m)) => format_bytes(m),
                _ => g.driver.clone(),
            });
            let pts = l.gpu.first().map(v).unwrap_or_default();
            self.side_gpu.chart.set_data(vec![Series { points: pts, color: color::TEAL, fill: true }], live_w);
        }
        drop(l);

        match self.detail.visible_child_name().as_deref() {
            Some("cpu") => self.render_cpu(s, now),
            Some("memory") => self.render_memory(s, now),
            Some("network") => self.render_network(s, now),
            Some("gpu") => self.render_gpu(s),
            _ => {}
        }
    }

    fn render_cpu(&self, s: &Snapshot, now: f64) {
        let (series, window) = self.series(now, |l| vec![l.cpu.iter().copied().collect()], |m| vec![m.cpu]);
        self.cpu_chart.set_data(
            series.into_iter().map(|p| Series { points: p, color: color::BLUE, fill: true }).collect(),
            window,
        );
        self.cpu_sub.set_label(&s.system.cpu_model);
        let c = &s.cpu;
        let p_cores = c.cores.iter().filter(|x| x.core_type == CoreType::Performance).count();
        let e_cores = c.cores.iter().filter(|x| x.core_type == CoreType::Efficiency).count();
        self.cpu_stats.set("Utilization", format!("{:.1}%", c.global_usage));
        self.cpu_stats.set("User", format!("{:.1}%", c.user_usage));
        self.cpu_stats.set("System", format!("{:.1}%", c.system_usage));
        self.cpu_stats.set("I/O wait", format!("{:.1}%", c.iowait_usage));
        self.cpu_stats.set(
            "Cores",
            if c.is_hybrid() {
                format!("{} ({p_cores} P + {e_cores} E)", c.cores.len())
            } else {
                c.cores.len().to_string()
            },
        );
        let procs = self.client.data().processes.len();
        let threads: u32 = self.client.data().processes.iter().map(|p| p.threads).sum();
        self.cpu_stats.set("Processes", procs.to_string());
        self.cpu_stats.set("Threads", threads.to_string());
        self.cpu_stats.set("Uptime", format_uptime(s.system.uptime_secs));

        // Per-core cards (live).
        let mut cells = self.core_cells.borrow_mut();
        while cells.len() < c.cores.len() {
            let i = cells.len();
            let b = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .css_classes(["card", "chart-card"])
                .build();
            let label = gtk::Label::builder().xalign(0.0).css_classes(["caption", "numeric"]).build();
            let chart = Chart::new(48, false);
            chart.set_y_max(Some(100.0));
            b.append(&label);
            b.append(&chart.area);
            self.cores_box.append(&b);
            cells.push((label, chart));
            let _ = i;
        }
        let l = self.live.borrow();
        let w = self.live_window();
        for (i, core) in c.cores.iter().enumerate() {
            let (label, chart) = &cells[i];
            let kind = match core.core_type {
                CoreType::Performance => " · P",
                CoreType::Efficiency => " · E",
                CoreType::Unknown => "",
            };
            label.set_label(&format!("CPU {}{kind}   {:.0}%", core.id, core.usage));
            let color = if core.core_type == CoreType::Efficiency { color::TEAL } else { color::BLUE };
            let pts = l.cores.get(i).map(|b| b.iter().copied().collect()).unwrap_or_default();
            chart.set_data(vec![Series { points: pts, color, fill: true }], w);
        }
    }

    fn render_memory(&self, s: &Snapshot, now: f64) {
        let (series, window) = self.series(now, |l| vec![l.mem.iter().copied().collect()], |m| vec![m.mem_percent()]);
        self.mem_chart.set_data(
            series.into_iter().map(|p| Series { points: p, color: color::PURPLE, fill: true }).collect(),
            window,
        );
        let m = &s.memory;
        self.mem_sub.set_label(&format!("{} total", format_bytes(m.total)));
        let total = m.total.max(1) as f64;
        let cache = (m.cached + m.buffers) as f64;
        self.mem_bar.set(vec![
            (m.used as f64 / total, color::PURPLE),
            ((cache / total).min(1.0 - m.used as f64 / total), color::YELLOW),
        ]);
        self.mem_stats.set("Used", format!("{} ({:.1}%)", format_bytes(m.used), m.usage_percentage()));
        self.mem_stats.set("Available", format_bytes(m.available));
        self.mem_stats.set("Cache", format_bytes(m.cached));
        self.mem_stats.set("Buffers", format_bytes(m.buffers));
        self.mem_stats.set("Shared", format_bytes(m.shmem));
        self.mem_stats.set(
            "Swap",
            if m.swap_total == 0 {
                "none".into()
            } else {
                format!("{} / {}", format_bytes(m.swap_used), format_bytes(m.swap_total))
            },
        );
        self.mem_stats.set(
            "Compressed (zram)",
            if m.compressed == 0 {
                "not in use".into()
            } else {
                format!("{} holding {}", format_bytes(m.compressed), format_bytes(m.compressed_original))
            },
        );
        let level = match m.pressure {
            MemoryPressure::Nominal => "Normal",
            MemoryPressure::Warning => "Elevated",
            MemoryPressure::Critical => "Critical",
            MemoryPressure::Unknown => "Unknown",
        };
        self.mem_stats.set(
            "Pressure",
            match m.psi_some_avg10 {
                Some(some) => format!("{level} ({some:.1}% stalled)"),
                None => level.to_string(),
            },
        );
    }

    fn render_network(&self, s: &Snapshot, now: f64) {
        let (series, window) = self.series(
            now,
            |l| vec![l.down.iter().copied().collect(), l.up.iter().copied().collect()],
            |m| vec![m.net_down, m.net_up],
        );
        let colors = [(color::GREEN, true), (color::ORANGE, false)];
        self.net_chart.set_data(
            series.into_iter().zip(colors).map(|(p, (c, fill))| Series { points: p, color: c, fill }).collect(),
            window,
        );
        let n = &s.network;
        self.net_stats.set("Download", format_bytes_per_second(n.total_rx_rate));
        self.net_stats.set("Upload", format_bytes_per_second(n.total_tx_rate));
        self.net_stats.set("Received", format_bytes(n.total_rx_bytes));
        self.net_stats.set("Sent", format_bytes(n.total_tx_bytes));

        while let Some(c) = self.net_ifaces.first_child() {
            self.net_ifaces.remove(&c);
        }
        for (col, h) in ["Interface", "State", "Download", "Upload", "Received", "Sent"].iter().enumerate() {
            self.net_ifaces.attach(
                &gtk::Label::builder().label(*h).xalign(0.0).css_classes(["stat-title"]).build(),
                col as i32,
                0,
                1,
                1,
            );
        }
        for (row, i) in n.interfaces.iter().enumerate() {
            let state = match (i.is_up, i.is_virtual) {
                (true, false) => "up",
                (true, true) => "up (virtual)",
                (false, true) => "down (virtual)",
                (false, false) => "down",
            };
            let cells = [
                i.name.clone(),
                state.to_string(),
                format_bytes_per_second(i.rx_rate),
                format_bytes_per_second(i.tx_rate),
                format_bytes(i.rx_bytes),
                format_bytes(i.tx_bytes),
            ];
            for (col, text) in cells.iter().enumerate() {
                let l = gtk::Label::builder().label(text).xalign(0.0).css_classes(["numeric"]).build();
                self.net_ifaces.attach(&l, col as i32, row as i32 + 1, 1, 1);
            }
        }
    }

    fn render_gpu(&self, s: &Snapshot) {
        let Some(g) = s.gpus.first() else { return };
        self.gpu_sub.set_label(&g.name);
        let pts = self.live.borrow().gpu.first().map(|b| b.iter().copied().collect()).unwrap_or_default();
        self.gpu_chart.set_data(vec![Series { points: pts, color: color::TEAL, fill: true }], self.live_window());
        self.gpu_stats.set("Utilization", g.utilization.map(|u| format!("{u:.0}%")).unwrap_or_else(|| "—".into()));
        self.gpu_stats.set(
            "Video memory",
            match (g.vram_used, g.vram_total) {
                (Some(u), Some(t)) => format!("{} / {}", format_bytes(u), format_bytes(t)),
                (Some(u), None) => format_bytes(u),
                _ => "—".into(),
            },
        );
        self.gpu_stats.set("Driver", &g.driver);
        self.gpu_stats.set("Memory type", if g.is_unified_memory { "Shared with system" } else { "Dedicated" });
        self.gpu_note.set_label(if g.utilization.is_none() {
            "This driver does not report utilization through sysfs; the chart stays empty. AMD GPUs (amdgpu) report it directly."
        } else {
            ""
        });
        self.gpu_chart.area.set_visible(g.utilization.is_some());
    }
}
