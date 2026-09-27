//! topmanager — the TopManager main window (GTK4 + libadwaita).
//!
//! All data comes from topmanagerd over D-Bus; the window never reads /proc
//! itself, so it shows exactly what the top-bar HUD and the alerts see.

mod chart;
mod client;
mod inspector;
mod pages;
mod preferences;
mod process_object;
mod window;

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = tm_core::bus::APP_ID;

/// Shows a transient message in the main window.
pub type Toaster = Rc<dyn Fn(&str)>;

const CSS: &str = "
.numeric { font-feature-settings: \"tnum\"; }
.process-table { font-feature-settings: \"tnum\"; }
.health-score { font-size: 48px; font-weight: 800; font-feature-settings: \"tnum\"; }
.health-excellent { color: @success_color; }
.health-good { color: #26a269; }
.health-fair { color: @warning_color; }
.health-poor { color: @error_color; }
.stat-title { font-size: 0.85em; opacity: 0.7; }
.stat-value { font-weight: 600; font-feature-settings: \"tnum\"; }
.big-value { font-size: 1.6em; font-weight: 700; font-feature-settings: \"tnum\"; }
.chart-card { padding: 12px; }
.sidebar-row { padding: 8px 6px; }
";

fn main() -> glib::ExitCode {
    let app =
        adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();

    app.connect_startup(|app| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
        app.add_action_entries([quit]);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.search", &["<Control>f"]);
    });

    // `topmanager --page processes|apps|performance|power` (also forwarded to
    // an already running instance).
    app.connect_command_line(|app, cmd| {
        let args: Vec<String> = cmd.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let page = args.iter().position(|a| a == "--page").and_then(|i| args.get(i + 1)).cloned();
        let win = match app.active_window().and_downcast::<adw::ApplicationWindow>() {
            Some(w) => w,
            None => window::build(app),
        };
        if let Some(page) = page {
            window::show_page(&win, &page);
        }
        win.present();
        if args.iter().any(|a| a == "--preferences") {
            let _ = WidgetExt::activate_action(&win, "win.preferences", None);
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}
