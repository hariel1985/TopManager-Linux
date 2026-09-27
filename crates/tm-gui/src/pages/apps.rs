//! Apps: running desktop applications, grouped from their systemd app scopes.
//!
//! Switching to or minimizing an app is impossible for a regular Wayland
//! client, so those actions go through the TopManager HUD extension, which
//! runs inside gnome-shell and exports a tiny D-Bus bridge.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;
use tm_core::format::format_bytes;
use tm_core::model::ProcessItem;

use crate::client::Client;
use crate::process_object::{app_info, generic_icon, AppEntry};
use crate::Toaster;

const BRIDGE_PATH: &str = "/io/github/hariel1985/TopManager/Shell";
const BRIDGE_IFACE: &str = "io.github.hariel1985.TopManager.Shell1";

struct AppRow {
    row: adw::ActionRow,
    cpu: gtk::Label,
    mem: gtk::Label,
    focus: gtk::Button,
    minimize: gtk::Button,
}

#[derive(Default, Clone)]
struct AppGroup {
    name: String,
    icon: Option<gio::Icon>,
    cpu: f64,
    memory: u64,
    procs: Vec<(u32, u64, bool)>,
}

pub struct AppsPage {
    pub root: gtk::Box,
    client: Client,
    toast: Toaster,
    list: gtk::ListBox,
    empty: adw::StatusPage,
    banner: adw::Banner,
    rows: RefCell<HashMap<String, AppRow>>,
    groups: RefCell<BTreeMap<String, AppGroup>>,
    info: RefCell<HashMap<String, Option<AppEntry>>>,
    windows: RefCell<HashMap<String, u32>>,
    bridge: Cell<bool>,
}

impl AppsPage {
    pub fn new(client: Client, toast: Toaster) -> Rc<Self> {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let empty = adw::StatusPage::builder()
            .icon_name("view-app-grid-symbolic")
            .title("No apps")
            .description("Apps started from the desktop appear here.")
            .vexpand(true)
            .build();
        let banner = adw::Banner::new("Enable the TopManager HUD extension to switch to and minimize apps from here.");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&list);
        content.append(&empty);
        let clamp = adw::Clamp::builder()
            .maximum_size(900)
            .child(&content)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scrolled = gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&banner);
        root.append(&scrolled);

        let page = Rc::new(AppsPage {
            root,
            client,
            toast,
            list,
            empty,
            banner,
            rows: RefCell::default(),
            groups: RefCell::default(),
            info: RefCell::default(),
            windows: RefCell::default(),
            bridge: Cell::new(true),
        });
        page.build_actions();
        page
    }

    fn build_actions(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&Rc<AppsPage>, &str)| {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, param| {
                if let (Some(p), Some(id)) = (w.upgrade(), param.and_then(|v| v.get::<String>())) {
                    f(&p, &id);
                }
            });
            group.add_action(&a);
        };
        add("quit", |p, id| p.signal_app(id, "term"));
        add("force-quit", |p, id| p.signal_app(id, "kill"));
        add("focus", |p, id| p.bridge_call("ActivateApp", id));
        add("minimize", |p, id| p.bridge_call("MinimizeApp", id));
        add("copy-id", |p, id| p.root.clipboard().set_text(id));
        self.root.insert_action_group("apps", Some(&group));
    }

    fn signal_app(&self, id: &str, action: &'static str) {
        let procs = self.groups.borrow().get(id).map(|g| g.procs.clone()).unwrap_or_default();
        let (client, toast, name) = (self.client.clone(), self.toast.clone(), id.to_string());
        glib::spawn_future_local(async move {
            let mut errors = 0;
            for (pid, start, protected) in procs {
                if !protected && client.signal_process(pid, start, action).await.is_err() {
                    errors += 1;
                }
            }
            if errors > 0 {
                toast(&format!("{name}: {errors} processes could not be signalled"));
            }
        });
    }

    fn bridge_call(&self, method: &str, id: &str) {
        let conn = self.client.connection().clone();
        let (toast, method, id) = (self.toast.clone(), method.to_string(), id.to_string());
        glib::spawn_future_local(async move {
            let r = conn
                .call_future(
                    Some("org.gnome.Shell"),
                    BRIDGE_PATH,
                    BRIDGE_IFACE,
                    &method,
                    Some(&(id.as_str(),).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    3000,
                )
                .await;
            if r.is_err() {
                toast("The TopManager HUD extension is needed for this");
            }
        });
    }

    /// Window counts from the Shell bridge; also tells whether it is available.
    fn refresh_windows(self: &Rc<Self>) {
        let conn = self.client.connection().clone();
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = conn
                .call_future(
                    Some("org.gnome.Shell"),
                    BRIDGE_PATH,
                    BRIDGE_IFACE,
                    "GetRunningApps",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    3000,
                )
                .await;
            let Some(page) = w.upgrade() else { return };
            let parsed =
                r.ok().and_then(|v| v.get::<(String,)>()).and_then(|(j,)| serde_json::from_str::<Value>(&j).ok());
            page.bridge.set(parsed.is_some());
            page.banner.set_revealed(parsed.is_none());
            let mut windows = page.windows.borrow_mut();
            windows.clear();
            if let Some(apps) = parsed.as_ref().and_then(Value::as_array) {
                for a in apps {
                    if let (Some(id), Some(n)) = (a["id"].as_str(), a["windows"].as_u64()) {
                        windows.insert(id.to_string(), n as u32);
                    }
                }
            }
        });
    }

    fn lookup(&self, id: &str) -> Option<AppEntry> {
        self.info.borrow_mut().entry(id.to_string()).or_insert_with(|| app_info(id)).clone()
    }

    pub fn update(self: &Rc<Self>, processes: &[ProcessItem]) {
        self.refresh_windows();
        let mut groups: BTreeMap<String, AppGroup> = BTreeMap::new();
        for p in processes {
            let Some(id) = p.app_id.as_deref() else { continue };
            let g = groups.entry(id.to_string()).or_default();
            g.cpu += p.cpu_total;
            g.memory += p.memory;
            g.procs.push((p.pid, p.start_ticks, p.protected));
        }
        // Scopes without a launcher entry (helper services, NoDisplay
        // desktop files like alarm notifiers) are not apps.
        groups.retain(|id, g| match self.lookup(id) {
            Some((name, icon, true)) => {
                g.name = name;
                g.icon = icon;
                true
            }
            _ => false,
        });

        let mut rows = self.rows.borrow_mut();
        rows.retain(|id, r| {
            let keep = groups.contains_key(id);
            if !keep {
                self.list.remove(&r.row);
            }
            keep
        });
        let windows = self.windows.borrow();
        for (id, g) in &groups {
            let r = rows.entry(id.clone()).or_insert_with(|| {
                let r = make_row(id, g);
                self.list.append(&r.row);
                r
            });
            let procs = g.procs.len();
            let win = windows.get(id).copied();
            let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            let subtitle = match win {
                Some(n) => format!(
                    "{} · {} · {id}",
                    plural(procs, "process", "processes"),
                    plural(n as usize, "window", "windows")
                ),
                None => format!("{} · {id}", plural(procs, "process", "processes")),
            };
            if r.row.subtitle().as_deref() != Some(subtitle.as_str()) {
                r.row.set_subtitle(&subtitle);
            }
            r.cpu.set_label(&format!("{:.1}%", g.cpu));
            r.mem.set_label(&format_bytes(g.memory));
            let bridge = self.bridge.get();
            r.focus.set_visible(bridge);
            r.minimize.set_visible(bridge && win.unwrap_or(0) > 0);
        }
        self.list.set_visible(!groups.is_empty());
        self.empty.set_visible(groups.is_empty());
        self.list.invalidate_sort();
        *self.groups.borrow_mut() = groups;
    }
}

fn make_row(id: &str, g: &AppGroup) -> AppRow {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&g.name)).build();
    let icon = gtk::Image::builder().pixel_size(32).build();
    icon.set_from_gicon(&g.icon.clone().unwrap_or_else(generic_icon));
    row.add_prefix(&icon);

    let cpu = gtk::Label::builder().width_chars(7).xalign(1.0).css_classes(["numeric"]).build();
    let mem = gtk::Label::builder().width_chars(9).xalign(1.0).css_classes(["numeric", "dim-label"]).build();
    row.add_suffix(&cpu);
    row.add_suffix(&mem);

    let target = id.to_variant();
    let button = |icon: &str, tip: &str, action: &str| {
        let b = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .action_name(action)
            .build();
        b.set_action_target_value(Some(&target));
        b
    };
    let focus = button("go-jump-symbolic", "Switch to", "apps.focus");
    let minimize = button("window-minimize-symbolic", "Minimize windows", "apps.minimize");
    row.add_suffix(&focus);
    row.add_suffix(&minimize);

    let menu = gio::Menu::new();
    let item = |label: &str, action: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some(action), Some(&target));
        i
    };
    let s1 = gio::Menu::new();
    s1.append_item(&item("Copy App ID", "apps.copy-id"));
    let s2 = gio::Menu::new();
    s2.append_item(&item("Quit", "apps.quit"));
    s2.append_item(&item("Force Quit", "apps.force-quit"));
    menu.append_section(None, &s1);
    menu.append_section(None, &s2);
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    row.add_suffix(&more);
    AppRow { row, cpu, mem, focus, minimize }
}
