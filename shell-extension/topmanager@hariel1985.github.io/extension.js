// TopManager HUD — the top-bar indicator.
//
// This runs inside gnome-shell (the compositor), so it does no measuring of
// its own: topmanagerd samples the system and pushes a compact `Tick` signal;
// the HUD only renders it. The panel label is touched only when its text
// changes, and the dropdown only while it is open.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension, gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';

import {formatBytes, formatMinutes, formatRate, healthClass} from './lib/format.js';

const BUS_NAME = 'io.github.hariel1985.TopManager';
const OBJECT_PATH = '/io/github/hariel1985/TopManager';
const INTERFACE = 'io.github.hariel1985.TopManager1';
const GUI_DESKTOP_IDS = ['io.github.hariel1985.TopManager.desktop', 'org.gnome.SystemMonitor.desktop', 'gnome-system-monitor.desktop'];

function callDaemon(method, params, replyType, callback) {
    Gio.DBus.session.call(
        BUS_NAME, OBJECT_PATH, INTERFACE, method, params,
        replyType ? new GLib.VariantType(replyType) : null,
        Gio.DBusCallFlags.NONE, -1, null,
        (conn, res) => {
            try {
                const reply = conn.call_finish(res);
                callback?.(reply ? reply.deepUnpack() : null, null);
            } catch (e) {
                callback?.(null, e);
            }
        });
}

/** A small filled line chart drawn with the widget's foreground color. */
const Sparkline = GObject.registerClass(
class Sparkline extends St.DrawingArea {
    _init(params = {}) {
        super._init({style_class: 'tm-sparkline', ...params});
        this._values = [];
        this._max = 100;
        this.connect('repaint', () => this._draw());
    }

    setValues(values, max = null) {
        this._values = values ?? [];
        this._max = max ?? Math.max(1024, ...this._values);
        this.queue_repaint();
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const c = this.get_theme_node().get_foreground_color();
        const v = this._values;
        if (v.length > 1 && w > 0 && h > 0) {
            const step = w / (v.length - 1);
            const y = x => h - 1 - Math.min(1, Math.max(0, x / this._max)) * (h - 2);
            cr.moveTo(0, y(v[0]));
            v.forEach((x, i) => cr.lineTo(i * step, y(x)));
            cr.setSourceRGBA(c.red / 255, c.green / 255, c.blue / 255, 0.95);
            cr.setLineWidth(1.25);
            cr.strokePreserve();
            cr.lineTo(w, h);
            cr.lineTo(0, h);
            cr.closePath();
            cr.setSourceRGBA(c.red / 255, c.green / 255, c.blue / 255, 0.18);
            cr.fill();
        }
        cr.$dispose();
    }
});

/** label ········ value [sparkline] */
function metricRow(title, withSpark) {
    const box = new St.BoxLayout({style_class: 'tm-row', x_expand: true});
    const name = new St.Label({text: title, style_class: 'tm-row-title', y_align: Clutter.ActorAlign.CENTER});
    const value = new St.Label({text: '—', style_class: 'tm-row-value', x_expand: true, x_align: Clutter.ActorAlign.END, y_align: Clutter.ActorAlign.CENTER});
    box.add_child(name);
    box.add_child(value);
    let spark = null;
    if (withSpark) {
        spark = new Sparkline({y_align: Clutter.ActorAlign.CENTER});
        box.add_child(spark);
    }
    return {box, value, spark};
}

const Indicator = GObject.registerClass(
class Indicator extends PanelMenu.Button {
    _init(extension) {
        super._init(0.0, 'TopManager HUD', false);
        this._extension = extension;
        this._data = null;
        this._online = false;
        this._labelText = null;

        // ---- panel ----
        const panelBox = new St.BoxLayout({style_class: 'panel-status-menu-box tm-panel'});
        this._panelIcon = new St.Icon({icon_name: 'utilities-system-monitor-symbolic', style_class: 'system-status-icon'});
        this._panelLabel = new St.Label({text: '—', style_class: 'tm-panel-label', y_align: Clutter.ActorAlign.CENTER});
        this._panelSpark = new Sparkline({style_class: 'tm-sparkline tm-panel-spark', y_align: Clutter.ActorAlign.CENTER, visible: false});
        this._badge = new St.Widget({style_class: 'tm-badge', visible: false, y_align: Clutter.ActorAlign.START});
        panelBox.add_child(this._panelIcon);
        panelBox.add_child(this._panelLabel);
        panelBox.add_child(this._panelSpark);
        panelBox.add_child(this._badge);
        this.add_child(panelBox);

        // ---- dropdown ----
        const content = new St.BoxLayout({vertical: true, style_class: 'tm-menu', x_expand: true});
        const item = new PopupMenu.PopupBaseMenuItem({reactive: false, can_focus: false, style_class: 'tm-menu-item'});
        item.add_child(content);
        this.menu.addMenuItem(item);

        const header = new St.BoxLayout({style_class: 'tm-header'});
        this._score = new St.Label({text: '—', style_class: 'tm-score', y_align: Clutter.ActorAlign.CENTER});
        const headerText = new St.BoxLayout({vertical: true, x_expand: true, y_align: Clutter.ActorAlign.CENTER});
        this._rating = new St.Label({text: _('Connecting…'), style_class: 'tm-rating'});
        this._diagnosis = new St.Label({text: '', style_class: 'tm-diagnosis'});
        this._diagnosis.clutter_text.line_wrap = true;
        headerText.add_child(this._rating);
        headerText.add_child(this._diagnosis);
        header.add_child(this._score);
        header.add_child(headerText);
        content.add_child(header);

        this._rows = {
            cpu: metricRow(_('CPU'), true),
            mem: metricRow(_('Memory'), true),
            down: metricRow(_('Download'), true),
            up: metricRow(_('Upload'), true),
            gpu: metricRow(_('GPU'), false),
            battery: metricRow(_('Battery'), false),
            thermal: metricRow(_('Temperature'), false),
        };
        const metrics = new St.BoxLayout({vertical: true, style_class: 'tm-section'});
        Object.values(this._rows).forEach(r => metrics.add_child(r.box));
        content.add_child(metrics);

        // top processes
        const topHeader = new St.BoxLayout({style_class: 'tm-section-header'});
        topHeader.add_child(new St.Label({text: _('Top processes'), style_class: 'tm-caption', x_expand: true, y_align: Clutter.ActorAlign.CENTER}));
        this._sortCpu = new St.Button({label: _('CPU'), style_class: 'tm-chip', toggle_mode: true});
        this._sortMem = new St.Button({label: _('Memory'), style_class: 'tm-chip', toggle_mode: true});
        this._sortCpu.connect('clicked', () => this._setTopSort('cpu'));
        this._sortMem.connect('clicked', () => this._setTopSort('memory'));
        topHeader.add_child(this._sortCpu);
        topHeader.add_child(this._sortMem);
        content.add_child(topHeader);
        this._topBox = new St.BoxLayout({vertical: true, style_class: 'tm-section'});
        content.add_child(this._topBox);

        // alerts
        this._alertsHeader = new St.Label({text: _('Recent alerts'), style_class: 'tm-caption tm-section-header'});
        this._alertsBox = new St.BoxLayout({vertical: true, style_class: 'tm-section'});
        content.add_child(this._alertsHeader);
        content.add_child(this._alertsBox);

        this._status = new St.Label({text: '', style_class: 'tm-status', visible: false});
        this._status.clutter_text.line_wrap = true;
        content.add_child(this._status);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._startItem = this.menu.addAction(_('Start TopManager service'), () => this._startService());
        const gui = GUI_DESKTOP_IDS.map(id => Gio.DesktopAppInfo.new(id)).find(a => a);
        if (gui) {
            const label = gui.get_id() === GUI_DESKTOP_IDS[0] ? _('Open TopManager') : _('Open System Monitor');
            this.menu.addAction(label, () => gui.launch([], global.create_app_launch_context(0, -1)));
        }
        this.menu.addAction(_('Settings'), () => this._extension.openPreferences());

        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._renderMenu();
        });

        this._renderOffline();
    }

    setOnline(online) {
        this._online = online;
        this._startItem.visible = !online;
        if (!online) {
            this._data = null;
            this._renderOffline();
        }
    }

    update(data) {
        if (!data || data.api !== 1 || !data.ready)
            return;
        this._data = data;
        this._renderPanel();
        if (this.menu.isOpen)
            this._renderMenu();
    }

    _renderOffline() {
        this._setLabel('—');
        this._panelSpark.visible = false;
        this._badge.visible = false;
        this._score.text = '—';
        this._score.style_class = 'tm-score';
        this._rating.text = this._online ? _('Waiting for data…') : _('TopManager service is not running');
        this._diagnosis.text = '';
    }

    _setLabel(text) {
        // Only touch the actor when the visible text changes: every change
        // relayouts the panel.
        if (text !== this._labelText) {
            this._labelText = text;
            this._panelLabel.text = text;
        }
    }

    _renderPanel() {
        const d = this._data;
        const hud = d.hud ?? {};
        let text;
        let spark = d.spark.cpu;
        let sparkMax = 100;
        switch (hud.metric) {
        case 'memory':
            text = `${Math.round(d.mem.pct)}%`;
            spark = d.spark.mem;
            break;
        case 'health':
            text = `♥ ${d.health.score}`;
            break;
        case 'download':
            text = `↓ ${formatRate(d.net.down)}`;
            spark = d.spark.down;
            sparkMax = null;
            break;
        case 'cpu_memory':
            text = `CPU ${Math.round(d.cpu)}% · RAM ${Math.round(d.mem.pct)}%`;
            break;
        default:
            text = `${Math.round(d.cpu)}%`;
        }
        this._panelIcon.visible = hud.metric !== 'cpu_memory' && hud.metric !== 'health';
        this._setLabel(text);
        this._panelLabel.style_class = hud.metric === 'health'
            ? `tm-panel-label ${healthClass(d.health.score)}` : 'tm-panel-label';
        this._badge.visible = d.alerts.active > 0;
        this._panelSpark.visible = !!hud.show_sparkline;
        if (hud.show_sparkline)
            this._panelSpark.setValues(spark, sparkMax);
    }

    _renderMenu() {
        const d = this._data;
        if (!d) {
            this._renderOffline();
            return;
        }
        this._score.text = `${d.health.score}`;
        this._score.style_class = `tm-score ${healthClass(d.health.score)}`;
        this._rating.text = `${_('Health')}: ${_(d.health.rating)}` +
            (d.alerts.active > 0 ? ` · ${d.alerts.active} ${_('active alerts')}` : '');
        this._diagnosis.text = d.health.diagnosis.length ? d.health.diagnosis.join('\n') : _('All good.');

        const r = this._rows;
        r.cpu.value.text = `${d.cpu.toFixed(1)}%`;
        r.cpu.spark.setValues(d.spark.cpu, 100);
        r.mem.value.text = `${d.mem.pct.toFixed(1)}%  ${formatBytes(d.mem.used)}`;
        r.mem.spark.setValues(d.spark.mem, 100);
        r.down.value.text = formatRate(d.net.down);
        r.up.value.text = formatRate(d.net.up);
        const netMax = Math.max(1024, ...d.spark.down, ...d.spark.up);
        r.down.spark.setValues(d.spark.down, netMax);
        r.up.spark.setValues(d.spark.up, netMax);

        const gpu = d.gpu;
        const gpuText = gpu ? [
            gpu.util != null ? `${Math.round(gpu.util)}%` : null,
            gpu.vram_used != null ? formatBytes(gpu.vram_used) : null,
        ].filter(Boolean).join('  ') : '';
        r.gpu.box.visible = !!gpuText;
        r.gpu.value.text = gpuText;

        const b = d.battery;
        r.battery.box.visible = !!b;
        if (b) {
            const extra = b.charging ? (b.ttf ? ` · ${_('full in')} ${formatMinutes(b.ttf)}` : ` · ${_('charging')}`)
                : b.tte ? ` · ${formatMinutes(b.tte)}` : b.plugged ? ` · ${_('plugged in')}` : '';
            r.battery.value.text = `${b.pct}%${extra}`;
        }
        r.thermal.box.visible = d.thermal.temp != null;
        if (d.thermal.temp != null)
            r.thermal.value.text = `${Math.round(d.thermal.temp)} °C`;

        this._renderTop();
        this._renderAlerts();
    }

    _renderTop() {
        const d = this._data;
        const byMem = d.hud?.top_sort === 'memory';
        this._sortCpu.checked = !byMem;
        this._sortMem.checked = byMem;
        this._topBox.destroy_all_children();
        for (const p of byMem ? d.top_mem : d.top_cpu) {
            const row = new St.BoxLayout({style_class: 'tm-row tm-proc'});
            row.add_child(new St.Label({text: p.name, style_class: 'tm-proc-name', x_expand: true, y_align: Clutter.ActorAlign.CENTER}));
            row.add_child(new St.Label({
                text: byMem ? formatBytes(p.mem) : `${Math.round(p.cpu)}%`,
                style_class: 'tm-row-value', y_align: Clutter.ActorAlign.CENTER,
            }));
            const quit = new St.Button({
                style_class: 'tm-quit', can_focus: true, y_align: Clutter.ActorAlign.CENTER,
                child: new St.Icon({icon_name: 'window-close-symbolic', icon_size: 14}),
                accessible_name: `${_('Quit')} ${p.name}`,
            });
            // Hidden (not just disabled) for session-critical processes.
            quit.opacity = p.protected ? 0 : 255;
            quit.reactive = !p.protected;
            quit.connect('clicked', () => this._quit(p));
            row.add_child(quit);
            this._topBox.add_child(row);
        }
    }

    _renderAlerts() {
        const recent = this._data.alerts.recent ?? [];
        this._alertsHeader.visible = recent.length > 0;
        this._alertsBox.visible = recent.length > 0;
        this._alertsBox.destroy_all_children();
        const icons = {info: 'dialog-information-symbolic', warning: 'dialog-warning-symbolic', critical: 'dialog-error-symbolic'};
        for (const a of recent) {
            const row = new St.BoxLayout({style_class: `tm-row tm-alert tm-alert-${a.severity}`});
            row.add_child(new St.Icon({icon_name: icons[a.severity] ?? icons.info, icon_size: 14, y_align: Clutter.ActorAlign.CENTER}));
            row.add_child(new St.Label({text: a.title, style_class: 'tm-alert-title', x_expand: true, y_align: Clutter.ActorAlign.CENTER}));
            row.add_child(new St.Label({text: this._ago(a.timestamp), style_class: 'tm-caption', y_align: Clutter.ActorAlign.CENTER}));
            this._alertsBox.add_child(row);
        }
    }

    _ago(ts) {
        const s = Math.max(0, Date.now() / 1000 - ts);
        if (s < 60)
            return _('now');
        if (s < 3600)
            return `${Math.floor(s / 60)}m`;
        if (s < 86400)
            return `${Math.floor(s / 3600)}h`;
        return `${Math.floor(s / 86400)}d`;
    }

    _showStatus(text) {
        this._status.text = text;
        this._status.visible = !!text;
    }

    _quit(p) {
        callDaemon('SignalProcess', new GLib.Variant('(uts)', [p.pid, p.start, 'term']), '(bs)', (reply, err) => {
            if (err)
                this._showStatus(err.message);
            else if (!reply[0])
                this._showStatus(`${p.name}: ${reply[1]}`);
            else
                this._showStatus('');
        });
    }

    _setTopSort(sort) {
        if (this._data?.hud)
            this._data.hud.top_sort = sort;
        this._renderTop();
        callDaemon('SetSetting', new GLib.Variant('(ss)', ['hud.top_sort', JSON.stringify(sort)]), '(bs)', null);
    }

    _startService() {
        Gio.DBus.session.call(
            'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'StartServiceByName',
            new GLib.Variant('(su)', [BUS_NAME, 0]), null, Gio.DBusCallFlags.NONE, -1, null,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                    this._showStatus('');
                } catch (e) {
                    this._showStatus(_('Could not start topmanagerd. Is TopManager installed?'));
                }
            });
    }
});

export default class TopManagerHudExtension extends Extension {
    enable() {
        this._indicator = new Indicator(this);
        // The panel may destroy the indicator before disable() runs (shell
        // shutdown); drop the reference so late D-Bus callbacks become no-ops.
        this._indicator.connect('destroy', () => {
            this._indicator = null;
        });
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');

        this._tickId = Gio.DBus.session.signal_subscribe(
            BUS_NAME, INTERFACE, 'Tick', OBJECT_PATH, null, Gio.DBusSignalFlags.NONE,
            (_conn, _sender, _path, _iface, _signal, params) => {
                try {
                    this._indicator?.update(JSON.parse(params.deepUnpack()[0]));
                } catch (e) {
                    console.warn(`TopManager HUD: bad Tick payload: ${e}`);
                }
            });
        this._settingsId = Gio.DBus.session.signal_subscribe(
            BUS_NAME, INTERFACE, 'SettingsChanged', OBJECT_PATH, null, Gio.DBusSignalFlags.NONE,
            () => this._fetchSummary());

        // AUTO_START D-Bus-activates topmanagerd if it isn't running yet.
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.AUTO_START,
            () => {
                this._indicator?.setOnline(true);
                this._fetchSummary();
            },
            () => this._indicator?.setOnline(false));
    }

    _fetchSummary() {
        callDaemon('GetSummary', null, '(s)', reply => {
            if (reply)
                this._indicator?.update(JSON.parse(reply[0]));
        });
    }

    disable() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        for (const id of [this._tickId, this._settingsId]) {
            if (id)
                Gio.DBus.session.signal_unsubscribe(id);
        }
        this._tickId = this._settingsId = 0;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
