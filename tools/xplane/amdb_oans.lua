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
local POLL_S = 4                  -- seconds between "which airport am I near" checks (a tiny reply)

-- A380 OANS palette. ImGui colours are 0xAABBGGRR.
local C = {
    bg = 0xFF000000, apron = 0xFF505050, taxiway = 0xFF686868, rwyext = 0xFF383838,
    runway = 0xFF262626, building = 0xFF705A48, terminal = 0xFFC8C800, stand = 0xFF0090B0,
    guide = 0xFF00D8FF, exit = 0xFF00D8FF, hold = 0xFF2828FF, rwycl = 0xFFE8E8E8,
    twy_bg = 0xFF00D8FF, twy_fg = 0xFF000000, rwy_txt = 0xFFFFFFFF, std_txt = 0xFFB8B8B8,
    term_txt = 0xFFC8C800, own = 0xFF00D8FF, ring = 0xFFE8E8E8, dim = 0xFF909090,
}
local FILLS = { "apron", "taxiway", "rwyext", "runway", "building", "terminal" }
local LINES = { { "stand", 1.0 }, { "guide", 1.6 }, { "exit", 1.6 }, { "hold", 2.6 }, { "rwycl", 1.2 } }
local TWY, RWY, STAND, TERM = 1, 2, 3, 4
local LABEL_MAX_RANGE = { [TWY] = 3, [RWY] = 5, [STAND] = 1, [TERM] = 4 }

-- ---------------------------------------------------------------- state
local wnd = nil
local ap = nil           -- the loaded airport table from the bridge
local ap_icao = nil      -- the airport currently drawn
local want_icao = nil    -- nearest airport the bridge last reported, if not yet loaded
local range_i = 2
local plan = false
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
    imgui.DrawList_AddRectFilled(0, 0, w, mh, C.bg)
    if not ap then
        imgui.DrawList_AddText(12, 12, C.dim, status)
        return
    end

    local dlon = amdb_lon - ap.lon
    if dlon > 180 then dlon = dlon - 360 elseif dlon < -180 then dlon = dlon + 360 end
    local ox, oy = dlon * ap.mx, (amdb_lat - ap.lat) * ap.my

    local rng_m = RANGES_NM[range_i] * 1852
    local ax, ay, scale, psi
    if plan then
        ax, ay = w / 2, mh / 2
        scale = (math.min(w, mh) * 0.45) / rng_m
        psi = 0
    else
        ax, ay = w / 2, mh * 0.80
        scale = (mh * 0.70) / rng_m
        psi = math.rad(amdb_hdg)
    end
    local cs, sn = math.cos(psi), math.sin(psi)
    local function P(x, y)
        local dx, dy = x - ox, y - oy
        return ax + (dx * cs - dy * sn) * scale, ay - (dx * sn + dy * cs) * scale
    end

    local far = math.max(ax, w - ax, ay, mh - ay) * 1.42 / scale
    local x0, x1, y0, y1 = ox - far, ox + far, oy - far, oy + far
    local vis = {}
    for _, t in ipairs(ap.tiles) do
        local b = t.b
        if b[3] >= x0 and b[1] <= x1 and b[4] >= y0 and b[2] <= y1 then vis[#vis + 1] = t end
    end

    local tri = imgui.DrawList_AddTriangleFilled
    for _, layer in ipairs(FILLS) do
        local col = C[layer]
        for _, t in ipairs(vis) do
            local f = t.f and t.f[layer]
            if f then
                for i = 1, #f, 6 do
                    local px, py = P(f[i], f[i + 1])
                    local qx, qy = P(f[i + 2], f[i + 3])
                    local rx, ry = P(f[i + 4], f[i + 5])
                    tri(px, py, qx, qy, rx, ry, col)
                end
            end
        end
    end

    local line = imgui.DrawList_AddLine
    for _, spec in ipairs(LINES) do
        local layer, thick = spec[1], spec[2]
        if layer ~= "stand" or range_i <= 2 then
            local col = C[layer]
            for _, t in ipairs(vis) do
                local ls = t.l and t.l[layer]
                if ls then
                    for _, pl in ipairs(ls) do
                        local px, py = P(pl[1], pl[2])
                        for i = 3, #pl, 2 do
                            local qx, qy = P(pl[i], pl[i + 1])
                            line(px, py, qx, qy, col, thick)
                            px, py = qx, qy
                        end
                    end
                end
            end
        end
    end

    local txt, rect = imgui.DrawList_AddText, imgui.DrawList_AddRectFilled
    for _, kind in ipairs({ STAND, TWY, TERM, RWY }) do
        if range_i <= LABEL_MAX_RANGE[kind] then
            for _, t in ipairs(vis) do
                local tx = t.t
                if tx then
                    for i = 1, #tx, 4 do
                        if tx[i + 3] == kind then
                            local sx, sy = P(tx[i], tx[i + 1])
                            if sx > 0 and sx < w and sy > 0 and sy < mh then
                                local s = tx[i + 2]
                                local half = #s * 3.5
                                if kind == TWY then
                                    rect(sx - half - 3, sy - 8, sx + half + 3, sy + 8, C.twy_bg)
                                    txt(sx - half, sy - 7, C.twy_fg, s)
                                elseif kind == RWY then
                                    txt(sx - half, sy - 7, C.rwy_txt, s)
                                elseif kind == STAND then
                                    txt(sx - half, sy - 7, C.std_txt, s)
                                else
                                    txt(sx - half, sy - 7, C.term_txt, s)
                                end
                            end
                        end
                    end
                end
            end
        end
    end

    local rpx = rng_m * scale
    if plan then
        imgui.DrawList_AddCircle(ax, ay, rpx, C.ring, 64, 1.0)
    else
        local px, py
        for a = -60, 60, 3 do
            local r = math.rad(a)
            local x, y = ax + math.sin(r) * rpx, ay - math.cos(r) * rpx
            if px then line(px, py, x, y, C.ring, 1.0) end
            px, py = x, y
        end
    end

    local rot = plan and math.rad(amdb_hdg) or 0
    local rc, rs = math.cos(rot), math.sin(rot)
    local function seg(x1, y1, x2, y2)
        line(ax + x1 * rc - y1 * rs, ay + x1 * rs + y1 * rc, ax + x2 * rc - y2 * rs, ay + x2 * rs + y2 * rc, C.own, 3.0)
    end
    seg(0, -14, 0, 12); seg(-13, -1, 13, -1); seg(-5, 11, 5, 11)

    txt(10, 8, C.ring, string.format("%s  %s NM", plan and "PLAN" or "ARC", tostring(RANGES_NM[range_i])))
    txt(10, 24, C.dim, status)
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
