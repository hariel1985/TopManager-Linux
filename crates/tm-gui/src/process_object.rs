//! A process row as a GObject, so the column view can bind to its properties.
//!
//! Rows are updated in place every tick (keyed by pid + start time): the
//! selection and scroll position survive, and only changed text notifies.

use std::cell::RefCell;

use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use tm_core::format::{format_bytes, format_bytes_per_second};
use tm_core::model::{ProcessItem, ProcessState};

mod imp {
    use super::*;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::ProcessObject)]
    pub struct ProcessObject {
        #[property(get, set)]
        name: RefCell<String>,
        #[property(get, set)]
        pid_text: RefCell<String>,
        #[property(get, set)]
        cpu_text: RefCell<String>,
        #[property(get, set)]
        cpu_total_text: RefCell<String>,
        #[property(get, set)]
        memory_text: RefCell<String>,
        #[property(get, set)]
        disk_text: RefCell<String>,
        #[property(get, set)]
        energy_text: RefCell<String>,
        #[property(get, set)]
        threads_text: RefCell<String>,
        #[property(get, set)]
        user: RefCell<String>,
        #[property(get, set)]
        state_text: RefCell<String>,
        #[property(get, set, nullable)]
        icon: RefCell<Option<gio::Icon>>,
        pub item: RefCell<ProcessItem>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProcessObject {
        const NAME: &'static str = "TmProcessObject";
        type Type = super::ProcessObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ProcessObject {}
}

glib::wrapper! {
    pub struct ProcessObject(ObjectSubclass<imp::ProcessObject>);
}

pub fn state_label(s: ProcessState) -> &'static str {
    match s {
        ProcessState::Running => "Running",
        ProcessState::Sleeping => "Sleeping",
        ProcessState::DiskSleep => "Waiting (I/O)",
        ProcessState::Stopped => "Stopped",
        ProcessState::Zombie => "Zombie",
        ProcessState::Idle => "Idle",
        ProcessState::Unknown => "Unknown",
    }
}

fn pct(v: f64) -> String {
    if v < 0.05 {
        "0.0".into()
    } else {
        format!("{v:.1}")
    }
}

impl ProcessObject {
    pub fn new(item: &ProcessItem, icon: Option<gio::Icon>) -> Self {
        let obj: Self = glib::Object::new();
        obj.set_icon(icon);
        obj.update(item);
        obj
    }

    pub fn item(&self) -> std::cell::Ref<'_, ProcessItem> {
        self.imp().item.borrow()
    }

    pub fn key(&self) -> (u32, u64) {
        let i = self.item();
        (i.pid, i.start_ticks)
    }

    /// Refresh from a new sample; only properties whose text changed notify.
    pub fn update(&self, p: &ProcessItem) {
        macro_rules! set {
            ($get:ident, $set:ident, $val:expr) => {{
                let v: String = $val;
                if self.$get() != v {
                    self.$set(v);
                }
            }};
        }
        set!(name, set_name, p.name.clone());
        set!(pid_text, set_pid_text, p.pid.to_string());
        set!(cpu_text, set_cpu_text, pct(p.cpu));
        set!(cpu_total_text, set_cpu_total_text, pct(p.cpu_total));
        set!(memory_text, set_memory_text, format_bytes(p.memory));
        set!(
            disk_text,
            set_disk_text,
            if p.disk_total_rate() < 1.0 { "—".into() } else { format_bytes_per_second(p.disk_total_rate()) }
        );
        set!(energy_text, set_energy_text, format!("{:.1}", p.energy));
        set!(threads_text, set_threads_text, p.threads.to_string());
        set!(user, set_user, p.user.clone());
        set!(state_text, set_state_text, state_label(p.state).to_string());
        *self.imp().item.borrow_mut() = p.clone();
    }
}

/// `(display name, icon, shown in app launchers)`
pub type AppEntry = (String, Option<gio::Icon>, bool);

thread_local! {
    /// Installed apps by id, reloaded at most once a minute on a miss.
    static APPS: RefCell<(std::collections::HashMap<String, AppEntry>, Option<std::time::Instant>)> =
        RefCell::default();
}

/// Resolve a desktop app id.
pub fn app_info(app_id: &str) -> Option<AppEntry> {
    APPS.with(|apps| {
        let mut apps = apps.borrow_mut();
        let stale = apps.1.is_none_or(|t| t.elapsed().as_secs() > 60);
        if stale && !apps.0.contains_key(app_id) {
            apps.0 = gio::AppInfo::all()
                .into_iter()
                .filter_map(|a| {
                    let id = a.id()?.trim_end_matches(".desktop").to_string();
                    Some((id, (a.display_name().to_string(), a.icon(), a.should_show())))
                })
                .collect();
            apps.1 = Some(std::time::Instant::now());
        }
        apps.0.get(app_id).cloned()
    })
}

/// Fallback icon for processes that don't belong to a desktop app.
pub fn generic_icon() -> gio::Icon {
    gio::ThemedIcon::new("application-x-executable-symbolic").upcast()
}
