// Formatting shared by the HUD and its preferences. Mirrors tm_core::format
// (binary units) so the HUD and the GUI print identical numbers.

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

export function formatBytes(bytes) {
    if (!Number.isFinite(bytes) || bytes <= 0)
        return '0 B';
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < UNITS.length - 1) {
        value /= 1024;
        unit++;
    }
    if (unit === 0)
        return `${Math.round(value)} B`;
    return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${UNITS[unit]}`;
}

export function formatRate(bytesPerSecond) {
    if (!Number.isFinite(bytesPerSecond) || Math.abs(bytesPerSecond) < 1)
        return '0 B/s';
    return `${formatBytes(Math.abs(bytesPerSecond))}/s`;
}

export function formatMinutes(minutes) {
    if (!minutes || minutes <= 0)
        return '—';
    const h = Math.floor(minutes / 60);
    const m = minutes % 60;
    return h > 0 ? `${h}h ${m}m` : `${m}m`;
}

export function healthClass(score) {
    if (score >= 85)
        return 'tm-health-excellent';
    if (score >= 70)
        return 'tm-health-good';
    if (score >= 50)
        return 'tm-health-fair';
    return 'tm-health-poor';
}
