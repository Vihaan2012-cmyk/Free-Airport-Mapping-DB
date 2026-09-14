-- AMDB OANS - an A380-style airport moving map for X-Plane 12 (FlyWithLua NG+)
--
-- Draws the airport you are at from amdb-bridge: pavement, runways, terminals,
-- taxiway guidance lines, holding positions, and taxiway, runway, stand and terminal
-- labels, with your aircraft on it. Heading-up ARC view like the A380 OANS, or
-- north-up PLAN. Nothing is stored locally; the running bridge builds the nearest
-- airport on demand and sends it here.
--
-- Start the bridge with:  amdb-bridge serve --xplane
-- Then in the sim: Plugins > FlyWithLua > FlyWithLua Macros > "AMDB OANS", or bind a
-- key to the command "amdb/oans/toggle".

local BRIDGE = "http://127.0.0.1:8770/" --@BRIDGE@

local RANGES_NM = { 0.25, 0.5, 1, 2, 4 }
local BAR_H = 34                  -- height of the control bar, px
local HEADER_H = 22               -- readout strip across the top of the map
local FONT_H = 13                 -- fallback glyph height if ImGui cannot measure
local POLL_S = 4                  -- seconds between "which airport am I near" checks (a tiny reply)

-- Airbus OANS depiction: black ground, grey pavement (aprons darker than taxiways),
-- runways grey with a white edge, a brown shoulder strip around the outside of all
-- pavement, yellow taxiway and stand guidance lines, red-orange holding positions,
-- terminals cyan and other buildings blue. ImGui packs colours as 0xAABBGGRR.
local function rgb(r, g, b) return 0xFF000000 + b * 0x10000 + g * 0x100 + r end
local function rgba(r, g, b, a) return a * 0x1000000 + b * 0x10000 + g * 0x100 + r end
local C = {
    bg = rgb(0, 0, 0), hdr = rgba(0, 0, 0, 205), tick = rgb(210, 210, 210), amber = rgb(255, 180, 0),
    apron = rgb(0x54, 0x54, 0x54), taxiway = rgb(0x8f, 0x8f, 0x8f),
    runway = rgb(0x80, 0x80, 0x80), rwyext = rgb(0x80, 0x80, 0x80), runway_far = rgb(255, 255, 255),
    -- Terminals and buildings read as cyan and blue, but a step down in brightness: at a
    -- big airport these cover much of the screen, and at full saturation they glare and
    -- flatten everything drawn over them. The labels keep the bright cyan.
    building = rgb(0x28, 0x5f, 0x9c), terminal = rgb(0, 0xa6, 0xa6),
    shoulder = rgb(0x85, 0x45, 0x1d), rwyedge = rgb(255, 255, 255),
    guide = rgb(255, 255, 0), guidefar = rgb(0x66, 0x66, 0x66), exit = rgb(255, 255, 0),
    stand = rgb(255, 255, 0), hold = rgb(255, 0x2f, 0), rwycl = rgb(255, 255, 255),
    twy_txt = rgb(255, 255, 0), rwy_txt = rgb(255, 255, 255), rwy_box = rgb(0, 0, 0),
    std_txt = rgb(200, 200, 200), term_txt = rgb(0, 255, 255), shadow = rgb(0, 0, 0),
    own = rgb(255, 255, 0), ring = rgb(255, 255, 255), dim = rgb(150, 150, 150),
}
local FILLS = { "apron", "taxiway", "rwyext", "runway", "building", "terminal" }
-- Drawn in this order; widths in pixels. Stand lines only at the two closest ranges.
local LINES = { { "rwyedge", 1.2 }, { "stand", 1.6 }, { "guidefar", 2.2 }, { "guide", 1.85 }, { "exit", 1.85 }, { "hold", 3.0 }, { "rwycl", 1.5 } }
local SHOULDER_W = 6.0            -- drawn before the fills, which then cover its inner half
local FAR_RANGE = 4               -- from this range index up: runways white, guidance lines grey (declutter)
local TWY, RWY, STAND, TERM = 1, 2, 3, 4
local LABEL_MAX_RANGE = { [TWY] = 3, [RWY] = 5, [STAND] = 1, [TERM] = 3 }

-- ImGui packs one window's geometry into 16-bit indices, so a draw list that goes past
-- 65,535 vertices wraps around and scatters triangles across the window. Stay well under
-- it: cull to what is actually on screen, and drop detail in steps when a frame runs big.
-- Level 1 is everything; 2 drops shoulders and plain buildings; 3 is the Airbus wide-view
-- depiction, white runways and grey guidance lines only.
local MAX_VERTS = 56000
local SKIP_FILL = {
    [2] = { building = true },
    [3] = { apron = true, taxiway = true, building = true, terminal = true },
}
local SKIP_LINE = {
    [1] = { guidefar = true },
    [2] = { shoulder = true, guidefar = true },
    [3] = { shoulder = true, rwyedge = true, stand = true, exit = true, hold = true, rwycl = true, guide = true },
}

-- ---------------------------------------------------------------- state
local wnd = nil
local ap = nil           -- the loaded airport table from the bridge
local ap_icao = nil      -- the airport currently drawn
local want_icao = nil    -- nearest airport the bridge last reported, if not yet loaded
local range_i = 2
local plan = false
local quality = 1        -- declutter level, raised automatically if a frame runs near MAX_VERTS
local status = "starting"
local last_err = nil
local poll_at = 0        -- next os.clock() at which to poll the bridge
local http = nil

-- ---------------------------------------------------------------- datarefs
local function bind(name, ...)
    for _, path in ipairs({ ... }) do
        if pcall(dataref, name, path) then return end
    end
    _G[name] = 0
    logMsg("AMDB OANS: no dataref for " .. name)
end
bind("amdb_lat", "sim/flightmodel/position/latitude")
bind("amdb_lon", "sim/flightmodel/position/longitude")
bind("amdb_hdg", "sim/flightmodel/position/true_psi", "sim/flightmodel/position/psi")

-- ---------------------------------------------------------------- bridge
-- Short blocking GETs on loopback. The frequent one (`get_json`) returns a tiny reply;
-- the heavy airport data is fetched only when the nearest airport changes.
local function req(url)
    if not http then
        local ok, mod = pcall(require, "socket.http")
        if not ok then return nil, "FlyWithLua socket module missing" end
        http = mod
        http.TIMEOUT = 3
    end
    local body, code = http.request(url)
    if not body or code ~= 200 then return nil, "bridge not running - start:  amdb-bridge serve --xplane" end
    local chunk = loadstring(body)
    if not chunk then return nil, "bad data from bridge" end
    local ok, t = pcall(chunk)
    if not ok or type(t) ~= "table" then return nil, "bad data from bridge" end
    return t
end

-- Which airport are we near? A tiny reply, safe to poll often.
local function poll()
    local t, err = req(string.format("%sxp/nearest?lat=%.6f&lon=%.6f", BRIDGE, amdb_lat, amdb_lon))
    if not t then status = err; return end
    if not t.icao then
        want_icao = nil
        if not ap then status = "no airport near you" end
        return
    end
    if t.icao == ap_icao then return end          -- already drawing it
    want_icao = t.icao
    -- Fetch the full data (builds on demand: returns {building=...} until ready).
    local d, derr = req(BRIDGE .. "xp/" .. t.icao)
    if not d then status = derr; return end
    if d.building then
        status = "building " .. tostring(d.building) .. " ..."   -- keep the previous airport on screen
    elseif d.icao then
        -- Half-extent of the field (metres from the reference point), for PLAN scaling.
        local ext = 500
        for _, t in ipairs(d.tiles) do
            local b = t.b
            ext = math.max(ext, math.abs(b[1]), math.abs(b[2]), math.abs(b[3]), math.abs(b[4]))
        end
        d.ext = ext
        ap = d
        ap_icao = d.icao
        want_icao = nil
        status = d.icao .. "  " .. (d.name or "")
        last_err = nil
    end
end

-- Every few seconds while the window is open.
function amdb_tick()
    if not wnd then return end
    local now = os.time()   -- wall-clock seconds
    -- Poll faster while we are waiting on a build, slower once an airport is drawn.
    if now < poll_at then return end
    poll_at = now + ((ap and not want_icao) and POLL_S or 2)
    local ok, e = pcall(poll)
    if not ok then last_err = tostring(e) end
end

-- ---------------------------------------------------------------- drawing
local function draw(w, h)
    local mh = h - BAR_H
    local txt, rect = imgui.DrawList_AddText, imgui.DrawList_AddRectFilled
    -- Real glyph metrics where ImGui will give them: guessed widths leave labels visibly
    -- off-centre and their boxes the wrong size.
    local function tsize(s)
        if imgui.CalcTextSize then
            local ok, a, b = pcall(imgui.CalcTextSize, s)
            if ok and type(a) == "number" then return a, (type(b) == "number" and b or FONT_H) end
        end
        return #s * 7, FONT_H
    end
    rect(0, 0, w, mh, C.bg)
    if not ap then
        txt(w / 2 - tsize(status) / 2, mh / 2 - 7, C.dim, status)
        return
    end

    -- Aircraft position in the airport's metre frame.
    local dlon = amdb_lon - ap.lon
    if dlon > 180 then dlon = dlon - 360 elseif dlon < -180 then dlon = dlon + 360 end
    local px_ac, py_ac = dlon * ap.mx, (amdb_lat - ap.lat) * ap.my

    -- ARC: heading up, centred on and following the aircraft. PLAN: north up, centred on
    -- the airport, scaled to show the whole field.
    local ax, ay, scale, psi, cox, coy
    if plan then
        ax, ay = w / 2, mh / 2
        scale = (math.min(w, mh) * 0.46) / ((ap.ext or 2000) + 150)
        psi, cox, coy = 0, 0, 0
    else
        ax, ay = w / 2, mh * 0.80
        scale = (mh * 0.70) / (RANGES_NM[range_i] * 1852)
        psi, cox, coy = math.rad(amdb_hdg), px_ac, py_ac
    end
    local cs, sn = math.cos(psi), math.sin(psi)
    local function P(x, y)
        local dx, dy = x - cox, y - coy
        return ax + (dx * cs - dy * sn) * scale, ay - (dx * sn + dy * cs) * scale
    end
    -- Declutter by what is actually on screen: the range whose pixel scale this view
    -- matches (PLAN of a big field is like ARC at 4 NM, of a small one like 1 NM).
    local eff_i = 1
    for i = 1, #RANGES_NM do
        if scale <= (mh * 0.70) / (RANGES_NM[i] * 1852) * 1.05 then eff_i = i end
    end

    local far = math.max(ax, w - ax, ay, mh - ay) * 1.42 / scale
    local x0, x1, y0, y1 = cox - far, cox + far, coy - far, coy + far
    local vis = {}
    for _, t in ipairs(ap.tiles) do
        local b = t.b
        if b[3] >= x0 and b[1] <= x1 and b[4] >= y0 and b[2] <= y1 then vis[#vis + 1] = t end
    end

    local far_mode = eff_i >= FAR_RANGE
    local q = far_mode and 3 or quality
    local skip_f, skip_l = SKIP_FILL[q], SKIP_LINE[q]
    local nv = 0                    -- estimated ImGui vertices this frame

    local tri = imgui.DrawList_AddTriangleFilled
    local line = imgui.DrawList_AddLine

    -- The shoulder goes down first, as a wide line along the pavement boundary: the fills
    -- then cover its inner half, leaving a band outside the pavement the way the OANS
    -- draws it. Stroked over the top it would instead eat into the pavement edge.
    if not (skip_l and skip_l.shoulder) then
        for _, t in ipairs(vis) do
            if nv >= MAX_VERTS then break end
            local ls = t.l and t.l.shoulder
            if ls then
                for _, pl in ipairs(ls) do
                    local px, py = P(pl[1], pl[2])
                    for i = 3, #pl, 2 do
                        local qx, qy = P(pl[i], pl[i + 1])
                        if math.max(px, qx) >= 0 and math.min(px, qx) <= w and math.max(py, qy) >= 0 and math.min(py, qy) <= mh then
                            line(px, py, qx, qy, C.shoulder, SHOULDER_W)
                            nv = nv + 8
                        end
                        px, py = qx, qy
                    end
                end
            end
        end
    end

    for _, layer in ipairs(FILLS) do
        if not (skip_f and skip_f[layer]) then
            local col = C[layer]
            if q >= 3 and layer == "runway" then col = C.runway_far end
            for _, t in ipairs(vis) do
                if nv >= MAX_VERTS then break end
                local f = t.f and t.f[layer]
                if f then
                    for i = 1, #f, 6 do
                        local px, py = P(f[i], f[i + 1])
                        local qx, qy = P(f[i + 2], f[i + 3])
                        local rx, ry = P(f[i + 4], f[i + 5])
                        -- Tiles are only a coarse prefilter; draw what really lands on screen.
                        if math.max(px, qx, rx) >= 0 and math.min(px, qx, rx) <= w and math.max(py, qy, ry) >= 0 and math.min(py, qy, ry) <= mh then
                            tri(px, py, qx, qy, rx, ry, col)
                            nv = nv + 6
                        end
                    end
                end
            end
        end
    end

    for _, spec in ipairs(LINES) do
        local layer, thick = spec[1], spec[2]
        if not (skip_l and skip_l[layer]) and (layer ~= "stand" or eff_i <= 2) then
            local col = C[layer]
            local cost = thick > 1.0 and 8 or 6      -- ImGui verts per antialiased segment
            for _, t in ipairs(vis) do
                if nv >= MAX_VERTS then break end
                local ls = t.l and t.l[layer]
                if ls then
                    for _, pl in ipairs(ls) do
                        local px, py = P(pl[1], pl[2])
                        for i = 3, #pl, 2 do
                            local qx, qy = P(pl[i], pl[i + 1])
                            if math.max(px, qx) >= 0 and math.min(px, qx) <= w and math.max(py, qy) >= 0 and math.min(py, qy) <= mh then
                                line(px, py, qx, qy, col, thick)
                                nv = nv + cost
                            end
                            px, py = qx, qy
                        end
                    end
                end
            end
        end
    end

    -- Labels: white runway numbers in a black box, yellow taxiway letters, cyan terminal
    -- names, small grey stand numbers, each with a one-pixel shadow so it stays legible on
    -- grey pavement. Most important kind first, and a label that would land on one already
    -- drawn is dropped: that, far more than colour, is what keeps the picture readable.
    local placed = {}
    local function label(x, y, col, s, big)
        local sc = big and 1.7 or 1.0
        local lw, lh = tsize(s)
        lw, lh = lw * sc, lh * sc
        local x0, y0, x1, y1 = x - lw / 2, y - lh / 2, x + lw / 2, y + lh / 2
        if x0 < 2 or y0 < HEADER_H + 2 or x1 > w - 2 or y1 > mh - 2 then return end
        for i = 1, #placed do
            local r = placed[i]
            if x0 < r[3] and x1 > r[1] and y0 < r[4] and y1 > r[2] then return end
        end
        placed[#placed + 1] = { x0 - 3, y0 - 2, x1 + 3, y1 + 2 }
        if big then
            rect(x0 - 5, y0 - 3, x1 + 5, y1 + 3, C.rwy_box)
            if imgui.SetWindowFontScale then imgui.SetWindowFontScale(sc) end
            txt(x0, y0, col, s)
            if imgui.SetWindowFontScale then imgui.SetWindowFontScale(1.0) end
            nv = nv + #s * 4 + 6
        else
            txt(x0 + 1, y0 + 1, C.shadow, s)
            txt(x0, y0, col, s)
            nv = nv + #s * 8                  -- a glyph quad each, drawn twice
        end
    end
    local LABEL_COL = { [RWY] = C.rwy_txt, [TWY] = C.twy_txt, [TERM] = C.term_txt, [STAND] = C.std_txt }
    for _, kind in ipairs({ RWY, TWY, TERM, STAND }) do
        if eff_i <= LABEL_MAX_RANGE[kind] then
            local col = LABEL_COL[kind]
            for _, t in ipairs(vis) do
                local tx = t.t
                if tx then
                    for i = 1, #tx, 4 do
                        if tx[i + 3] == kind then
                            local sx, sy = P(tx[i], tx[i + 1])
                            label(sx, sy, col, tx[i + 2], kind == RWY)
                        end
                    end
                end
            end
        end
    end

    -- ARC: the range arc with a compass scale, and a broken half-range arc, as on the ND.
    if not plan then
        local rpx = RANGES_NM[range_i] * 1852 * scale
        local function pt(a, r)
            local d = math.rad(a)
            return ax + math.sin(d) * r, ay - math.cos(d) * r
        end
        local ppx, ppy
        for a = -60, 60, 3 do
            local x, y = pt(a, rpx)
            if ppx then line(ppx, ppy, x, y, C.ring, 1.2) end
            ppx, ppy = x, y
        end
        ppx = nil
        for a = -60, 60, 6 do
            local x, y = pt(a, rpx * 0.5)
            if ppx and a % 12 == 0 then line(ppx, ppy, x, y, C.dim, 1.0) end
            ppx, ppy = x, y
        end
        for a = -60, 60, 10 do
            local major = a % 30 == 0
            local x1, y1 = pt(a, rpx)
            local x2, y2 = pt(a, rpx + (major and 11 or 6))
            line(x1, y1, x2, y2, C.tick, major and 1.6 or 1.0)
            -- Only the inner three fit on the page at this window size.
            if major and math.abs(a) <= 30 then
                local s = string.format("%03d", math.floor(amdb_hdg + a + 0.5) % 360)
                local hx, hy = pt(a, rpx + 24)
                local lw = tsize(s)
                txt(hx - lw / 2 + 1, hy - 6, C.shadow, s)
                txt(hx - lw / 2, hy - 7, C.tick, s)
            end
        end
        nv = nv + 700
    end

    -- Ownship: fixed and nose-up in ARC; at its real position, rotated to heading, in PLAN.
    local sx, sy = ax, ay
    if plan then sx, sy = P(px_ac, py_ac) end
    local rot = plan and math.rad(amdb_hdg) or 0
    local rc, rs = math.cos(rot), math.sin(rot)
    local function seg(x1, y1, x2, y2, t)
        line(sx + x1 * rc - y1 * rs, sy + x1 * rs + y1 * rc, sx + x2 * rc - y2 * rs, sy + x2 * rs + y2 * rc, C.own, t or 2.6)
    end
    seg(0, -15, 0, 13, 3.0)                        -- fuselage
    seg(0, -3, -14, 7); seg(0, -3, 14, 7)          -- swept wings
    seg(0, 10, -6, 15); seg(0, 10, 6, 15)          -- tailplane
    nv = nv + 48

    -- Readout strip, so nothing is printed straight onto the map.
    rect(0, 0, w, HEADER_H, C.hdr)
    txt(8, 4, C.ring, plan and "PLAN" or string.format("ARC %s NM", tostring(RANGES_NM[range_i])))
    local hs = string.format("HDG %03d", math.floor(amdb_hdg + 0.5) % 360)
    txt(w / 2 - tsize(hs) / 2, 4, C.ring, hs)
    local rs2 = ap.icao or ""
    txt(w - tsize(rs2) - 8, 4, C.dim, rs2)
    -- Anything the map cannot say for itself: a build in progress, or the bridge gone away
    -- while an airport is still on screen.
    local note = want_icao and ("BUILDING " .. want_icao) or (status:sub(1, 4) ~= (ap.icao or "") and status or nil)
    if note then
        txt(w / 2 - tsize(note) / 2, HEADER_H + 7, C.amber, note)
    end
    nv = nv + 160

    -- Keep the next frame inside the index limit: shed a level of detail when this one ran
    -- close to the budget, and take it back once there is comfortable room again.
    if nv > MAX_VERTS * 0.92 then
        quality = math.min(3, quality + 1)
    elseif nv < MAX_VERTS * 0.45 and quality > 1 then
        quality = quality - 1
    end
end

function amdb_build(wnd_in, x, y)
    local w = imgui.GetWindowWidth()
    local h = (imgui.GetWindowHeight and imgui.GetWindowHeight()) or w
    local ok, err = pcall(draw, w, h)
    if not ok then last_err = tostring(err) end
    imgui.DrawList_AddRectFilled(0, h - BAR_H, w, h, C.bg)
    imgui.Dummy(w - 20, h - BAR_H - 12)
    if imgui.Button(" + ") and range_i > 1 then range_i = range_i - 1 end
    imgui.SameLine()
    if imgui.Button(" - ") and range_i < #RANGES_NM then range_i = range_i + 1 end
    imgui.SameLine()
    if imgui.Button(plan and "ARC" or "PLAN") then plan = not plan end
    if last_err then
        imgui.SameLine()
        imgui.TextUnformatted("! " .. last_err)
    end
end

-- ---------------------------------------------------------------- window
function amdb_open()
    if wnd then return end
    poll_at = 0
    wnd = float_wnd_create(560, 620, 1, true)
    float_wnd_set_title(wnd, "AMDB OANS")
    float_wnd_set_imgui_builder(wnd, "amdb_build")
    float_wnd_set_onclose(wnd, "amdb_closed")
end

function amdb_closed() wnd = nil end
function amdb_close() if wnd then float_wnd_destroy(wnd) end wnd = nil end
function amdb_toggle() if wnd then amdb_close() else amdb_open() end end

do_often("amdb_tick()")
add_macro("AMDB OANS", "amdb_open()", "amdb_close()", "deactivate")
create_command("amdb/oans/toggle", "Toggle the AMDB airport moving map", "amdb_toggle()", "", "")
