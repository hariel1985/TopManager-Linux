//! The main window: header with a view switcher, the four pages, an offline
//! state, and toasts.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::client::{Client, Update};
use crate::pages::apps::AppsPage;
use crate::pages::performance::PerformancePage;
use crate::pages::power::PowerPage;
use crate::pages::processes::ProcessesPage;
use crate::{preferences, Toaster};

struct Pages {
    client: Client,
    processes: Rc<ProcessesPage>,
    apps: Rc<AppsPage>,
    performance: Rc<PerformancePage>,
    power: Rc<PowerPage>,
}

thread_local! {
    static STACKS: RefCell<Vec<(glib::WeakRef<adw::ApplicationWindow>, adw::ViewStack)>> = const { RefCell::new(Vec::new()) };
}

pub fn show_page(win: &adw::ApplicationWindow, name: &str) {
    STACKS.with(|s| {
        for (w, stack) in s.borrow().iter() {
            if w.upgrade().as_ref() == Some(win) && stack.child_by_name(name).is_some() {
                stack.set_visible_child_name(name);
            }
        }
    });
}

fn offline_page(client: &Client, toast: Toaster) -> adw::StatusPage {
    let start = gtk::Button::builder().label("Start Service").halign(gtk::Align::Center).build();
    start.add_css_class("pill");
    start.add_css_class("suggested-action");
    let c = client.clone();
    start.connect_clicked(move |_| {
        let (c, toast) = (c.clone(), toast.clone());
        glib::spawn_future_local(async move {
            if let Err(e) = c.start_service().await {
                toast(&format!("Could not start topmanagerd: {e}"));
            }
        });
    });
    adw::StatusPage::builder()
        .icon_name(crate::APP_ID)
        .title("TopManager service is not running")
        .description("The window shows data collected by the topmanagerd service.")
        .child(&start)
        .build()
}

pub fn build(app: &adw::Application) -> adw::ApplicationWindow {
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("TopManager")
        .default_width(1120)
        .default_height(740)
        .width_request(360)
        .height_request(400)
        .build();

    let toasts = adw::ToastOverlay::new();
    let t = toasts.downgrade();
    let toast: Toaster = Rc::new(move |msg: &str| {
        if let Some(t) = t.upgrade() {
            t.add_toast(adw::Toast::new(msg));
        }
    });

    let client = match Client::connect() {
        Ok(c) => c,
        Err(e) => {
            let page = adw::StatusPage::builder()
                .icon_name("dialog-error-symbolic")
                .title("No session bus")
                .description(e.to_string())
                .build();
            win.set_content(Some(&page));
            return win;
        }
    };

    let pages = Rc::new(Pages {
        client: client.clone(),
        processes: ProcessesPage::new(client.clone(), toast.clone()),
        apps: AppsPage::new(client.clone(), toast.clone()),
        performance: PerformancePage::new(client.clone()),
        power: PowerPage::new(client.clone(), toast.clone()),
    });

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(&pages.processes.root, Some("processes"), "Processes", "view-list-symbolic");
    stack.add_titled_with_icon(&pages.apps.root, Some("apps"), "Apps", "view-app-grid-symbolic");
    stack.add_titled_with_icon(
        &pages.performance.root,
        Some("performance"),
        "Performance",
        "power-profile-performance-symbolic",
    );
    stack.add_titled_with_icon(&pages.power.root, Some("power"), "Power & Storage", "drive-harddisk-symbolic");

    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();

    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("About TopManager"), Some("win.about"));
    let menu_button =
        gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).build();
    header.pack_end(&menu_button);

    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();

    let offline = offline_page(&client, toast.clone());
    let content = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    content.add_named(&stack, Some("online"));
    content.add_named(&offline, Some("offline"));
    content.set_visible_child_name("offline");

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    toolbar.add_bottom_bar(&switcher_bar);
    toasts.set_child(Some(&toolbar));
    win.set_content(Some(&toasts));

    // Narrow windows: move the switcher to the bottom.
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        720.0,
        adw::LengthUnit::Sp,
    ));
    bp.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
    bp.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
    win.add_breakpoint(bp);

    // Window actions.
    let (w, c, tst) = (win.downgrade(), client.clone(), toast.clone());
    let prefs = gio::ActionEntry::builder("preferences")
        .activate(move |_: &adw::ApplicationWindow, _, _| {
            if let Some(w) = w.upgrade() {
                preferences::show(&w, c.clone(), tst.clone());
            }
        })
        .build();
    let w = win.downgrade();
    let about = gio::ActionEntry::builder("about")
        .activate(move |_: &adw::ApplicationWindow, _, _| {
            if let Some(w) = w.upgrade() {
                show_about(&w);
            }
        })
        .build();
    let (s, p) = (stack.downgrade(), pages.processes.search.downgrade());
    let search = gio::ActionEntry::builder("search")
        .activate(move |_: &adw::ApplicationWindow, _, _| {
            if let (Some(s), Some(p)) = (s.upgrade(), p.upgrade()) {
                s.set_visible_child_name("processes");
                p.grab_focus();
            }
        })
        .build();
    win.add_action_entries([prefs, about, search]);

    // Data flow: the client pushes updates; each page renders what it needs.
    let (pg, cs, st) = (pages.clone(), content.downgrade(), stack.downgrade());
    client.listen(move |u| match u {
        Update::Online(on) => {
            if let Some(cs) = cs.upgrade() {
                cs.set_visible_child_name(if *on { "online" } else { "offline" });
            }
        }
        Update::Data => {
            if let Some(stack) = st.upgrade() {
                pg.render(stack.visible_child_name().as_deref());
            }
        }
        Update::Settings => {}
    });
    let pg = pages.clone();
    stack.connect_visible_child_name_notify(move |s| pg.render(s.visible_child_name().as_deref()));

    STACKS.with(|s| s.borrow_mut().push((win.downgrade(), stack.clone())));
    win
}

impl Pages {
    /// Render the visible page every tick; the others catch up when shown.
    /// The performance page also records history in the background.
    fn render(&self, visible: Option<&str>) {
        let data = self.client.data();
        let Some(snap) = data.snapshot.as_ref() else { return };
        self.performance.record(snap);
        match visible {
            Some("processes") => self.processes.update(&data.processes, snap.memory.total),
            Some("apps") => self.apps.update(&data.processes),
            Some("performance") => self.performance.render(snap),
            Some("power") => self.power.update(snap, data.summary.as_ref()),
            _ => {}
        }
    }
}

fn show_about(win: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("TopManager")
        .application_icon(crate::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("hariel1985")
        .website("https://github.com/hariel1985/TopManager-Linux")
        .issue_url("https://github.com/hariel1985/TopManager-Linux/issues")
        .license_type(gtk::License::Gpl30)
        .comments("System monitor with a top-bar HUD, health score and alerts.")
        .build();
    about.present(Some(win));
}
