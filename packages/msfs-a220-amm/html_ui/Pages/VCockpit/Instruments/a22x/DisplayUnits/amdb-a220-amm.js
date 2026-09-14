'use strict';
/* global SimVar */

// AMDB Airport Moving Map for the Synaptic A220, drawn from a local amdb-bridge.
//
// No Navigraph account, no hosts-file redirect and no certificate: the map fetches plain
// HTTP from the bridge on this machine, and the bridge builds whatever airport you are at
// on demand. Works the same under MSFS 2020 and 2024.
//
// Controls (bind these to keys in the sim, or leave them alone and the map shows itself
// whenever the aircraft is on the ground):
//   L:AMDB_AMM_VISIBLE   0 automatic, 1 always on, 2 always off
//   L:AMDB_AMM_RANGE     0-4, selects from RANGES_NM below

(function () {
    const BRIDGE = 'http://127.0.0.1:8770/v1';
    const RANGES_NM = [0.25, 0.5, 1, 2, 4];
    const DEG = Math.PI / 180;
    const M_PER_DEG = 111320;
    const POLL_MS = 4000;          // how often to ask which airport we are at
    const DRAW_MS = 100;           // redraw rate; the map moves slowly on the ground

    // The aircraft's own palette, taken from its display stylesheet, so the map reads as
    // part of the A220 rather than as an overlay: a grey ramp for pavement, white paint,
    // amber guidance, red holding positions, blue structures.
    const FILL_LAYERS = [
        ['water', '#0a1a2a'],
        ['serviceroad', '#444444'],
        ['apronelement', '#555555'],
        ['deicingarea', '#555555'],
        ['parkingstandarea', '#5e5e5e'],
        ['taxiwayelement', '#686868'],
        ['runwayshoulder', '#777777'],
        ['runwaydisplacedarea', '#8f8f8f'],
        ['runwayelement', '#9a9a9a'],
        ['constructionarea', '#631c76'],
        ['verticalpolygonalstructure', '#003a7b'],
        ['runwaymarking', '#ffffff'],
    ];
    // [layer, colour, width in px, dash pattern]
    const LINE_LAYERS = [
        ['paintedcenterline', '#ffffff', 2, [18, 18]],
        ['taxiwayintersectionmarking', '#ffe100', 2, null],
        ['taxiwayguidanceline', '#ffe100', 2, null],
        ['standguidanceline', '#ffe100', 2, null],
        ['runwayexitline', '#ffe100', 2, null],
        ['taxiwayholdingposition', '#ff2c20', 3, null],
        ['verticallinestructure', '#848484', 1, null],
    ];
    const LABEL_LAYERS = [
        ['runwaythreshold', 'idthr', '#ffffff', '700 23px "A22X Mono", monospace', 5],
        ['taxiwayguidanceline', 'idlin', '#ffe100', '400 20.25px "A22X Mono", monospace', 3],
        ['parkingstandlocation', 'idstd', '#d6d6d6', '400 13.5px "A22X Mono", monospace', 1],
    ];
    const WANTED = FILL_LAYERS.map((l) => l[0])
        .concat(LINE_LAYERS.map((l) => l[0]))
        .concat(LABEL_LAYERS.map((l) => l[0]))
        .concat(['hotspot', 'aerodromereferencepoint'])
        .filter((v, i, a) => a.indexOf(v) === i);

    let canvas = null;
    let ctx = null;
    let data = null;            // the airport currently drawn
    let ref = null;             // its reference point, the origin of the metre frame
    let icao = null;
    let busy = false;
    let nextPoll = 0;
    let nextDraw = 0;
    let note = '';

    // A stand is called by its designator on the radio and printed that way on the map.
    // OpenStreetMap often spells it out in full ("Terminal 2 Gate E15B"), which buries
    // everything around it.
    function tidyStand(s) {
        let t = String(s).trim();
        const paren = t.lastIndexOf(' (');
        if (paren > 0) t = t.slice(0, paren).trim();
        const gate = t.lastIndexOf(' Gate ');
        if (gate >= 0 && t.slice(gate + 6).trim()) t = t.slice(gate + 6).trim();
        return t.length > 10 ? t.slice(0, 10).trim() : t;
    }

    function lvar(name) {
        try {
            return SimVar.GetSimVarValue(name, 'number') || 0;
        } catch (e) {
            return 0;
        }
    }

    function simvar(name, unit) {
        try {
            const v = SimVar.GetSimVarValue(name, unit);
            return typeof v === 'number' && isFinite(v) ? v : null;
        } catch (e) {
            return null;
        }
    }

    // The A220 draws its displays as SVG panes. Parenting the canvas to the map pane means
    // it inherits the aircraft's own placement and size, instead of this package guessing
    // at display-unit geometry and risking drawing over the PFD.
    function findPane() {
        const svgs = document.querySelectorAll('svg');
        let best = null;
        let bestArea = 0;
        for (let i = 0; i < svgs.length; i++) {
            const box = (svgs[i].getAttribute('viewBox') || '').trim().split(/[\s,]+/).map(Number);
            if (box.length !== 4 || !isFinite(box[2]) || !isFinite(box[3])) continue;
            // A map pane is wide and not very tall; the taller panes are the PFD and EICAS.
            if (box[3] > 800 || box[2] < 300) continue;
            const area = box[2] * box[3];
            if (area > bestArea) {
                bestArea = area;
                best = { svg: svgs[i], w: box[2], h: box[3] };
            }
        }
        return best;
    }

    function ensureCanvas() {
        if (canvas && canvas.isConnected) return true;
        const pane = findPane();
        if (!pane || !pane.svg.parentElement) return false;
        canvas = document.createElement('canvas');
        canvas.className = 'amdb-amm-canvas';
        canvas.width = Math.round(pane.w);
        canvas.height = Math.round(pane.h);
        const host = pane.svg.parentElement;
        if (getComputedStyle(host).position === 'static') host.style.position = 'relative';
        host.appendChild(canvas);
        ctx = canvas.getContext('2d');
        return true;
    }

    function get(url) {
        return fetch(url, { method: 'GET', headers: { Accept: 'application/json' } })
            .then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))));
    }

    function load(lat, lon) {
        if (busy) return;
        busy = true;
        // The local bridge and nothing else: /v1/nearest answers with the airports around
        // a position, nearest first.
        get(BRIDGE + '/nearest?lat=' + lat.toFixed(6) + '&lon=' + lon.toFixed(6) + '&radius_km=60&limit=1')
            .then((rows) => {
                const found = Array.isArray(rows) && rows.length ? rows[0].idarpt : null;
                if (!found) {
                    note = 'NO AIRPORT NEAR';
                    busy = false;
                    return null;
                }
                if (found === icao) {
                    busy = false;
                    return null;
                }
                note = 'LOADING ' + found;
                return get(BRIDGE + '/' + found + '?include=' + WANTED.join(',')).then((fc) => {
                    const arp = fc.aerodromereferencepoint;
                    let origin = null;
                    if (arp && arp.features && arp.features.length) {
                        origin = arp.features[0].geometry.coordinates;
                    }
                    if (!origin) origin = [lon, lat];
                    ref = { lon: origin[0], lat: origin[1] };
                    data = fc;
                    icao = found;
                    note = '';
                    busy = false;
                });
            })
            .catch((e) => {
                note = 'BRIDGE OFFLINE';
                busy = false;
            });
    }

    function project(lon, lat) {
        return [
            (lon - ref.lon) * M_PER_DEG * Math.cos(ref.lat * DEG),
            (lat - ref.lat) * M_PER_DEG,
        ];
    }

    // Walks a GeoJSON geometry, handing each ring or line to `emit` as a flat array.
    function eachPart(geom, emit) {
        if (!geom) return;
        const t = geom.type;
        const c = geom.coordinates;
        if (t === 'Polygon' || t === 'MultiLineString') {
            for (let i = 0; i < c.length; i++) emit(c[i]);
        } else if (t === 'MultiPolygon') {
            for (let i = 0; i < c.length; i++) for (let j = 0; j < c[i].length; j++) emit(c[i][j]);
        } else if (t === 'LineString') {
            emit(c);
        }
    }

    function path(tf, geom) {
        eachPart(geom, (ring) => {
            for (let i = 0; i < ring.length; i++) {
                const p = tf(ring[i][0], ring[i][1]);
                if (i === 0) ctx.moveTo(p[0], p[1]);
                else ctx.lineTo(p[0], p[1]);
            }
        });
    }

    function draw() {
        if (!ctx) return;
        const w = canvas.width;
        const h = canvas.height;
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.clearRect(0, 0, w, h);
        ctx.fillStyle = '#050505';
        ctx.fillRect(0, 0, w, h);

        const lat = simvar('PLANE LATITUDE', 'degree latitude');
        const lon = simvar('PLANE LONGITUDE', 'degree longitude');
        const hdg = simvar('PLANE HEADING DEGREES TRUE', 'degree') || 0;

        if (!data || !ref || lat === null || lon === null) {
            ctx.fillStyle = '#848484';
            ctx.font = '400 20.25px "A22X Mono", monospace';
            ctx.textAlign = 'center';
            ctx.fillText(note || 'AIRPORT MAP', w / 2, h / 2);
            return;
        }

        // Heading-up, ownship low on the pane so most of the view is ahead of the aircraft.
        const rangeIdx = Math.min(RANGES_NM.length - 1, Math.max(0, Math.round(lvar('L:AMDB_AMM_RANGE'))));
        const rangeM = RANGES_NM[rangeIdx] * 1852;
        const scale = (h * 0.72) / rangeM;
        const own = project(lon, lat);
        const cs = Math.cos(hdg * DEG);
        const sn = Math.sin(hdg * DEG);
        const ax = w / 2;
        const ay = h * 0.78;
        const tf = (lo, la) => {
            const p = project(lo, la);
            const dx = p[0] - own[0];
            const dy = p[1] - own[1];
            return [ax + (dx * cs - dy * sn) * scale, ay - (dx * sn + dy * cs) * scale];
        };

        for (let i = 0; i < FILL_LAYERS.length; i++) {
            const fc = data[FILL_LAYERS[i][0]];
            if (!fc || !fc.features || !fc.features.length) continue;
            ctx.fillStyle = FILL_LAYERS[i][1];
            ctx.beginPath();
            for (let f = 0; f < fc.features.length; f++) path(tf, fc.features[f].geometry);
            ctx.fill('evenodd');
        }

        for (let i = 0; i < LINE_LAYERS.length; i++) {
            const spec = LINE_LAYERS[i];
            const fc = data[spec[0]];
            if (!fc || !fc.features || !fc.features.length) continue;
            ctx.strokeStyle = spec[1];
            ctx.lineWidth = spec[2];
            ctx.setLineDash(spec[3] || []);
            ctx.beginPath();
            for (let f = 0; f < fc.features.length; f++) path(tf, fc.features[f].geometry);
            ctx.stroke();
        }
        ctx.setLineDash([]);

        // Hotspots: the one layer that exists to be noticed, so it is outlined, not filled.
        const hs = data.hotspot;
        if (hs && hs.features && hs.features.length) {
            ctx.strokeStyle = '#e029eb';
            ctx.lineWidth = 2;
            ctx.beginPath();
            for (let f = 0; f < hs.features.length; f++) path(tf, hs.features[f].geometry);
            ctx.stroke();
        }

        drawLabels(tf, w, h, rangeIdx, hdg);
        drawOwnship(ax, ay);
        drawHeader(w, h, rangeM);
    }

    // A runway designator is painted along the runway, so it is drawn along it here too:
    // the bearing turned into screen angle, then kept upright so it never reads upside
    // down. Everything else is drawn level.
    function labelAngle(brngTrue, hdg) {
        let a = brngTrue - hdg;
        a = ((a % 360) + 360) % 360;
        if (a > 180) a -= 360;
        if (a > 90) a -= 180;
        else if (a < -90) a += 180;
        return a;
    }

    function drawLabels(tf, w, h, rangeIdx, hdg) {
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        const placed = [];
        for (let i = 0; i < LABEL_LAYERS.length; i++) {
            const spec = LABEL_LAYERS[i];
            // Each kind earns its place only down to a certain range: stand numbers are
            // useful on the apron and nothing but clutter from two miles out.
            if (rangeIdx > spec[4]) continue;
            const fc = data[spec[0]];
            if (!fc || !fc.features) continue;
            ctx.font = spec[3];
            const seen = {};
            for (let f = 0; f < fc.features.length; f++) {
                const feat = fc.features[f];
                let text = feat.properties && feat.properties[spec[1]];
                if (!text) continue;
                if (spec[0] === 'parkingstandlocation') text = tidyStand(text);
                if (!text) continue;
                const g = feat.geometry;
                const props = feat.properties;
                let c = null;
                // The bridge precomputes a midpoint for lines and a centroid for areas.
                // Both are guaranteed to sit on the feature, which the middle coordinate
                // of a polyline is not when the line bends or comes in several parts.
                if (g.type === 'Point') c = g.coordinates;
                else if (props.midpoint) c = props.midpoint.coordinates || props.midpoint;
                else if (props.centroid) c = props.centroid.coordinates || props.centroid;
                else if (g.type === 'LineString') c = g.coordinates[Math.floor(g.coordinates.length / 2)];
                if (!c) continue;
                // One label per designator: a taxiway is one taxiway however many segments
                // it was drawn as.
                const key = spec[0] + ':' + text;
                if (seen[key]) continue;
                const p = tf(c[0], c[1]);
                if (p[0] < 4 || p[1] < 4 || p[0] > w - 4 || p[1] > h - 4) continue;
                const half = ctx.measureText(String(text)).width / 2 + 3;
                let clash = false;
                for (let q = 0; q < placed.length; q++) {
                    const r = placed[q];
                    if (Math.abs(p[0] - r[0]) < half + r[2] && Math.abs(p[1] - r[1]) < 14) {
                        clash = true;
                        break;
                    }
                }
                if (clash) continue;
                seen[key] = true;
                placed.push([p[0], p[1], half]);
                const isRunway = spec[0] === 'runwaythreshold';
                const turn = isRunway && typeof props.brngtrue === 'number' ? labelAngle(props.brngtrue, hdg) : 0;
                ctx.save();
                ctx.translate(p[0], p[1]);
                if (turn) ctx.rotate(turn * DEG);
                if (isRunway) {
                    // The runway designator is boxed on the display: black panel, cyan
                    // outline, so it carries over any pavement it lands on.
                    const bw = half + 5;
                    const bh = 15;
                    ctx.fillStyle = '#000000';
                    ctx.fillRect(-bw, -bh, bw * 2, bh * 2);
                    ctx.strokeStyle = '#00bfe6';
                    ctx.lineWidth = 2;
                    ctx.strokeRect(-bw, -bh, bw * 2, bh * 2);
                    ctx.fillStyle = spec[2];
                    ctx.fillText(String(text), 0, 0);
                } else {
                    ctx.fillStyle = '#000000';
                    ctx.fillText(String(text), 1, 1);
                    ctx.fillStyle = spec[2];
                    ctx.fillText(String(text), 0, 0);
                }
                ctx.restore();
            }
        }
    }

    // The Collins moving map marks the aeroplane with a white chevron.
    function drawOwnship(ax, ay) {
        ctx.strokeStyle = '#ffffff';
        ctx.lineWidth = 3;
        ctx.setLineDash([]);
        ctx.beginPath();
        ctx.moveTo(ax - 11, ay + 9);
        ctx.lineTo(ax, ay - 11);
        ctx.lineTo(ax + 11, ay + 9);
        ctx.stroke();
    }

    function drawHeader(w, h, rangeM) {
        ctx.font = '400 13.5px "A22X Mono", monospace';
        ctx.textAlign = 'left';
        ctx.textBaseline = 'top';
        ctx.fillStyle = '#ffffff';
        ctx.fillText((rangeM / 1852).toFixed(2).replace(/0$/, '') + ' NM', 8, 6);
        if (icao) {
            ctx.textAlign = 'right';
            ctx.fillStyle = '#848484';
            ctx.fillText(icao, w - 8, 6);
        }
        if (note) {
            ctx.textAlign = 'center';
            ctx.fillStyle = '#ffe100';
            ctx.fillText(note, w / 2, 6);
        }
    }

    function visible() {
        const mode = Math.round(lvar('L:AMDB_AMM_VISIBLE'));
        if (mode === 1) return true;
        if (mode === 2) return false;
        return simvar('SIM ON GROUND', 'bool') === 1;   // automatic: shown while taxiing
    }

    // Called from the instrument's Update(). Everything is guarded: a fault in the map
    // must never take the aircraft's displays down with it.
    function tick() {
        try {
            if (!ensureCanvas()) return;
            const show = visible();
            canvas.classList.toggle('amdb-amm-hidden', !show);
            if (!show) return;

            const now = Date.now();
            if (now >= nextPoll) {
                nextPoll = now + POLL_MS;
                const lat = simvar('PLANE LATITUDE', 'degree latitude');
                const lon = simvar('PLANE LONGITUDE', 'degree longitude');
                if (lat !== null && lon !== null) load(lat, lon);
            }
            if (now >= nextDraw) {
                nextDraw = now + DRAW_MS;
                draw();
            }
        } catch (e) {
            // Swallowed deliberately; see above.
        }
    }

    window.AMDB_AMM = { tick: tick };
})();
