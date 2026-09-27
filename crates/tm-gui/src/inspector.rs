//! Process inspector: the deep dive shown on double-click / Ctrl+I.

use adw::prelude::*;
use gtk::{gio, glib};
use tm_core::format::{format_bytes, format_bytes_per_second};
use tm_core::model::{ProcessDetail, ProcessItem};

use crate::client::Client;
use crate::process_object::state_label;
use crate::Toaster;

fn row(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(title).subtitle(value).subtitle_selectable(true).build();
    r.add_css_class("property");
    r
}

fn opt_bytes(v: Option<u64>) -> String {
    v.map(format_bytes).unwrap_or_else(|| "—".into())
}

fn local_time(unix: f64) -> String {
    glib::DateTime::from_unix_local(unix as i64)
        .and_then(|d| d.format("%Y-%m-%d %H:%M:%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "—".into())
}

/// Reveal a file in the file manager (org.freedesktop.FileManager1).
fn show_in_files(client: &Client, path: &str, toast: Toaster) {
    let uri = gio::File::for_path(path).uri().to_string();
    let conn = client.connection().clone();
    glib::spawn_future_local(async move {
        let r = conn
            .call_future(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1",
                "ShowItems",
                Some(&(vec![uri], "").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        if let Err(e) = r {
            toast(&format!("Could not open the file manager: {e}"));
        }
    });
}

pub fn show(parent: &impl IsA<gtk::Widget>, client: Client, toast: Toaster, p: ProcessItem) {
    let page = adw::PreferencesPage::new();
    let dialog = adw::Dialog::builder().title(&p.name).content_width(560).content_height(640).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let general = adw::PreferencesGroup::builder().title("Process").build();
    general.add(&row("Name", &p.name));
    general.add(&row("PID / parent", &format!("{} / {}", p.pid, p.ppid)));
    general.add(&row("User", &format!("{} (uid {})", p.user, p.uid)));
    general.add(&row("State", state_label(p.state)));
    general.add(&row("Started", &local_time(p.start_time)));
    if let Some(app) = &p.app_id {
        general.add(&row("Application", app));
    }
    page.add(&general);

    let resources = adw::PreferencesGroup::builder().title("Resources").build();
    resources.add(&row("CPU", &format!("{:.1}% of one core · {:.1}% of all cores", p.cpu, p.cpu_total)));
    resources.add(&row("Threads", &p.threads.to_string()));
    resources.add(&row("Energy impact (estimate)", &format!("{:.1}", p.energy)));
    resources.add(&row(
        "Disk",
        &format!(
            "read {} ({} total) · write {} ({} total)",
            format_bytes_per_second(p.disk_read_rate),
            format_bytes(p.disk_read_bytes),
            format_bytes_per_second(p.disk_write_rate),
            format_bytes(p.disk_write_bytes)
        ),
    ));
    page.add(&resources);

    let memory = adw::PreferencesGroup::builder()
        .title("Memory")
        .description("Private is what the process alone holds in RAM. Resident includes pages shared with other processes; PSS splits shared pages fairly between them.")
        .build();
    memory.add(&row("Private", &format_bytes(p.memory)));
    memory.add(&row("Resident (RSS)", &format_bytes(p.resident)));
    page.add(&memory);

    let exe_group = adw::PreferencesGroup::builder().title("Executable").build();
    page.add(&exe_group);

    dialog.present(Some(parent));

    glib::spawn_future_local(async move {
        let d: ProcessDetail = match client.detail(p.pid).await {
            Ok(d) if d.start_ticks == p.start_ticks => d,
            Ok(_) => {
                exe_group.set_description(Some("The process has exited."));
                return;
            }
            Err(e) => {
                exe_group.set_description(Some(&format!("Details unavailable: {e}")));
                return;
            }
        };
        memory.add(&row("Proportional (PSS)", &opt_bytes(d.pss)));
        memory.add(&row("Unique (USS)", &opt_bytes(d.uss)));
        memory.add(&row("Swapped out", &opt_bytes(d.swap)));
        resources.add(&row("Open files", &d.open_files.map(|n| n.to_string()).unwrap_or_else(|| "—".into())));
        resources.add(&row(
            "Context switches",
            &format!(
                "{} voluntary · {} involuntary",
                d.voluntary_ctxt_switches.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                d.nonvoluntary_ctxt_switches.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
            ),
        ));
        if let Some(nice) = d.nice {
            general.add(&row("Nice", &nice.to_string()));
        }

        match &d.exe {
            Some(exe) => {
                let r = row("Path", exe);
                let open = gtk::Button::builder()
                    .icon_name("folder-open-symbolic")
                    .tooltip_text("Show in Files")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let (exe_c, client_c, toast_c) = (exe.clone(), client.clone(), toast.clone());
                open.connect_clicked(move |_| show_in_files(&client_c, &exe_c, toast_c.clone()));
                r.add_suffix(&open);
                exe_group.add(&r);
            }
            None => exe_group.set_description(Some("Not readable (the process belongs to another user).")),
        }
        if !d.cmdline.is_empty() {
            let r = row("Command line", &d.cmdline.join(" "));
            let copy = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Copy")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let text = d.cmdline.join(" ");
            copy.connect_clicked(move |b| b.clipboard().set_text(&text));
            r.add_suffix(&copy);
            exe_group.add(&r);
        }
        if let Some(cwd) = &d.cwd {
            exe_group.add(&row("Working directory", cwd));
        }
        if let Some(cg) = &d.cgroup {
            exe_group.add(&row("Control group", cg));
        }
    });
}
