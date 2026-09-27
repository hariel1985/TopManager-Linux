// TopManager HUD preferences. Settings live in topmanagerd's config.toml, so
// this page reads and writes them over D-Bus instead of GSettings.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences, gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const BUS_NAME = 'io.github.hariel1985.TopManager';
const OBJECT_PATH = '/io/github/hariel1985/TopManager';
const INTERFACE = 'io.github.hariel1985.TopManager1';

function callSync(method, params, replyType) {
    return Gio.DBus.session.call_sync(
        BUS_NAME, OBJECT_PATH, INTERFACE, method, params,
        new GLib.VariantType(replyType), Gio.DBusCallFlags.NONE, 2000, null).deepUnpack();
}

function set(key, value, onError) {
    Gio.DBus.session.call(
        BUS_NAME, OBJECT_PATH, INTERFACE, 'SetSetting',
        new GLib.Variant('(ss)', [key, JSON.stringify(value)]),
        new GLib.VariantType('(bs)'), Gio.DBusCallFlags.NONE, -1, null,
        (conn, res) => {
            try {
                const [ok, err] = conn.call_finish(res).deepUnpack();
                if (!ok)
                    onError(err);
            } catch (e) {
                onError(e.message);
            }
        });
}

export default class TopManagerHudPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const page = new Adw.PreferencesPage();
        window.add(page);

        let s;
        try {
            s = JSON.parse(callSync('GetSettings', null, '(s)')[0]);
        } catch (e) {
            const group = new Adw.PreferencesGroup();
            group.add(new Adw.StatusPage({
                icon_name: 'dialog-warning-symbolic',
                title: _('TopManager service is not running'),
                description: _('Install TopManager, or start it with “systemctl --user start topmanagerd”.'),
            }));
            page.add(group);
            return;
        }

        const toast = msg => window.add_toast(new Adw.Toast({title: msg}));

        // ---- top bar ----
        const hud = new Adw.PreferencesGroup({title: _('Top bar')});
        page.add(hud);

        const metrics = [
            ['cpu', _('CPU %')],
            ['memory', _('Memory %')],
            ['cpu_memory', _('CPU and memory')],
            ['health', _('Health score')],
            ['download', _('Download speed')],
        ];
        const metricRow = new Adw.ComboRow({
            title: _('Show in the top bar'),
            model: Gtk.StringList.new(metrics.map(m => m[1])),
            selected: Math.max(0, metrics.findIndex(m => m[0] === s.hud.metric)),
        });
        metricRow.connect('notify::selected', () => set('hud.metric', metrics[metricRow.selected][0], toast));
        hud.add(metricRow);

        const spark = new Adw.SwitchRow({title: _('Sparkline next to the value'), active: s.hud.show_sparkline});
        spark.connect('notify::active', () => set('hud.show_sparkline', spark.active, toast));
        hud.add(spark);

        const topCount = Adw.SpinRow.new_with_range(1, 10, 1);
        topCount.title = _('Processes in the menu');
        topCount.value = s.hud.top_count;
        topCount.connect('notify::value', () => set('hud.top_count', Math.round(topCount.value), toast));
        hud.add(topCount);

        // ---- general ----
        const general = new Adw.PreferencesGroup({title: _('Monitoring')});
        page.add(general);

        const interval = Adw.SpinRow.new_with_range(1, 30, 1);
        interval.title = _('Refresh interval');
        interval.subtitle = _('Seconds between samples');
        interval.value = s.general.refresh_interval;
        interval.connect('notify::value', () => set('general.refresh_interval', interval.value, toast));
        general.add(interval);

        const notify = new Adw.SwitchRow({title: _('Desktop notifications for alerts'), active: s.general.notifications});
        notify.connect('notify::active', () => set('general.notifications', notify.active, toast));
        general.add(notify);

        // ---- alerts ----
        const alerts = new Adw.PreferencesGroup({title: _('Alert thresholds')});
        page.add(alerts);

        const cpu = Adw.SpinRow.new_with_range(10, 100, 5);
        cpu.title = _('High CPU (%)');
        cpu.value = s.alerts.cpu_percent;
        cpu.connect('notify::value', () => set('alerts.cpu_percent', cpu.value, toast));
        alerts.add(cpu);

        const disk = Adw.SpinRow.new_with_range(50, 100, 1);
        disk.title = _('Disk almost full (%)');
        disk.value = Math.round(s.alerts.disk_used_fraction * 100);
        disk.connect('notify::value', () => set('alerts.disk_used_fraction', disk.value / 100, toast));
        alerts.add(disk);

        const battery = Adw.SpinRow.new_with_range(0, 100, 5);
        battery.title = _('Low battery (%)');
        battery.value = s.alerts.low_battery_percent;
        battery.connect('notify::value', () => set('alerts.low_battery_percent', Math.round(battery.value), toast));
        alerts.add(battery);
    }
}
