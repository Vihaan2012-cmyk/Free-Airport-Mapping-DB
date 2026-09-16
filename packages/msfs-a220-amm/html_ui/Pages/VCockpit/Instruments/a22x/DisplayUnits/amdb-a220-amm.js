'use strict';
/* global SimVar */

// AMDB Airport Moving Map for the Synaptic A220, drawn from a local amdb-bridge.
//
// No Navigraph account, no hosts-file redirect and no certificate: the map fetches plain
// HTTP from the bridge on this machine, and the bridge builds whatever airport you are at
// on demand. Works the same under MSFS 2020 and 2024.
//
// The map mounts itself on the display that shows AIRPORT MAP FAULT, since the aircraft
// only ever prints that on the screen the airport map belongs to, and follows that
// display's own range selection. It draws below the aircraft's symbology, so the ND's
// own rings and readouts stay on top.
//
// Controls (bind these to keys, or leave them alone):
//   L:AMDB_AMM_VISIBLE   0 automatic (low or on the ground), 1 always on, 2 always off
//   L:AMDB_AMM_DISPLAY   which ND to use when the aircraft has more than one; 0 is auto

(function () {
    const BRIDGE = 'http://127.0.0.1:8770/v1';
    // The airport map's own scale, as the aircraft labels it: three ranges in feet and one
    // in miles. Anything wider is the navigation display's business, not this map's.
    const RANGE_NM = { '1000': 0.164579, '2000': 0.3291577, '3000': 0.4937365, '1': 1 };
    const RANGE_ORDER = ['1000', '2000', '3000', '1'];
    const DEFAULT_RANGE = '2000';
    // The aircraft's own canvas size for this display, and the element that identifies it.
    const PANE_W = 1480;
    const PANE_H = 1110;
    const FAULT_TEXT = 'AIRPORT MAP FAULT';
    const CANVAS_ID = 'AMDB_A220_AMM_CANVAS';
    // Shown from this height down, and on the ground: an airport map is of no use in the
    // cruise, and the real one comes alive on short final.
    const ENGAGE_FT = 100;
    const DEG = Math.PI / 180;
    const M_PER_DEG = 111320;
    const POLL_MS = 4000;          // how often to ask which airport we are at
    // Culling rejects about nineteen features in twenty before a point is read, so a frame
    // is cheap now and this no longer has to be nursed. It was 100 ms when every feature
    // was reprojected every time, which showed as a visibly lagging map.
    const DRAW_MS = 33;

    // The aircraft's own palette, taken from its display stylesheet, so the map reads as
    // part of the A220 rather than as an overlay: a grey ramp for pavement, white paint,
    // amber guidance, red holding positions, blue structures.
    // Matched to the aircraft's own airport map: pale pavement on black, so the field
    // reads like a printed chart, and structures in grey rather than a colour of their
    // own. The only saturated things on the map are the ones meant to be noticed.
    const FILL_LAYERS = [
        ['water', '#1a2632'],
        ['serviceroad', '#6e6e6e'],
        ['apronelement', '#9c9c9c'],
        ['deicingarea', '#9c9c9c'],
        ['parkingstandarea', '#a4a4a4'],
        ['taxiwayelement', '#b4b4b4'],
        ['runwayshoulder', '#8a8a8a'],
        ['runwaydisplacedarea', '#9c9c9c'],
        ['runwayelement', '#a8a8a8'],
        ['constructionarea', '#7a5a2a'],
        ['verticalpolygonalstructure', '#6e6e6e'],
        ['runwaymarking', '#ffffff'],
    ];
    // [layer, colour, width in px, dash pattern]
    const LINE_LAYERS = [
        ['paintedcenterline', '#ffffff', 2, [18, 18]],
        ['taxiwayintersectionmarking', '#d8c840', 2, null],
        ['taxiwayguidanceline', '#d8c840', 2, null],
        ['standguidanceline', '#d8c840', 2, null],
        ['runwayexitline', '#d8c840', 2, null],
        ['taxiwayholdingposition', '#ff3b30', 3, null],
        ['verticallinestructure', '#7a7a7a', 1, null],
    ];
    // The last number is the widest range index the label survives to, against RANGE_ORDER.
    const LABEL_LAYERS = [
        ['runwaythreshold', 'idthr', '#ffffff', '700 23px "A22X Mono", monospace', 3],
        ['taxiwayguidanceline', 'idlin', '#e8d84a', '400 20.25px "A22X Mono", monospace', 3],
        ['parkingstandlocation', 'idstd', '#e8d84a', '400 15px "A22X Mono", monospace', 1],
        // The bridge serves no building names, so a structure is labelled by its ident
        // where it has one, and silently skipped where it does not.
        ['verticalpolygonalstructure', 'ident', '#d8d8d8', '400 15px "A22X Mono", monospace', 1],
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
    // Where the map is mounted, and the aircraft's own range readout on that display.
    // Reported to the bridge on each lookup, because there is no console inside the sim
    // and its log is the only place this can be read.
    let paneInfo = 'init';
    let mountParent = null;
    let rangeText = null;
    // Geometry projected into metres once, when the airport arrives, instead of on every
    // frame. Each entry also carries its bounding box so anything off the screen can be
    // skipped without touching its points at all.
    let prepared = null;
    // Where the view is centred. Null follows the aircraft; dragging sets it to a real
    // position, which is what lets the airport lookup follow the pan to a different field
    // instead of snapping back to whatever is under the aeroplane.
    let viewCenter = null;
    let drag = null;
    let dragHooked = false;
    // The pixels-per-metre the last frame drew at, so a drag can convert the distance the
    // cursor moved into a distance on the ground.
    let lastScale = 0;
    // The aircraft's own "no airport map" message. Kept to hand because it is both the
    // anchor that says which display to mount on and something to hide once this map is
    // the one supplying the data.
    let faultEl = null;
    // The rest of the aircraft's symbology that is put away while the map is drawing.
    // Resolved once when the map mounts rather than searched for on every pass.
    let clutter = [];
    // Features that survived culling on the last frame, reported so the saving is a
    // measurement rather than an assumption.
    let lastDrawn = 0;
    // The half of the display unit the navigation display occupies. One SVG carries both
    // the navigation display and the engine page side by side, so the map has to keep to
    // its own half or it paints over the engine indications.
    let viewX = 0;
    let viewW = PANE_W / 2;

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

    // A building is labelled the way a chart labels it - "Terminal", "Concourse D" - not
    // with its full legal name. OpenStreetMap carries the long form, and centred on a
    // building's middle that sprawls well past the building and onto the taxiways either
    // side of it, which reads as the label being in the wrong place.
    function tidyStructure(s) {
        let t = String(s).trim();
        const cut = t.search(/\s+(do|da|de|del|of)\s+/i);
        if (cut > 0) t = t.slice(0, cut).trim();
        const paren = t.indexOf(' (');
        if (paren > 0) t = t.slice(0, paren).trim();
        const comma = t.indexOf(',');
        if (comma > 0) t = t.slice(0, comma).trim();
        return t.length > 14 ? t.slice(0, 14).trim() : t;
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

    // The aircraft itself says where the airport map belongs: it prints AIRPORT MAP FAULT
    // on that display and nowhere else. Finding that text and taking the SVG it lives in
    // is exact, where ranking display surfaces by size or visibility was guesswork that
    // picked a different screen every time the aircraft rebuilt its panels.
    function findFaultText(root) {
        const texts = root.querySelectorAll('text');
        for (let i = 0; i < texts.length; i++) {
            if ((texts[i].textContent || '').trim() === FAULT_TEXT) return texts[i];
        }
        return null;
    }

    // A flag on the display, found by what it says. Several of these are drawn on a black
    // backing rectangle inside a group of their own, and the group is what wants hiding:
    // hiding the text alone would leave the black box sitting on the map. Where the flag
    // is not in a group of its own, the text itself comes back instead.
    function findFlag(root, label) {
        const texts = root.querySelectorAll('text');
        for (let i = 0; i < texts.length; i++) {
            if ((texts[i].textContent || '').trim() !== label) continue;
            const g = texts[i].parentNode;
            // Only when the group holds nothing but this flag. If the aircraft ever
            // rearranges its display, that test fails and too little is hidden, rather
            // than a group with half the symbology in it being blanked.
            if (g && g.tagName === 'g' && (g.textContent || '').trim() === label) return g;
            return texts[i];
        }
        return null;
    }

    // Everything hidden while the map is drawing, worked out once on mounting. An entry
    // may name the message its element has to be showing for hiding it to be correct,
    // because the aircraft reuses one element for a whole family of messages.
    function findClutter(svg) {
        const out = [];
        function add(el, only) {
            if (el) out.push({ el: el, only: only || null });
        }

        // The aircraft's other map-failure annunciator. Like AIRPORT MAP FAULT, it says
        // the map is not being drawn, which stops being true the moment it is.
        add(findFlag(svg, 'FMS MAP'));
        // The flight management message line. One element carries every message it has,
        // so it is only hidden while it actually reads NO FLIGHT PLAN; a real message
        // such as DISCONTINUITY still gets through.
        add(findFlag(svg, 'NO FLIGHT PLAN'), 'NO FLIGHT PLAN');
        // The traffic legend. Its text is not in a group of its own, and the black panel
        // behind it is a bare rectangle drawn separately, so the two are taken together
        // or the panel is left behind with nothing in it.
        add(findFlag(svg, 'TCAS OFF'));
        add(svg.querySelector('rect[y="916"][width="168"][height="194"]'));
        // The compass ring: the arc, its heading labels and readout, the track diamond and
        // the range rings drawn with them. The ring is clipped through the one part of the
        // assembly carrying an id of its own, and two levels above that clip path is the
        // group holding all of it. The speed and altitude readouts are a separate
        // component and are left alone, being as true over this map as over any other.
        const mask = svg.querySelector('[id^="nd-ring-mask-"]');
        if (mask && mask.parentNode) add(mask.parentNode.parentNode);
        return out;
    }

    // An inline style rather than the visibility attribute, because the attribute is bound
    // to the aircraft's own state and gets rewritten; an inline style outranks that.
    // Clearing it hands control back to the aircraft instead of pinning a value of ours.
    function setHidden(el, hide) {
        if (!el || !el.style) return;
        const want = hide ? 'hidden' : '';
        if (el.style.visibility !== want) el.style.visibility = want;
    }

    // The display's own range readout, so the map zooms with the aircraft's range knob
    // instead of keeping a private idea of scale.
    function findRangeText(svg) {
        const texts = svg.querySelectorAll('text');
        for (let i = 0; i < texts.length; i++) {
            const v = (texts[i].textContent || '').trim();
            if (Object.prototype.hasOwnProperty.call(RANGE_NM, v)) return texts[i];
        }
        return null;
    }

    // Which ND to mount on. The aircraft numbers them, so try each in turn and take the
    // one that is actually showing the fault; L:AMDB_AMM_DISPLAY pins it to one.
    function findMount() {
        const forced = Math.round(lvar('L:AMDB_AMM_DISPLAY'));
        const order = forced > 0 ? [forced] : [1, 2, 3, 4];
        for (let i = 0; i < order.length; i++) {
            const mount = document.getElementById('ND_' + order[i] + '_MOUNT');
            if (!mount) continue;
            const fault = findFaultText(mount);
            if (fault && fault.ownerSVGElement && fault.ownerSVGElement.parentElement) {
                return { n: order[i], svg: fault.ownerSVGElement, fault: fault };
            }
        }
        return null;
    }

    // Resolve everything or nothing. Half a mount is what produced a canvas that was
    // attached but never drawn on, so the map either has a real display or waits.
    function ensureCanvas() {
        const found = findMount();
        if (!found) {
            paneInfo = 'nomount';
            canvas = null;
            ctx = null;
            return false;
        }
        const parent = found.svg.parentElement;
        // Keyed by element id rather than a variable: when the aircraft rebuilds a display
        // the old canvas goes with it, and the id is how that is noticed.
        let el = null;
        try { el = document.getElementById(CANVAS_ID); } catch (e) { el = null; }
        if (el && el.parentElement !== parent) {
            if (el.parentElement) el.parentElement.removeChild(el);
            el = null;
        }
        if (!el) {
            el = document.createElement('canvas');
            el.id = CANVAS_ID;
            el.width = PANE_W;
            el.height = PANE_H;
            const st = el.style;
            st.position = 'absolute';
            st.left = '0px';
            st.top = '0px';
            st.pointerEvents = 'none';
            st.background = 'transparent';
            // Underneath the aircraft's own symbology, so its rings, readouts and the
            // aeroplane symbol keep drawing over the map rather than being buried by it.
            parent.insertBefore(el, parent.firstChild || found.svg);
        }
        // Stretched to whatever the display is occupying at the moment. The ND is not
        // always full width - sharing the screen with the engine page it is far narrower -
        // so a fixed size hangs off the side of it. The drawing space stays 1480 x 1110
        // and is scaled onto the pane, which also keeps type and line weights in
        // proportion at any size. Re-read every pass, so a reconfigured screen is followed.
        const rect = found.svg.getBoundingClientRect ? found.svg.getBoundingClientRect() : null;
        const cssW = Math.round((rect && rect.width) || found.svg.clientWidth || PANE_W);
        const cssH = Math.round((rect && rect.height) || found.svg.clientHeight || PANE_H);
        if (cssW > 0 && cssH > 0) {
            el.style.width = cssW + 'px';
            el.style.height = cssH + 'px';
        }
        canvas = el;
        ctx = el.getContext('2d');
        faultEl = found.fault;
        clutter = findClutter(found.svg);
        hookDrag();
        mountParent = parent;
        rangeText = findRangeText(found.svg);

        // Which half of the display unit is the navigation display. The fault text is
        // centred in that half, so its own position answers it without assuming a side:
        // the crew can put the navigation display on either one.
        // The fault text is centred in the navigation half, so its own x says both which
        // half that is and how wide it is - the same relationship the aircraft's own map
        // relies on.
        let cx = PANE_W / 4;
        const attrX = Number(found.fault.getAttribute('x'));
        if (isFinite(attrX) && attrX > 0) {
            cx = attrX;
        } else {
            try {
                const b = found.fault.getBBox();
                if (b && isFinite(b.x) && isFinite(b.width)) cx = b.x + b.width / 2;
            } catch (e) { /* keep the default */ }
        }
        viewW = PANE_W / 2;
        viewX = cx < PANE_W / 2 ? 0 : PANE_W / 2;

        paneInfo = 'nd' + found.n + (rangeText ? 'r' : '-') + (viewX ? 'R' : 'L');
        return !!ctx;
    }

    // The label the display is showing, if it is one this map has a scale for. The
    // airport map covers only the three close ranges and a mile; anything wider belongs
    // to the navigation display, and null here is what keeps the map off at those.
    function currentRange() {
        const label = rangeText ? (rangeText.textContent || '').trim() : '';
        return Object.prototype.hasOwnProperty.call(RANGE_NM, label) ? label : null;
    }

    // Coherent GT does not reliably implement Response.json(), and an undefined coming
    // back from it looks exactly like "the bridge found no airport". Take the text and
    // parse it, which every engine supports.
    function get(url) {
        return fetch(url, { method: 'GET', headers: { Accept: 'application/json' } })
            .then((r) => {
                if (!r.ok) throw new Error('HTTP ' + r.status);
                return r.text();
            })
            .then((t) => JSON.parse(t));
    }

    function load(lat, lon) {
        if (busy) return;
        busy = true;
        // The local bridge and nothing else: /v1/nearest answers with the airports around
        // a position, nearest first.
        // The radius and limit are left at the bridge's defaults to keep the query short:
        // it truncates a long one in its log, and the log is where this is read. Fields
        // are pane, airport, data loaded, and the size the canvas actually occupies on
        // screen, which is what separates a drawing fault from a data one.
        get(BRIDGE + '/nearest?lat=' + lat.toFixed(4) + '&lon=' + lon.toFixed(4)
            + '&dbg=' + paneInfo
                + '.' + (icao || 'none')
                + '.' + (data ? 'd' : 'n')
                + '.' + currentRange()
                + '.' + (canvas ? canvas.clientWidth + 'x' + canvas.clientHeight : 'noc')
                + '.f' + lastDrawn
                + (viewCenter ? '.pan' : '') + (dragHooked ? '.hook' : ''))
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
                    prepared = prepareAll(fc);
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

    // Metres back to degrees, so a drag across the glass becomes a real position.
    function unproject(x, y) {
        return [
            ref.lon + x / (M_PER_DEG * Math.cos(ref.lat * DEG)),
            ref.lat + y / M_PER_DEG,
        ];
    }

    // The centre of the view right now, in metres on the airport's frame.
    function centreMetres(lat, lon) {
        return viewCenter ? project(viewCenter.lon, viewCenter.lat) : project(lon, lat);
    }

    // The aircraft's own cursor lives on this element; hooking it is what makes the map
    // respond to the cockpit's pointer rather than needing a keybind.
    // Is the cursor over the map itself, rather than the button row along the top or the
    // engine page on the other half of the display unit?
    function pointerOnMap(e) {
        if (!canvas || !canvas.getBoundingClientRect) return false;
        const rect = canvas.getBoundingClientRect();
        if (!rect || !(rect.width > 0) || !(rect.height > 0)) return false;
        const lx = (e.clientX - rect.left) * canvas.width / rect.width;
        const ly = (e.clientY - rect.top) * canvas.height / rect.height;
        // The top strip carries the display's own buttons; leave those to the aircraft.
        return lx >= viewX && lx <= viewX + viewW && ly >= 85 && ly <= canvas.height;
    }

    // Listeners go on the window, with capture, and cover mouse as well as pointer events.
    // Bound to the interaction element instead, the press arrives but the movement never
    // does: the cockpit cursor delivers the rest of the gesture to the document.
    function hookDrag() {
        if (dragHooked) return;
        if (!window.addEventListener) return;
        dragHooked = true;

        const down = (e) => {
            if (!ctx || !ref || !canvas) return;
            if (e.button !== undefined && e.button !== 0) return;
            if (!pointerOnMap(e)) return;
            drag = { x: e.clientX, y: e.clientY };
        };
        const move = (e) => {
            if (!drag || !ctx || !ref || !canvas) return;
            // Client pixels to canvas pixels to metres, then undo the heading rotation so
            // the ground follows the cursor rather than the compass.
            const rect = canvas.getBoundingClientRect();
            if (!rect || !(rect.width > 0)) return;
            const dsx = (e.clientX - drag.x) * canvas.width / rect.width;
            const dsy = (e.clientY - drag.y) * canvas.height / rect.height;
            if (!dsx && !dsy) return;
            const lat = simvar('PLANE LATITUDE', 'degree latitude');
            const lon = simvar('PLANE LONGITUDE', 'degree longitude');
            if (lat === null || lon === null) return;
            const hdg = simvar('PLANE HEADING DEGREES TRUE', 'degree') || 0;
            const cs = Math.cos(hdg * DEG);
            const sn = Math.sin(hdg * DEG);
            const s = lastScale || 1;
            const dx = (dsx * cs - dsy * sn) / s;
            const dy = (-dsx * sn - dsy * cs) / s;
            const c = centreMetres(lat, lon);
            const moved = unproject(c[0] - dx, c[1] - dy);
            viewCenter = { lon: moved[0], lat: moved[1] };
            drag = { x: e.clientX, y: e.clientY };
        };
        const release = () => {
            if (!drag) return;
            drag = null;
            // Look again at once: the view may now be over a different airport.
            nextPoll = 0;
        };

        window.addEventListener('mousedown', down, true);
        window.addEventListener('mousemove', move, true);
        window.addEventListener('mouseup', release, true);
        window.addEventListener('pointerdown', down, true);
        window.addEventListener('pointermove', move, true);
        window.addEventListener('pointerup', release, true);
        window.addEventListener('pointercancel', release, true);
        window.addEventListener('blur', release);
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

    // One pass over the airport when it loads: every ring becomes a flat array of metres
    // east and north, with the feature's extent alongside it. Drawing then costs a rotate
    // and a scale per point rather than a projection, and most features are rejected on
    // their bounding box before a single point is read.
    function prepareLayer(fc) {
        const out = [];
        if (!fc || !fc.features) return out;
        for (let i = 0; i < fc.features.length; i++) {
            const feat = fc.features[i];
            const parts = [];
            let minx = Infinity, miny = Infinity, maxx = -Infinity, maxy = -Infinity;
            eachPart(feat.geometry, (ring) => {
                const a = new Float64Array(ring.length * 2);
                for (let j = 0; j < ring.length; j++) {
                    const p = project(ring[j][0], ring[j][1]);
                    a[j * 2] = p[0];
                    a[j * 2 + 1] = p[1];
                    if (p[0] < minx) minx = p[0];
                    if (p[0] > maxx) maxx = p[0];
                    if (p[1] < miny) miny = p[1];
                    if (p[1] > maxy) maxy = p[1];
                }
                if (a.length >= 4) parts.push(a);
            });
            if (parts.length) out.push({ parts: parts, minx: minx, miny: miny, maxx: maxx, maxy: maxy });
        }
        return out;
    }

    function prepareAll(fc) {
        const out = {};
        for (let i = 0; i < FILL_LAYERS.length; i++) out[FILL_LAYERS[i][0]] = prepareLayer(fc[FILL_LAYERS[i][0]]);
        for (let i = 0; i < LINE_LAYERS.length; i++) out[LINE_LAYERS[i][0]] = prepareLayer(fc[LINE_LAYERS[i][0]]);
        return out;
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

        // Confined to the navigation display's half. This canvas spans the whole display
        // unit, and the engine indications live on the other half of it.
        const vx = viewX;
        const vw = viewW;
        ctx.save();
        ctx.beginPath();
        ctx.rect(vx, 0, vw, h);
        ctx.clip();
        ctx.fillStyle = '#050505';
        ctx.fillRect(vx, 0, vw, h);

        const lat = simvar('PLANE LATITUDE', 'degree latitude');
        const lon = simvar('PLANE LONGITUDE', 'degree longitude');
        const hdg = simvar('PLANE HEADING DEGREES TRUE', 'degree') || 0;

        if (!data || !ref || lat === null || lon === null) {
            ctx.fillStyle = '#848484';
            ctx.font = '400 20.25px "A22X Mono", monospace';
            ctx.textAlign = 'center';
            ctx.fillText(note || 'AIRPORT MAP', vx + vw / 2, h / 2);
            ctx.restore();
            return;
        }

        // Heading-up, ownship low on the pane so most of the view is ahead of the aircraft.
        const rangeLabel = currentRange() || DEFAULT_RANGE;
        const rangeIdx = RANGE_ORDER.indexOf(rangeLabel);
        const rangeM = RANGE_NM[rangeLabel] * 1852;
        const scale = (h * 0.72) / rangeM;
        lastScale = scale;
        const own = project(lon, lat);
        const cs = Math.cos(hdg * DEG);
        const sn = Math.sin(hdg * DEG);
        const ax = vx + vw / 2;
        const ay = h * 0.78;
        // The view follows the aircraft until it is dragged off it.
        const centre = centreMetres(lat, lon);
        const ox = centre[0];
        const oy = centre[1];
        const tf = (lo, la) => {
            const p = project(lo, la);
            const dx = p[0] - ox;
            const dy = p[1] - oy;
            return [ax + (dx * cs - dy * sn) * scale, ay - (dx * sn + dy * cs) * scale];
        };
        // Everything that can reach the pane, whatever way it is turned: the half diagonal
        // from the centre. Anything whose extent falls outside this is never touched.
        const reach = Math.sqrt(vw * vw + h * h) / scale;
        const cullMinX = ox - reach, cullMaxX = ox + reach;
        const cullMinY = oy - reach, cullMaxY = oy + reach;
        let drawn = 0;

        // One prepared feature: rotate and scale its stored metres straight into a path.
        const stroke = (feat) => {
            if (feat.maxx < cullMinX || feat.minx > cullMaxX || feat.maxy < cullMinY || feat.miny > cullMaxY) return;
            drawn++;
            for (let p = 0; p < feat.parts.length; p++) {
                const a = feat.parts[p];
                for (let j = 0; j < a.length; j += 2) {
                    const dx = a[j] - ox;
                    const dy = a[j + 1] - oy;
                    const sx = ax + (dx * cs - dy * sn) * scale;
                    const sy = ay - (dx * sn + dy * cs) * scale;
                    if (j === 0) ctx.moveTo(sx, sy);
                    else ctx.lineTo(sx, sy);
                }
            }
        };

        for (let i = 0; i < FILL_LAYERS.length; i++) {
            const feats = prepared ? prepared[FILL_LAYERS[i][0]] : null;
            if (!feats || !feats.length) continue;
            ctx.fillStyle = FILL_LAYERS[i][1];
            ctx.beginPath();
            for (let f = 0; f < feats.length; f++) stroke(feats[f]);
            ctx.fill('evenodd');
        }

        for (let i = 0; i < LINE_LAYERS.length; i++) {
            const spec = LINE_LAYERS[i];
            const feats = prepared ? prepared[spec[0]] : null;
            if (!feats || !feats.length) continue;
            ctx.strokeStyle = spec[1];
            ctx.lineWidth = spec[2];
            ctx.setLineDash(spec[3] || []);
            ctx.beginPath();
            for (let f = 0; f < feats.length; f++) stroke(feats[f]);
            ctx.stroke();
        }
        ctx.setLineDash([]);

        // Hotspots exist to be noticed, and the aircraft rings them in red rather than
        // tracing their outline: a circle round the middle, big enough to cover the area,
        // with its identifier beside it.
        const hs = data.hotspot;
        if (hs && hs.features && hs.features.length) {
            ctx.strokeStyle = '#c0342a';
            ctx.lineWidth = 2.5;
            for (let f = 0; f < hs.features.length; f++) {
                const feat = hs.features[f];
                const c = feat.properties.centroid;
                if (!c) continue;
                const mid = tf(c.coordinates[0], c.coordinates[1]);
                let radius = 0;
                eachPart(feat.geometry, (ring) => {
                    for (let i = 0; i < ring.length; i++) {
                        const p = tf(ring[i][0], ring[i][1]);
                        const d = Math.sqrt((p[0] - mid[0]) * (p[0] - mid[0]) + (p[1] - mid[1]) * (p[1] - mid[1]));
                        if (d > radius) radius = d;
                    }
                });
                if (radius < 6) radius = 6;
                ctx.beginPath();
                ctx.arc(mid[0], mid[1], radius, 0, Math.PI * 2);
                ctx.stroke();
                const id = feat.properties.idhot;
                if (id) {
                    ctx.font = '700 18px "A22X Mono", monospace';
                    ctx.textAlign = 'left';
                    ctx.textBaseline = 'middle';
                    ctx.fillStyle = '#ffffff';
                    ctx.fillText(String(id), mid[0] + radius + 5, mid[1]);
                }
            }
        }

        drawLabels(tf, vx, vw, h, rangeIdx);
        // At the centre while the view follows it; at its real place once dragged away.
        const oshp = viewCenter ? tf(lon, lat) : [ax, ay];
        drawOwnship(oshp[0], oshp[1]);
        drawHeader(vx, vw);
        ctx.restore();
        lastDrawn = drawn;
    }

    // Every label is drawn level with the screen, runway designators included. Turning a
    // designator to its runway's bearing sounds right - that is how the paint on the
    // ground lies - but the map itself already rotates with the aeroplane, so the text
    // then only reads straight on the one heading where the two rotations cancel, and
    // lies on its side everywhere else. Keeping labels upright also means the width
    // measured for the overlap test is the width actually occupied.
    function drawLabels(tf, vx, vw, h, rangeIdx) {
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
                else if (spec[0] === 'verticalpolygonalstructure') text = tidyStructure(text);
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
                if (p[0] < vx + 4 || p[1] < 4 || p[0] > vx + vw - 4 || p[1] > h - 4) continue;
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
                ctx.save();
                ctx.translate(p[0], p[1]);
                if (isRunway) {
                    // A rounded blue capsule, as the aircraft draws it, so the designator
                    // stays legible over the pale pavement it sits on.
                    const bw = half + 7;
                    const bh = 16;
                    const r = bh;
                    ctx.beginPath();
                    ctx.moveTo(-bw + r, -bh);
                    ctx.lineTo(bw - r, -bh);
                    ctx.arc(bw - r, 0, r, -Math.PI / 2, Math.PI / 2);
                    ctx.lineTo(-bw + r, bh);
                    ctx.arc(-bw + r, 0, r, Math.PI / 2, -Math.PI / 2);
                    ctx.closePath();
                    ctx.fillStyle = '#0b1420';
                    ctx.fill();
                    ctx.strokeStyle = '#4a86d8';
                    ctx.lineWidth = 2.5;
                    ctx.stroke();
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

    // Placed within the navigation display's half, not the whole display unit, or the
    // airport name would print over the engine indications on the other side.
    function drawHeader(vx, vw) {
        ctx.font = '400 13.5px "A22X Mono", monospace';
        ctx.textAlign = 'left';
        ctx.textBaseline = 'top';
        ctx.fillStyle = '#ffffff';
        // Labelled the way the aircraft labels it: feet for the three close ranges, miles
        // for the widest. Converting to miles would print 0.33 NM where the display says
        // 2000 FT, which is the same distance under a different name.
        const label = currentRange() || DEFAULT_RANGE;
        ctx.fillText(label === '1' ? '1 NM' : label + ' FT', vx + 8, 6);
        if (icao) {
            ctx.textAlign = 'right';
            ctx.fillStyle = '#848484';
            ctx.fillText(icao, vx + vw - 8, 6);
        }
        if (note) {
            ctx.textAlign = 'center';
            ctx.fillStyle = '#ffe100';
            ctx.fillText(note, vx + vw / 2, 6);
        }
    }

    // Automatic means low or stopped: an airport map earns its place from short final
    // onwards and all the way to the gate, and is only clutter at altitude.
    function visible() {
        const mode = Math.round(lvar('L:AMDB_AMM_VISIBLE'));
        if (mode === 1) return true;
        if (mode === 2) return false;
        // Only at the ranges the airport map has a scale for. Selecting 20 NM means the
        // crew is looking at the navigation picture, not the taxiways.
        if (!currentRange()) return false;
        if (simvar('SIM ON GROUND', 'bool') === 1) return true;
        const agl = simvar('PLANE ALT ABOVE GROUND MINUS CG', 'feet');
        if (agl !== null) return agl <= ENGAGE_FT;
        const radio = simvar('RADIO HEIGHT', 'feet');
        return radio !== null && radio <= ENGAGE_FT;
    }

    // Called from the instrument's Update(). Everything is guarded: a fault in the map
    // must never take the aircraft's displays down with it.
    function tick() {
        try {
            if (!ensureCanvas()) return;
            const show = visible();
            canvas.classList.toggle('amdb-amm-hidden', !show);
            // While the map is drawing, the aircraft's own annunciators and compass ring
            // are put away. Hidden and not removed: the fault text stays in the document
            // as the mount anchor, and every one of them comes back the moment the map is
            // not being shown. Re-applied every pass because the aircraft rebuilds its
            // displays and restores them when it does.
            const drawing = show && !!data;
            setHidden(faultEl, drawing);
            for (let i = 0; i < clutter.length; i++) {
                const c = clutter[i];
                const says = c.only === null || (c.el.textContent || '').trim() === c.only;
                setHidden(c.el, drawing && says);
            }
            if (!show) return;

            // Snaps the view back onto the aircraft after it has been dragged away.
            if (Math.round(lvar('L:AMDB_AMM_PAN_RESET')) === 1) viewCenter = null;

            const now = Date.now();
            if (now >= nextPoll) {
                nextPoll = now + POLL_MS;
                const lat = simvar('PLANE LATITUDE', 'degree latitude');
                const lon = simvar('PLANE LONGITUDE', 'degree longitude');
                // Look where the view is pointed, not where the aeroplane is: dragging
                // across to another field is what loads that field.
                if (viewCenter) load(viewCenter.lat, viewCenter.lon);
                else if (lat !== null && lon !== null) load(lat, lon);
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
