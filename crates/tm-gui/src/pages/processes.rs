//! Processes: sortable, searchable table with a context menu and inspector.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use tm_core::format::format_bytes;
use tm_core::model::{ProcessItem, ProcessState, Snapshot};

use crate::client::Client;
use crate::inspector;
use crate::process_object::{app_info, generic_icon, ProcessObject};
use crate::Toaster;

pub struct ProcessesPage {
    pub root: gtk::Box,
    client: Client,
    toast: Toaster,
    store: gio::ListStore,
    rows: RefCell<HashMap<(u32, u64), ProcessObject>>,
    selection: gtk::SingleSelection,
    view: gtk::ColumnView,
    filter: gtk::CustomFilter,
    query: Rc<RefCell<String>>,
    mine_only: Rc<Cell<bool>>,
    status: gtk::Label,
    selected_label: gtk::Label,
    actions: gio::SimpleActionGroup,
    menu: gtk::PopoverMenu,
    icons: RefCell<HashMap<String, Option<gio::Icon>>>,
    pub search: gtk::SearchEntry,
}

fn my_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(u32::MAX)
}

fn obj(o: &glib::Object) -> &ProcessObject {
    o.downcast_ref::<ProcessObject>().expect("ProcessObject")
}

impl ProcessesPage {
    pub fn new(client: Client, toast: Toaster) -> Rc<Self> {
        let store = gio::ListStore::new::<ProcessObject>();
        let query: Rc<RefCell<String>> = Rc::default();
        let mine_only = Rc::new(Cell::new(false));
        let uid = my_uid();
        let (q, m) = (query.clone(), mine_only.clone());
        let filter = gtk::CustomFilter::new(move |o| {
            let p = obj(o).item();
            if p.is_kernel_thread() || (m.get() && p.uid != uid) {
                return false;
            }
            let q = q.borrow();
            q.is_empty()
                || p.name.to_lowercase().contains(q.as_str())
                || p.pid.to_string().starts_with(q.as_str())
                || p.user.to_lowercase() == *q
                || p.app_id.as_deref().is_some_and(|a| a.to_lowercase().contains(q.as_str()))
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let view = gtk::ColumnView::builder().reorderable(true).show_row_separators(false).build();
        view.add_css_class("process-table");
        let sorted = gtk::SortListModel::new(Some(filtered), view.sorter());
        let selection = gtk::SingleSelection::builder().model(&sorted).autoselect(false).can_unselect(true).build();
        view.set_model(Some(&selection));

        let search =
            gtk::SearchEntry::builder().placeholder_text("Search by name, PID, user or app").hexpand(true).build();
        let mine =
            gtk::ToggleButton::builder().label("My processes").tooltip_text("Show only your own processes").build();
        let status = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
        let toolbar =
            gtk::Box::builder().spacing(8).margin_start(12).margin_end(12).margin_top(8).margin_bottom(8).build();
        toolbar.append(&search);
        toolbar.append(&mine);
        toolbar.append(&status);

        let scrolled = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();

        let selected_label = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        let bar = gtk::ActionBar::new();
        bar.pack_start(&selected_label);
        let btn = |icon: &str, tip: &str, action: &str| {
            gtk::Button::builder().icon_name(icon).tooltip_text(tip).action_name(action).build()
        };
        let end = gtk::Button::builder().label("End Process").action_name("proc.terminate").build();
        end.add_css_class("destructive-action");
        bar.pack_end(&end);
        bar.pack_end(&btn("process-stop-symbolic", "Force Quit (SIGKILL)", "proc.kill"));
        bar.pack_end(&btn("media-playback-start-symbolic", "Resume", "proc.resume"));
        bar.pack_end(&btn("media-playback-pause-symbolic", "Suspend", "proc.suspend"));
        bar.pack_end(&btn("dialog-information-symbolic", "Inspect (Ctrl+I)", "proc.inspect"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&toolbar);
        root.append(&scrolled);
        root.append(&bar);

        let menu_model = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("Inspect"), Some("proc.inspect"));
        s1.append(Some("Copy PID"), Some("proc.copy-pid"));
        let s2 = gio::Menu::new();
        s2.append(Some("Suspend"), Some("proc.suspend"));
        s2.append(Some("Resume"), Some("proc.resume"));
        let s3 = gio::Menu::new();
        s3.append(Some("End Process"), Some("proc.terminate"));
        s3.append(Some("Force Quit"), Some("proc.kill"));
        menu_model.append_section(None, &s1);
        menu_model.append_section(None, &s2);
        menu_model.append_section(None, &s3);
        let menu = gtk::PopoverMenu::from_model(Some(&menu_model));
        menu.set_parent(&view);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);

        let page = Rc::new(ProcessesPage {
            root,
            client,
            toast,
            store,
            rows: RefCell::default(),
            selection,
            view,
            filter,
            query,
            mine_only,
            status,
            selected_label,
            actions: gio::SimpleActionGroup::new(),
            menu,
            icons: RefCell::default(),
            search,
        });
        page.build_columns();
        page.build_actions();

        let w = Rc::downgrade(&page);
        page.search.connect_search_changed(move |e| {
            if let Some(p) = w.upgrade() {
                *p.query.borrow_mut() = e.text().trim().to_lowercase();
                p.filter.changed(gtk::FilterChange::Different);
            }
        });
        let w = Rc::downgrade(&page);
        mine.connect_toggled(move |b| {
            if let Some(p) = w.upgrade() {
                p.mine_only.set(b.is_active());
                p.filter.changed(gtk::FilterChange::Different);
            }
        });
        let w = Rc::downgrade(&page);
        page.selection.connect_selected_item_notify(move |_| {
            if let Some(p) = w.upgrade() {
                p.sync_actions();
            }
        });
        let w = Rc::downgrade(&page);
        page.view.connect_activate(move |_, _| {
            if let Some(p) = w.upgrade() {
                p.inspect();
            }
        });
        page.sync_actions();
        page
    }

    fn build_columns(self: &Rc<Self>) {
        type Cmp = fn(&ProcessItem, &ProcessItem) -> std::cmp::Ordering;
        let cols: [(&str, &str, bool, i32, Cmp); 10] = [
            ("Name", "name", false, 260, |a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            ("PID", "pid-text", true, 80, |a, b| a.pid.cmp(&b.pid)),
            ("CPU %", "cpu-text", true, 80, |a, b| a.cpu.total_cmp(&b.cpu)),
            ("CPU (all) %", "cpu-total-text", true, 100, |a, b| a.cpu_total.total_cmp(&b.cpu_total)),
            ("Memory", "memory-text", true, 100, |a, b| a.memory.cmp(&b.memory)),
            ("Disk", "disk-text", true, 100, |a, b| a.disk_total_rate().total_cmp(&b.disk_total_rate())),
            ("Energy", "energy-text", true, 80, |a, b| a.energy.total_cmp(&b.energy)),
            ("Threads", "threads-text", true, 80, |a, b| a.threads.cmp(&b.threads)),
            ("User", "user", false, 100, |a, b| a.user.cmp(&b.user)),
            ("State", "state-text", false, 110, |a, b| (a.state as u8).cmp(&(b.state as u8))),
        ];
        let mut cpu_col = None;
        for (i, (title, prop, numeric, width, cmp)) in cols.into_iter().enumerate() {
            let factory = gtk::SignalListItemFactory::new();
            let page = Rc::downgrade(self);
            let prop = prop.to_string();
            factory.connect_setup(move |_, li| {
                let li = li.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let label = gtk::Label::builder()
                    .xalign(if numeric { 1.0 } else { 0.0 })
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .hexpand(true)
                    .build();
                if numeric {
                    label.add_css_class("numeric");
                }
                li.property_expression("item").chain_property::<ProcessObject>(&prop).bind(
                    &label,
                    "label",
                    gtk::Widget::NONE,
                );
                let cell: gtk::Widget = if i == 0 {
                    let icon = gtk::Image::builder().pixel_size(16).build();
                    li.property_expression("item").chain_property::<ProcessObject>("icon").bind(
                        &icon,
                        "gicon",
                        gtk::Widget::NONE,
                    );
                    let b = gtk::Box::builder().spacing(8).build();
                    b.append(&icon);
                    b.append(&label);
                    b.upcast()
                } else {
                    label.upcast()
                };
                attach_context_menu(&cell, li, page.clone());
                li.set_child(Some(&cell));
            });
            let col = gtk::ColumnViewColumn::builder()
                .title(title)
                .factory(&factory)
                .resizable(true)
                .fixed_width(width)
                .expand(i == 0)
                .build();
            col.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| {
                let (a, b) = (obj(a).item(), obj(b).item());
                cmp(&a, &b).then(a.pid.cmp(&b.pid)).into()
            })));
            if i == 2 {
                cpu_col = Some(col.clone());
            }
            self.view.append_column(&col);
        }
        self.view.sort_by_column(cpu_col.as_ref(), gtk::SortType::Descending);
    }

    fn build_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<ProcessesPage>)| {
            let a = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(p) = w.upgrade() {
                    f(&p);
                }
            });
            self.actions.add_action(&a);
        };
        add("inspect", |p| p.inspect());
        add("terminate", |p| p.signal("term"));
        add("kill", |p| p.confirm_kill());
        add("suspend", |p| p.signal("stop"));
        add("resume", |p| p.signal("cont"));
        add("copy-pid", |p| {
            if let Some(o) = p.selected() {
                p.root.clipboard().set_text(&o.item().pid.to_string());
            }
        });
        self.root.insert_action_group("proc", Some(&self.actions));

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Managed);
        for (trigger, action) in [("Delete", "proc.terminate"), ("<Control>i", "proc.inspect")] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.root.add_controller(shortcuts);
    }

    fn selected(&self) -> Option<ProcessObject> {
        self.selection.selected_item().and_downcast::<ProcessObject>()
    }

    fn sync_actions(&self) {
        let sel = self.selected();
        let item = sel.as_ref().map(|o| o.item().clone());
        let enable = |name: &str, on: bool| {
            if let Some(a) = self.actions.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        };
        let has = item.is_some();
        let protected = item.as_ref().is_some_and(|p| p.protected);
        let stopped = item.as_ref().is_some_and(|p| p.state == ProcessState::Stopped);
        enable("inspect", has);
        enable("copy-pid", has);
        enable("terminate", has && !protected);
        enable("kill", has && !protected);
        enable("suspend", has && !protected && !stopped);
        enable("resume", has && stopped);
        self.selected_label.set_label(&match &item {
            Some(p) if p.protected => format!("{} (PID {}) — needed by your desktop session", p.name, p.pid),
            Some(p) => format!("{} (PID {})", p.name, p.pid),
            None => String::new(),
        });
    }

    fn inspect(&self) {
        if let Some(o) = self.selected() {
            inspector::show(&self.root, self.client.clone(), self.toast.clone(), o.item().clone());
        }
    }

    fn signal(&self, action: &'static str) {
        let Some(o) = self.selected() else { return };
        let p = o.item().clone();
        let (client, toast) = (self.client.clone(), self.toast.clone());
        glib::spawn_future_local(async move {
            if let Err(e) = client.signal_process(p.pid, p.start_ticks, action).await {
                toast(&format!("{}: {e}", p.name));
            }
        });
    }

    fn confirm_kill(self: &Rc<Self>) {
        let Some(o) = self.selected() else { return };
        let name = o.item().name.clone();
        let dialog = adw::AlertDialog::new(
            Some(&format!("Force quit “{name}”?")),
            Some("The process is stopped immediately. Unsaved work in it will be lost."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("kill", "Force Quit")]);
        dialog.set_response_appearance("kill", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let w = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            if r == "kill" {
                if let Some(p) = w.upgrade() {
                    p.signal("kill");
                }
            }
        });
        dialog.present(Some(&self.root));
    }

    fn icon_for(&self, app_id: Option<&str>) -> Option<gio::Icon> {
        let Some(id) = app_id else { return Some(generic_icon()) };
        self.icons
            .borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| app_info(id).and_then(|(_, i, _)| i))
            .clone()
            .or_else(|| Some(generic_icon()))
    }

    /// Merge a new process list into the table.
    pub fn update(&self, processes: &[ProcessItem], snap: &Snapshot) {
        let mut rows = self.rows.borrow_mut();
        let mut seen = HashSet::with_capacity(processes.len());
        let mut added = Vec::new();
        for p in processes {
            let key = (p.pid, p.start_ticks);
            seen.insert(key);
            match rows.get(&key) {
                Some(o) => o.update(p),
                None => {
                    let o = ProcessObject::new(p, self.icon_for(p.app_id.as_deref()));
                    rows.insert(key, o.clone());
                    added.push(o);
                }
            }
        }
        // Remove exited processes, from the back so positions stay valid.
        for i in (0..self.store.n_items()).rev() {
            if let Some(o) = self.store.item(i).and_downcast::<ProcessObject>() {
                if !seen.contains(&o.key()) {
                    rows.remove(&o.key());
                    self.store.remove(i);
                }
            }
        }
        self.store.extend_from_slice(&added);
        drop(rows);

        // Values changed in place: re-apply the filter and the sort order.
        self.filter.changed(gtk::FilterChange::Different);
        if let Some(sorter) = self.view.sorter() {
            sorter.changed(gtk::SorterChange::Different);
        }
        self.sync_actions();

        let visible = self.selection.n_items();
        // System-wide figures, the same ones the HUD shows. The processes'
        // own private memory is smaller: shared memory (tmpfs, GPU and app
        // buffers) and the kernel's memory belong to no single process.
        let m = &snap.memory;
        let private: u64 = processes.iter().map(|p| p.memory).sum();
        self.status.set_label(&format!(
            "{visible} processes · CPU {:.0}% · Memory {} used ({:.0}%), processes {}",
            snap.cpu.global_usage,
            format_bytes(m.used),
            m.usage_percentage(),
            format_bytes(private)
        ));
        self.status.set_tooltip_text(Some(&format!(
            "Used memory: {} of {} (what the system can't hand out without reclaiming).\n\
             Private memory of all processes: {}.\n\
             The rest is shared memory ({}: tmpfs, graphics and app buffers) and the kernel's own memory.",
            format_bytes(m.used),
            format_bytes(m.total),
            format_bytes(private),
            format_bytes(m.shmem)
        )));
    }

    fn show_menu(&self, cell: &gtk::Widget, x: f64, y: f64, position: u32) {
        self.selection.set_selected(position);
        if let Some(p) = cell.compute_point(&self.view, &gtk::graphene::Point::new(x as f32, y as f32)) {
            self.menu.set_pointing_to(Some(&gtk::gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
            self.menu.popup();
        }
    }
}

fn attach_context_menu(cell: &gtk::Widget, li: &gtk::ListItem, page: Weak<ProcessesPage>) {
    let gesture = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_SECONDARY).build();
    let li = li.downgrade();
    let c = cell.downgrade();
    gesture.connect_pressed(move |g, _, x, y| {
        let (Some(li), Some(cell), Some(page)) = (li.upgrade(), c.upgrade(), page.upgrade()) else { return };
        g.set_state(gtk::EventSequenceState::Claimed);
        page.show_menu(&cell, x, y, li.position());
    });
    cell.add_controller(gesture);
}
