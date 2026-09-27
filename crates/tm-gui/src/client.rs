//! Connection to topmanagerd over the session bus (GIO D-Bus, so everything
//! runs on the GTK main loop).
//!
//! The window holds a "full" subscription lease while it is open, which makes
//! the daemon collect per-process disk I/O and wakeups. On every `Tick` the
//! client fetches the snapshot and process list and hands them to listeners.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;
use tm_core::alert::SystemAlert;
use tm_core::bus;
use tm_core::metrics::MetricsSample;
use tm_core::model::{ProcessDetail, ProcessItem, Snapshot};
use tm_core::settings::Settings;

/// Renew the subscription well within the daemon's 15 s lease.
const LEASE_RENEW_SECS: u32 = 8;

#[derive(Clone)]
pub enum Update {
    Online(bool),
    /// New snapshot + processes + summary are available in `Client::data()`.
    Data,
    Settings,
}

#[derive(Default)]
pub struct Data {
    pub summary: Option<Value>,
    pub snapshot: Option<Snapshot>,
    pub processes: Vec<ProcessItem>,
    pub settings: Option<Settings>,
    pub seq: u64,
}

type Listener = Box<dyn Fn(&Update)>;

struct Inner {
    conn: gio::DBusConnection,
    data: RefCell<Data>,
    online: Cell<bool>,
    fetching: Cell<bool>,
    lease: Cell<u32>,
    listeners: RefCell<Vec<Listener>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    unwatch: RefCell<Option<Box<dyn FnOnce()>>>,
}

#[derive(Clone)]
pub struct Client(Rc<Inner>);

impl Client {
    pub fn connect() -> Result<Self, glib::Error> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
        let client = Client(Rc::new(Inner {
            conn,
            data: RefCell::default(),
            online: Cell::new(false),
            fetching: Cell::new(false),
            lease: Cell::new(0),
            listeners: RefCell::default(),
            subscriptions: RefCell::default(),
            unwatch: RefCell::default(),
        }));
        client.start();
        Ok(client)
    }

    fn start(&self) {
        let weak = Rc::downgrade(&self.0);
        let tick = self.0.conn.subscribe_to_signal(
            Some(bus::NAME),
            Some(bus::INTERFACE),
            Some("Tick"),
            Some(bus::PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |sig| {
                if let Some(inner) = weak.upgrade() {
                    let client = Client(inner);
                    if let Some((json,)) = sig.parameters.get::<(String,)>() {
                        client.on_tick(&json);
                    }
                }
            },
        );
        let weak = Rc::downgrade(&self.0);
        let settings = self.0.conn.subscribe_to_signal(
            Some(bus::NAME),
            Some(bus::INTERFACE),
            Some("SettingsChanged"),
            Some(bus::PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |sig| {
                if let Some(inner) = weak.upgrade() {
                    let client = Client(inner);
                    if let Some((json,)) = sig.parameters.get::<(String,)>() {
                        client.0.data.borrow_mut().settings = serde_json::from_str(&json).ok();
                        client.emit(&Update::Settings);
                    }
                }
            },
        );
        self.0.subscriptions.borrow_mut().extend([tick, settings]);

        // AUTO_START D-Bus-activates topmanagerd when it isn't running.
        let (w1, w2) = (Rc::downgrade(&self.0), Rc::downgrade(&self.0));
        let id = gio::bus_watch_name_on_connection(
            &self.0.conn,
            bus::NAME,
            gio::BusNameWatcherFlags::AUTO_START,
            move |_, _, _| {
                if let Some(inner) = w1.upgrade() {
                    Client(inner).set_online(true);
                }
            },
            move |_, _| {
                if let Some(inner) = w2.upgrade() {
                    Client(inner).set_online(false);
                }
            },
        );
        *self.0.unwatch.borrow_mut() = Some(Box::new(move || gio::bus_unwatch_name(id)));

        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_seconds_local(LEASE_RENEW_SECS, move || match weak.upgrade() {
            Some(inner) => {
                Client(inner).renew_lease();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    pub fn listen(&self, f: impl Fn(&Update) + 'static) {
        self.0.listeners.borrow_mut().push(Box::new(f));
    }

    fn emit(&self, update: &Update) {
        for l in self.0.listeners.borrow().iter() {
            l(update);
        }
    }

    pub fn data(&self) -> std::cell::Ref<'_, Data> {
        self.0.data.borrow()
    }

    pub fn is_online(&self) -> bool {
        self.0.online.get()
    }

    fn set_online(&self, online: bool) {
        self.0.online.set(online);
        self.emit(&Update::Online(online));
        if online {
            self.0.lease.set(0);
            self.renew_lease();
            self.fetch();
            let c = self.clone();
            glib::spawn_future_local(async move {
                if let Ok(s) = c.settings().await {
                    c.0.data.borrow_mut().settings = Some(s);
                    c.emit(&Update::Settings);
                }
            });
        }
    }

    fn renew_lease(&self) {
        if !self.is_online() {
            return;
        }
        let c = self.clone();
        glib::spawn_future_local(async move {
            let lease = c.0.lease.get();
            if let Ok(v) = c.call("Subscribe", Some(("full", lease).to_variant())).await {
                if let Some((id,)) = v.get::<(u32,)>() {
                    c.0.lease.set(id);
                }
            }
        });
    }

    fn on_tick(&self, json: &str) {
        if let Ok(v) = serde_json::from_str::<Value>(json) {
            self.0.data.borrow_mut().summary = Some(v);
        }
        self.fetch();
    }

    /// Fetch snapshot + processes; skipped while a previous fetch is running.
    fn fetch(&self) {
        if self.0.fetching.replace(true) {
            return;
        }
        let c = self.clone();
        glib::spawn_future_local(async move {
            let snap = c.call_json("GetSnapshot", None).await;
            let procs = c.call_json("GetProcesses", None).await;
            if c.0.data.borrow().summary.is_none() {
                if let Ok(s) = c.call_json("GetSummary", None).await {
                    c.0.data.borrow_mut().summary = Some(s);
                }
            }
            c.0.fetching.set(false);
            let mut changed = false;
            {
                let mut d = c.0.data.borrow_mut();
                if let Ok(s) = snap {
                    if s.get("ready") != Some(&Value::Bool(false)) {
                        if let Ok(snapshot) = serde_json::from_value::<Snapshot>(s) {
                            d.snapshot = Some(snapshot);
                            changed = true;
                        }
                    }
                }
                if let Ok(p) = procs {
                    d.seq = p.get("seq").and_then(Value::as_u64).unwrap_or(d.seq);
                    if let Some(list) = p.get("processes").cloned() {
                        if let Ok(list) = serde_json::from_value::<Vec<ProcessItem>>(list) {
                            d.processes = list;
                        }
                    }
                }
            }
            if changed {
                c.emit(&Update::Data);
            }
        });
    }

    pub async fn call(&self, method: &str, params: Option<glib::Variant>) -> Result<glib::Variant, glib::Error> {
        self.0
            .conn
            .call_future(
                Some(bus::NAME),
                bus::PATH,
                bus::INTERFACE,
                method,
                params.as_ref(),
                None,
                gio::DBusCallFlags::NONE,
                10_000,
            )
            .await
    }

    async fn call_json(&self, method: &str, params: Option<glib::Variant>) -> Result<Value, String> {
        let v = self.call(method, params).await.map_err(|e| e.to_string())?;
        let (json,) = v.get::<(String,)>().ok_or("unexpected reply type")?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    }

    /// `action`: term, kill, stop, cont.
    pub async fn signal_process(&self, pid: u32, start_ticks: u64, action: &str) -> Result<(), String> {
        let v = self
            .call("SignalProcess", Some((pid, start_ticks, action).to_variant()))
            .await
            .map_err(|e| e.to_string())?;
        match v.get::<(bool, String)>() {
            Some((true, _)) => Ok(()),
            Some((false, err)) => Err(err),
            None => Err("unexpected reply".into()),
        }
    }

    pub async fn detail(&self, pid: u32) -> Result<ProcessDetail, String> {
        let v = self.call_json("GetProcessDetail", Some((pid,).to_variant())).await?;
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    pub async fn history(&self, range: &str) -> Result<Vec<MetricsSample>, String> {
        let v = self.call_json("GetHistory", Some((range,).to_variant())).await?;
        serde_json::from_value(v.get("samples").cloned().unwrap_or_default()).map_err(|e| e.to_string())
    }

    pub async fn alerts(&self) -> Result<Vec<SystemAlert>, String> {
        let v = self.call_json("GetAlerts", None).await?;
        serde_json::from_value(v.get("inbox").cloned().unwrap_or_default()).map_err(|e| e.to_string())
    }

    pub async fn clear_alerts(&self) -> Result<(), String> {
        self.call("ClearAlerts", None).await.map(|_| ()).map_err(|e| e.to_string())
    }

    pub async fn settings(&self) -> Result<Settings, String> {
        let v = self.call_json("GetSettings", None).await?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    pub async fn set_setting(&self, key: &str, value: Value) -> Result<(), String> {
        let v =
            self.call("SetSetting", Some((key, value.to_string()).to_variant())).await.map_err(|e| e.to_string())?;
        match v.get::<(bool, String)>() {
            Some((true, _)) => Ok(()),
            Some((false, err)) => Err(err),
            None => Err("unexpected reply".into()),
        }
    }

    /// Ask the bus to start topmanagerd (D-Bus activation through systemd).
    pub async fn start_service(&self) -> Result<(), String> {
        self.0
            .conn
            .call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "StartServiceByName",
                Some(&(bus::NAME, 0u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                10_000,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.0.conn
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.borrow_mut().take() {
            unwatch();
        }
    }
}
