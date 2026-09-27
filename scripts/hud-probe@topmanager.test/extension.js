// Test-only probe for scripts/test-hud.sh. Never installed for real users.
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const OUT = GLib.getenv('TM_PROBE_OUT');

function write(name, text) {
    GLib.file_set_contents(`${OUT}/${name}`, text);
}

function shoot(name) {
    return new Promise(resolve => {
        const file = Gio.File.new_for_path(`${OUT}/${name}`);
        const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
        new Shell.Screenshot().screenshot(false, stream, (obj, res) => {
            try {
                obj.screenshot_finish(res);
            } catch (e) {
                write('error.txt', `screenshot: ${e}`);
            }
            stream.close(null);
            resolve();
        });
    });
}

const wait = ms => new Promise(r => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
    r();
    return GLib.SOURCE_REMOVE;
}));

export default class Probe extends Extension {
    enable() {
        this._run().catch(e => write('error.txt', `${e}\n${e.stack}`));
    }

    async _run() {
        const delay = Number(GLib.getenv('TM_PROBE_DELAY') || '8000');
        await wait(delay);
        Main.overview.hide();
        await wait(1000);
        const hud = Main.panel.statusArea['topmanager@hariel1985.github.io'];
        if (!hud) {
            write('error.txt', 'HUD indicator not found in the panel');
            write('done', '1');
            return;
        }
        await shoot('panel.png');
        hud.menu.open(false);
        await wait(1500);
        await shoot('menu.png');
        write('state.json', JSON.stringify({
            label: hud._panelLabel.text,
            online: hud._online,
            score: hud._score.text,
            rating: hud._rating.text,
            topRows: hud._topBox.get_n_children(),
            seq: hud._data?.seq ?? null,
        }, null, 2));
        write('done', '1');
    }

    disable() {}
}
