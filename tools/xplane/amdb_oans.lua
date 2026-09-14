-- AMDB OANS - an A380-style airport moving map for X-Plane 12 (FlyWithLua NG+)
--
-- Draws the airport you are at from amdbgen data: pavement, runways, terminals,
-- taxiway guidance lines, holding positions, and taxiway, runway, stand and terminal
-- labels, with your aircraft on it. Heading-up ARC view like the A380 OANS, or
-- north-up PLAN.
--
-- Open it from Plugins > FlyWithLua > FlyWithLua Macros > "AMDB OANS", or bind a key
-- to the command "amdb/oans/toggle". The data comes from `amdbgen xplane`, which also
-- installs this script and sets the data folder on the next line.

local DATA_DIR = "D:/OANS Cache/airports" --@DATA_DIR@

local LOAD_RADIUS_M = 12000      -- load an airport when its reference point is this close
local RANGES_NM = { 0.25, 0.5, 1, 2, 4 }
local BAR_H = 34                 -- height of the control bar, px

-- A380 OANS palette. ImGui colours are 0xAABBGGRR.
local C = {
    bg       = 0xFF000000,
    apron    = 0xFF505050,
    taxiway  = 0xFF686868,
    rwyext   = 0xFF383838,
    runway   = 0xFF262626,
    building = 0xFF705A48,
    terminal = 0xFFC8C800,
    stand    = 0xFF0090B0,
    guide    = 0xFF00D8FF,
    exit     = 0xFF00D8FF,
    hold     = 0xFF2828FF,
    rwycl    = 0xFFE8E8E8,
    twy_bg   = 0xFF00D8FF,
    twy_fg   = 0xFF000000,
    rwy_txt  = 0xFFFFFFFF,
    std_txt  = 0xFFB8B8B8,
    term_txt = 0xFFC8C800,
    own      = 0xFF00D8FF,
    ring     = 0xFFE8E8E8,
    dim      = 0xFF909090,
}

-- Drawn bottom to top.
local FILLS = { "apron", "taxiway", "rwyext", "runway", "building", "terminal" }
local LINES = { { "stand", 1.0 }, { "guide", 1.6 }, { "exit", 1.6 }, { "hold", 2.6 }, { "rwycl", 1.2 } }
-- Label kinds from the data file, and the widest range (index into RANGES_NM) each shows at.
local TWY, RWY, STAND, TERM = 1, 2, 3, 4
local LABEL_MAX_RANGE = { [TWY] = 3, [RWY] = 5, [STAND] = 1, [TERM] = 4 }

-- ---------------------------------------------------------------- state
local wnd = nil
local index = nil        -- flat: icao, lat, lon, icao, lat, lon, ...
local ap = nil           -- the loaded airport table
local ap_icao = nil
local range_i = 2
local plan = false
local status = "starting"
local last_err = nil

-- ---------------------------------------------------------------- datarefs
local function bind(name, ...)
    for _, path in ipairs({ ... }) do
        if pcall(dataref, name, path) then return true end
    end
    _G[name] = 0
    logMsg("AMDB OANS: no dataref for " .. name)
    return false
end
bind("amdb_lat", "sim/flightmodel/position/latitude")
bind("amdb_lon", "sim/flightmodel/position/longitude")
bind("amdb_hdg", "sim/flightmodel/position/true_psi", "sim/flightmodel/position/psi")

-- ---------------------------------------------------------------- data
local function load_index()
    local ok, t = pcall(dofile, DATA_DIR .. "/index.lua")
    if ok and type(t) == "table" then
        index = t
        status = string.format("%d airports available", math.floor(#t / 3))
    else
        index = {}
        status = "no data in " .. DATA_DIR .. ": run  amdbgen xplane --all"
        last_err = tostring(t)
    end
end

local function nearest()
    if not index then return nil end
    local lat, lon = amdb_lat, amdb_lon
    local cl = math.cos(math.rad(lat))
    local best, bd = nil, LOAD_RADIUS_M * LOAD_RADIUS_M
    for i = 1, #index, 3 do
        local dlon = index[i + 2] - lon
        if dlon > 180 then dlon = dlon - 360 elseif dlon < -180 then dlon = dlon + 360 end
        local dx = dlon * 111195 * cl
        local dy = (index[i + 1] - lat) * 111195
        local d = dx * dx + dy * dy
        if d < bd then bd, best = d, index[i] end
    end
    return best
end

-- Once a second while the window is open: load the nearest airport when it changes.
function amdb_tick()
    if not wnd then return end
    if not index then load_index() end
    local icao = nearest()
    if icao and icao ~= ap_icao then
        local ok, t = pcall(dofile, DATA_DIR .. "/" .. icao .. "/oans.lua")
        ap_icao = icao           -- do not retry a missing file every second
        if ok and type(t) == "table" then
            ap = t
            status = icao .. "  " .. (t.name or "")
            last_err = nil
        else
            ap = nil
            status = icao .. ": no OANS data (run  amdbgen xplane " .. icao .. ")"
            last_err = tostring(t)
        end
    elseif not icao and not ap then
        status = string.format("no airport within %d NM", math.floor(LOAD_RADIUS_M / 1852))
    end
    -- Leaving an airport keeps it on screen, like the real OANS, until another loads.
end

-- ---------------------------------------------------------------- drawing
local function draw(w, h)
    local mh = h - BAR_H
    imgui.DrawList_AddRectFilled(0, 0, w, mh, C.bg)
    if not ap then
        imgui.DrawList_AddText(12, 12, C.dim, status)
        return
    end

    -- Aircraft position in the airport's metre frame (linearised at the reference point,
    -- which matches amdbgen's projection to well under a metre across an airport).
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

    -- Tiles whose box overlaps what the window can show.
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

    -- Range arc (ARC) or ring (PLAN).
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

    -- Ownship: nose up in ARC, rotated to the heading in PLAN.
    local rot = plan and math.rad(amdb_hdg) or 0
    local rc, rs = math.cos(rot), math.sin(rot)
    local function seg(x1, y1, x2, y2)
        line(ax + x1 * rc - y1 * rs, ay + x1 * rs + y1 * rc, ax + x2 * rc - y2 * rs, ay + x2 * rs + y2 * rc, C.own, 3.0)
    end
    seg(0, -14, 0, 12)
    seg(-13, -1, 13, -1)
    seg(-5, 11, 5, 11)

    txt(10, 8, C.ring, string.format("%s  %s NM", plan and "PLAN" or "ARC", tostring(RANGES_NM[range_i])))
    txt(10, 24, C.dim, status)
end

function amdb_build(wnd_in, x, y)
    local w = imgui.GetWindowWidth()
    local h = (imgui.GetWindowHeight and imgui.GetWindowHeight()) or w
    local ok, err = pcall(draw, w, h)
    if not ok then last_err = tostring(err) end
    -- Cover anything that spilled below the map, then the controls.
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
    index = nil          -- reread, so newly built airports appear
    wnd = float_wnd_create(560, 620, 1, true)
    float_wnd_set_title(wnd, "AMDB OANS")
    float_wnd_set_imgui_builder(wnd, "amdb_build")
    float_wnd_set_onclose(wnd, "amdb_closed")
    amdb_tick()
end

function amdb_closed()
    wnd = nil
end

function amdb_close()
    if wnd then float_wnd_destroy(wnd) end
    wnd = nil
end

function amdb_toggle()
    if wnd then amdb_close() else amdb_open() end
end

do_often("amdb_tick()")
add_macro("AMDB OANS", "amdb_open()", "amdb_close()", "deactivate")
create_command("amdb/oans/toggle", "Toggle the AMDB airport moving map", "amdb_toggle()", "", "")
