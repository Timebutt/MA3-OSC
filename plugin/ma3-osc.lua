-- MA3-OSC grandMA3 plugin: sends executor state to MA3-OSC over OSC.
-- Based on pam-OSC by xxpasixx (https://github.com/xxpasixx/pam-osc).
-- Copyright (C) 2024 xxpasixx, 2026 MA3-OSC contributors
-- This program is free software: you can redistribute it and/or modify
-- it under the terms of the GNU General Public License as published by
-- the Free Software Foundation, either version 3 of the License, or
-- (at your option) any later version.
-- This program is distributed in the hope that it will be useful,
-- but WITHOUT ANY WARRANTY; without even the implied warranty of
-- MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
-- GNU General Public License for more details.
-- You should have received a copy of the GNU General Public License
-- along with this program.  If not, see <https://www.gnu.org/licenses/>.
--
-- Changes compared to pam-OSC.lua:
--  * sends /pam/heartbeat once per second, so MA3-OSC can tell when MA is gone
--  * every poll runs protected: a Lua error is printed and the loop keeps running
--  * names containing " , or ; no longer break the OSC message
--  * fader values are sent as whole numbers; the undefined `faderEnd` is gone
--  * a page that doesn't exist (fixed page number) no longer stops the plugin
--  * uses IsRunningPlayback (newer MA3) instead of the deprecated HasActivePlayback
--  * if SendOSC fails, explains once how to fix the OSC setup instead of failing silently
--  * executors set to Fix on another page are reported too, under their own page
--  * reports the command line, and finishes a waiting command ("Assign Sequence 5")
--    when an executor button on the surface is pressed, like a console's executor key
--  * answers read-only requests from MA3-OSC's agent interface (MCP)

-- OSC line in Menu > In & Out > OSC that points to MA3-OSC. To use another
-- line without editing this file: SetGlobalVariable "ma3OscEntry" 3
local DEFAULT_OSC_ENTRY = 2
local oscEntry = DEFAULT_OSC_ENTRY

-- Executors to watch. Add ranges here if you map more executors.
local executorsToWatch = {}
local function watch(from, to)
    for i = from, to do
        executorsToWatch[#executorsToWatch + 1] = i
    end
end
watch(101, 122)
watch(191, 198)
watch(201, 222)
watch(291, 298)
watch(301, 322)
watch(401, 422)

local TICK = 1 / 10            -- poll interval (seconds)
local HEARTBEAT_TICKS = 10     -- heartbeat every 10 ticks = 1 s
local RESEND_TICKS = 15        -- "automatic resend buttons" period

local oldFader, oldButton, oldColor, oldName, oldPage = {}, {}, {}, {}, {}
local MASTERS = { "highlight", "lowlight", "solo", "blind" }
local oldMaster = {}
local oldTimecodes = {}
local oldDeskLocked = nil

local sendFailed = false

-- MA3-OSC sets this global variable to "Page <p>.<exec>" when an executor
-- button is pressed while the command line holds a command.
local TARGET_VAR = "ma3OscTarget"
-- Commands whose destination follows "At" once a source is given
-- ("Assign Sequence 5 At Page 1.201"). Others take the executor directly
-- ("Store Page 1.201", "Edit Page 1.201", "Store Cue 5 Page 1.201").
local AT_KEYWORDS = { "assign", "copy", "move", "clone" }
local oldCmdline = nil
local clearMethod = nil
local clearFailed = false

-- MA3-OSC's agent interface (MCP) sets this global variable to
-- "<id>:<base64 request>". The reply goes back as /ma3osc/rpc messages.
local RPC_VAR = "ma3OscRpc"
local RPC_PROTOCOL = 1
-- Base64 characters per reply message. MA drops (with only a System Monitor
-- note) any OSC packet over 2048 bytes, and Cmd still returns "OK".
local RPC_CHUNK = 1700
local RPC_MAX_CHUNKS = 64

-- Cmd() returns "OK" on success (grandMA3 2.5), older versions may return
-- nothing or true. Anything else is MA's error text, e.g. "Illegal object".
local function cmdOk(result)
    return result == nil or result == true or string.upper(tostring(result)) == "OK"
end

local function send(addr, typ, value)
    local result = Cmd('SendOSC ' .. oscEntry .. ' "' .. addr .. ',' .. typ .. ',' .. tostring(value) .. '"')
    -- Note: an over-long message is dropped by MA but still returns "OK".
    local failed = not cmdOk(result)
    if failed and not sendFailed then
        Printf("MA3-OSC plugin: SendOSC on OSC line " .. oscEntry .. " failed (" .. tostring(result) .. ").")
        Printf("  Check Menu > In & Out > OSC: line " .. oscEntry .. " exists, 'Send' is Yes, 'Enable Output' is on,")
        Printf("  and its destination is the MA3-OSC computer, port 9004.")
        Printf("  To use another line: SetGlobalVariable \"ma3OscEntry\" <number>, then restart the plugin.")
    elseif not failed and sendFailed then
        Printf("MA3-OSC plugin: SendOSC works again")
    end
    sendFailed = failed
end

-- Newer MA3 versions renamed HasActivePlayback to IsRunningPlayback and print a
-- deprecation warning for every call of the old name. Detect once which exists.
local playbackMethod = nil
local function isRunning(obj)
    if playbackMethod == nil then
        local ok = pcall(function() return obj:IsRunningPlayback() end)
        playbackMethod = ok and "IsRunningPlayback" or "HasActivePlayback"
    end
    return obj[playbackMethod](obj) and true or false
end

-- Text that is safe inside SendOSC "addr,s,text" and inside our "seq;cue" field.
local function clean(text)
    if text == nil then
        return ""
    end
    text = tostring(text)
    text = string.gsub(text, '"', "'")
    text = string.gsub(text, ',', ' ')
    text = string.gsub(text, ';', ':')
    text = string.gsub(text, '[%c]', ' ')
    return text
end

local function appearanceColor(sequence)
    local result = "255;255;255;255"
    local function fromAppearance(a)
        if a == nil then
            return
        end
        if a['BACKR'] == 0 and a['BACKG'] == 0 and a['BACKB'] == 0 and a['BACKALPHA'] == 0 then
            result = "255;255;255;255"
        else
            result = a['BACKR'] .. ";" .. a['BACKG'] .. ";" .. a['BACKB'] .. ";" .. a['BACKALPHA']
        end
    end
    fromAppearance(sequence["APPEARANCE"])
    if sequence.preferCueAppearance == true and sequence:CurrentChild() then
        local cue = sequence:CurrentChild()[1]
        if cue and cue.Appearance then
            fromAppearance(cue.Appearance)
        end
    end
    return result
end

local function displayName(sequence)
    return clean(sequence["NAME"]) .. ";" .. clean(sequence["CUENAME"])
end

local function masterEnabled(name)
    local grand = MasterPool()['Grand']
    return grand ~= nil and grand[name] ~= nil and grand[name]['FADERENABLED'] and true or false
end

-- Children of an MA object (or of a plain Lua table, as in the tests).
local function children(obj)
    local ok, list = pcall(function() return obj:Children() end)
    if ok and type(list) == "table" then
        return list
    end
    return type(obj) == "table" and obj or {}
end

-- True when an executor is set to Fix: it then stays on every page.
local function isFixed(exec)
    local ok, v = pcall(function() return exec.FIX end)
    if not ok then
        return false
    end
    return v == true or v == 1 or v == "Yes" or v == "On"
end

-- Fixed executors of all pages, by executor number: { [no] = { exec, page } }.
-- The lowest page wins if more than one page reports the same number as fixed.
local function fixedExecutors()
    local result = {}
    local pages = {}
    for _, p in pairs(children(DataPool().Pages)) do
        if type(p) == "table" or type(p) == "userdata" then
            pages[#pages + 1] = p
        end
    end
    table.sort(pages, function(a, b) return (a.index or 0) < (b.index or 0) end)
    for _, p in ipairs(pages) do
        for _, exec in pairs(children(p)) do
            if exec.No and result[exec.No] == nil and isFixed(exec) then
                result[exec.No] = { exec = exec, page = p.index }
            end
        end
    end
    return result
end

-- Text in the user's command line, "" if it can't be read.
local function cmdlineText(cmdline)
    local ok, text = pcall(function() return cmdline.CMDTEXT end)
    if ok and type(text) == "string" then
        return text
    end
    return ""
end

-- The waiting command with the executor as its destination.
local function completeCommand(text, target)
    text = string.gsub(text, "%s+$", "")
    local words = {}
    for w in string.gmatch(text, "%S+") do
        if string.sub(w, 1, 1) ~= "/" then -- skip options like /Merge
            words[#words + 1] = string.lower(w)
        end
    end
    local needsAt = false
    if #words >= 2 and words[#words] ~= "at" and #words[1] >= 2 then
        for _, k in ipairs(AT_KEYWORDS) do
            if string.sub(k, 1, #words[1]) == words[1] then -- "As" is short for Assign
                needsAt = true
            end
        end
    end
    return text .. (needsAt and " At " or " ") .. target
end

-- Ways to empty the user's command line; the first that works is remembered.
local CLEARERS = {
    { "ClearCmd()", function(c) c:ClearCmd() end },
    { "the CLEARCMD property", function(c) c.CLEARCMD = "" end },
    { "the Escape key", function() Keyboard(1, "press", "Escape"); Keyboard(1, "release", "Escape") end },
}

local function clearCmdline(cmdline)
    local order = {}
    for _, cl in ipairs(CLEARERS) do
        if cl[1] == clearMethod then table.insert(order, 1, cl) else order[#order + 1] = cl end
    end
    for _, cl in ipairs(order) do
        if pcall(cl[2], cmdline) and cmdlineText(cmdline) == "" then
            if clearMethod ~= cl[1] then
                clearMethod = cl[1]
                Printf("MA3-OSC plugin: the command line is cleared with " .. cl[1])
            end
            return
        end
    end
    if not clearFailed then
        clearFailed = true
        Printf("MA3-OSC plugin: could not clear the command line after finishing a command; press Esc to clear it")
    end
end

-- Finish a waiting command with the executor MA3-OSC reported, then report
-- the command line whenever it changes.
local function handleCmdline(forceReload)
    local okObj, cmdline = pcall(CmdObj)
    if not okObj then
        cmdline = nil
    end
    local text = cmdline and cmdlineText(cmdline) or ""
    local target = GetVar(GlobalVars(), TARGET_VAR)
    if target ~= nil and target ~= "" then
        SetVar(GlobalVars(), TARGET_VAR, "")
        if text ~= "" then
            local command = completeCommand(text, tostring(target))
            Printf("MA3-OSC plugin: " .. command)
            local result = Cmd(command)
            if not cmdOk(result) then
                Printf("MA3-OSC plugin: '" .. command .. "' failed (" .. tostring(result) .. ")")
            end
            clearCmdline(cmdline)
            text = cmdlineText(cmdline)
        end
    end
    if text ~= oldCmdline or forceReload then
        oldCmdline = text
        send("/cmdline", "s", clean(text))
    end
end

-- ------------------------------------------------------------------ requests
-- Read-only requests from MA3-OSC (its MCP server for agents). A request is
-- "<id>:<base64 text>"; the text is a tool name, then one "key=value" per line.
-- The reply is JSON {"ok":true,"result":...} or {"ok":false,"error":"..."},
-- base64 encoded and sent in chunks as /ma3osc/rpc <id> <index> <count> <chunk>.

local B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
local B64_INDEX = {}
for i = 1, 64 do
    B64_INDEX[string.byte(B64, i)] = i - 1
end

-- Plain arithmetic instead of bit operators, so it runs on any Lua version.
local function b64encode(data)
    local out = {}
    for i = 1, #data, 3 do
        local a, b, c = string.byte(data, i, i + 2)
        local n = a * 65536 + (b or 0) * 256 + (c or 0)
        local d1, d2 = math.floor(n / 262144) % 64, math.floor(n / 4096) % 64
        local d3, d4 = math.floor(n / 64) % 64, n % 64
        out[#out + 1] = string.sub(B64, d1 + 1, d1 + 1) .. string.sub(B64, d2 + 1, d2 + 1) ..
            (b and string.sub(B64, d3 + 1, d3 + 1) or "=") .. (c and string.sub(B64, d4 + 1, d4 + 1) or "=")
    end
    return table.concat(out)
end

local function b64decode(text)
    text = (string.gsub(text, "[^%w%+/]", ""))
    local out = {}
    for i = 1, #text, 4 do
        local n, k = 0, 0
        for j = i, math.min(i + 3, #text) do
            n = n * 64 + (B64_INDEX[string.byte(text, j)] or 0)
            k = k + 1
        end
        for _ = k + 1, 4 do
            n = n * 64
        end
        local bytes = string.char(math.floor(n / 65536) % 256, math.floor(n / 256) % 256, n % 256)
        out[#out + 1] = string.sub(bytes, 1, math.floor(k * 3 / 4))
    end
    return table.concat(out)
end

local JSON_ESCAPES = { ['"'] = '\\"', ['\\'] = '\\\\', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }

local function jsonString(s)
    return '"' .. (string.gsub(s, '[%c"\\]', function(c)
        return JSON_ESCAPES[c] or string.format("\\u%04x", string.byte(c))
    end)) .. '"'
end

-- Tables with keys 1..n (or empty) become arrays, others objects.
local function toJson(v)
    local t = type(v)
    if t == "nil" then
        return "null"
    elseif t == "boolean" then
        return tostring(v)
    elseif t == "number" then
        if v ~= v or v == math.huge or v == -math.huge then
            return "null"
        elseif v == math.floor(v) and math.abs(v) < 1e15 then
            return string.format("%d", v)
        end
        return string.format("%.10g", v)
    elseif t == "table" then
        local parts = {}
        if #v > 0 or next(v) == nil then
            for i = 1, #v do
                parts[i] = toJson(v[i])
            end
            return "[" .. table.concat(parts, ",") .. "]"
        end
        local keys = {}
        for k in pairs(v) do
            keys[#keys + 1] = tostring(k)
        end
        table.sort(keys)
        for _, k in ipairs(keys) do
            parts[#parts + 1] = jsonString(k) .. ":" .. toJson(v[k])
        end
        return "{" .. table.concat(parts, ",") .. "}"
    end
    return jsonString(tostring(v))
end

-- Calls f if it exists in this MA version; nil if missing or on error.
local function try(f, ...)
    if type(f) ~= "function" then
        return nil
    end
    local ok, v = pcall(f, ...)
    if ok then
        return v
    end
    return nil
end

-- A property as a plain value (string, number, boolean or nil).
local function prop(obj, ...)
    if obj == nil then
        return nil
    end
    for _, name in ipairs({ ... }) do
        local ok, v = pcall(function() return obj[name] end)
        if ok and v ~= nil then
            local t = type(v)
            if t == "string" or t == "number" or t == "boolean" then
                return v
            end
            return tostring(v)
        end
    end
    return nil
end

local function className(obj)
    local ok, v = pcall(function() return obj:GetClass() end)
    if ok and type(v) == "string" then
        return v
    end
    return nil
end

local function intArg(args, name, default, low, high)
    local v = args[name]
    if v == nil or v == "" then
        return default
    end
    local n = tonumber(v)
    if n == nil or n ~= math.floor(n) or n < low or n > high then
        error(name .. " must be a whole number from " .. low .. " to " .. high, 0)
    end
    return n
end

-- Pools an agent may list, by the lower-case name it uses.
local POOLS = {
    sequences = "Sequences", groups = "Groups", macros = "Macros", pages = "Pages",
    timecodes = "Timecodes", plugins = "Plugins", worlds = "Worlds", filters = "Filters",
    matricks = "MAtricks", layouts = "Layouts", presets = "PresetPools",
}

local RPC_TOOLS = {}

function RPC_TOOLS.hello()
    return { protocol = RPC_PROTOCOL, version = try(Version) }
end

function RPC_TOOLS.status()
    local page = try(CurrentExecPage)
    local masters = {}
    for _, name in ipairs(MASTERS) do
        masters[name] = try(masterEnabled, name)
    end
    return {
        protocol = RPC_PROTOCOL,
        version = try(Version),
        hostOS = try(HostOS),
        page = prop(page, "index"),
        commandLine = cmdlineText(try(CmdObj)),
        deskLocked = try(DeskLocked) and true or false,
        selectedFixtures = try(SelectionCount),
        mastersEnabled = masters,
        oscLine = oscEntry,
    }
end

function RPC_TOOLS.executors(args)
    local pageNo = intArg(args, "page", nil, 1, 9999)
    local pageObj
    if pageNo ~= nil then
        for _, p in pairs(children(DataPool().Pages)) do
            if prop(p, "index") == pageNo then
                pageObj = p
            end
        end
    else
        pageObj = CurrentExecPage()
    end
    if pageObj == nil then
        error("page " .. tostring(pageNo) .. " does not exist", 0)
    end
    local list = {}
    for _, exec in pairs(children(pageObj)) do
        local e = { no = prop(exec, "No", "no"), key = prop(exec, "KEY"), fix = isFixed(exec) }
        local okF, fader = pcall(function() return exec:GetFader({ token = "FaderMaster", faderDisabled = false }) end)
        if okF and type(fader) == "number" then
            e.fader = fader
        end
        local okO, obj = pcall(function() return exec.Object end)
        if okO and obj ~= nil then
            e.object = { class = className(obj), no = prop(obj, "No", "no"), name = prop(obj, "NAME", "Name"),
                cue = prop(obj, "CUENAME") }
            local okR, running = pcall(isRunning, obj)
            if okR then
                e.running = running
            end
        end
        if e.no ~= nil then
            list[#list + 1] = e
        end
    end
    table.sort(list, function(a, b) return (tonumber(a.no) or 0) < (tonumber(b.no) or 0) end)
    return { page = prop(pageObj, "index", "No"), name = prop(pageObj, "NAME", "Name"), executors = list }
end

function RPC_TOOLS.pool(args)
    local poolName = string.lower(tostring(args.pool or ""))
    local key = POOLS[poolName]
    if key == nil then
        local names = {}
        for k in pairs(POOLS) do
            names[#names + 1] = k
        end
        table.sort(names)
        error("unknown pool '" .. poolName .. "'; use one of: " .. table.concat(names, ", "), 0)
    end
    local pool
    if key == "PresetPools" then
        local presetPool = intArg(args, "presetPool", nil, 1, 99)
        if presetPool == nil then
            error("presets need presetPool (1 Dimmer, 2 Position, 3 Gobo, 4 Color, 5 Beam, ...)", 0)
        end
        pool = DataPool().PresetPools[presetPool]
    else
        pool = DataPool()[key]
    end
    if pool == nil then
        error("pool " .. poolName .. " is not available", 0)
    end
    local offset = intArg(args, "offset", 0, 0, 100000)
    local limit = intArg(args, "limit", 200, 1, 500)
    local all = {}
    for _, obj in ipairs(children(pool)) do
        all[#all + 1] = { no = prop(obj, "No", "no", "index"), name = prop(obj, "NAME", "Name"), class = className(obj) }
    end
    table.sort(all, function(a, b) return (tonumber(a.no) or 0) < (tonumber(b.no) or 0) end)
    local items = {}
    for i = offset + 1, math.min(#all, offset + limit) do
        items[#items + 1] = all[i]
    end
    return { pool = poolName, total = #all, offset = offset, items = items, more = offset + limit < #all }
end

-- Name of a referenced object (fixture type, DMX mode, ...), nil if none.
local function refName(obj, ...)
    for _, name in ipairs({ ... }) do
        local ok, v = pcall(function() return obj[name] end)
        if ok and v ~= nil then
            if type(v) == "string" or type(v) == "number" then
                return v
            end
            return prop(v, "NAME", "Name")
        end
    end
    return nil
end

local function childNamed(obj, name)
    local ok, v = pcall(function() return obj[name] end)
    if ok and v ~= nil and type(v) ~= "string" and type(v) ~= "number" then
        return v
    end
    for _, c in pairs(children(obj)) do
        if prop(c, "NAME", "Name") == name then
            return c
        end
    end
    return nil
end

-- MA shows an empty ID, layer, address, ... as "None" (or ""): treat it as missing.
local function present(v)
    if v == nil or v == "" or v == "None" then
        return nil
    end
    return v
end

-- FID and CID as text: whole numbers ("101") and sub-fixture IDs ("101.1") alike.
local function idText(v)
    v = present(v)
    if type(v) == "number" and v == math.floor(v) then
        return string.format("%d", v)
    end
    return v ~= nil and tostring(v) or nil
end

local function fixtureInfo(entry)
    local fixture = entry.fixture
    local subs = #children(fixture)
    return {
        fid = idText(prop(fixture, "FID", "fid")),
        cid = idText(prop(fixture, "CID", "cid")),
        idType = present(refName(fixture, "IDTYPE", "IdType", "idtype")),
        name = present(prop(fixture, "NAME", "Name")),
        fixtureType = present(refName(fixture, "FIXTURETYPE", "FixtureType")),
        mode = present(refName(fixture, "MODE", "Mode")),
        patch = present(prop(fixture, "PATCH", "Patch")),
        layer = present(refName(fixture, "LAYER", "Layer")),
        class = present(refName(fixture, "CLASS", "Class")),
        stage = entry.stage,
        depth = entry.depth,
        parent = entry.parent,
        subfixtures = subs > 0 and subs or nil,
    }
end

-- A fixture and everything below it, depth first (grouping fixtures, sub-fixtures).
local FIXTURE_DEPTH = 8
local function walkFixtures(fixture, stage, parent, depth, out)
    out[#out + 1] = { fixture = fixture, stage = stage, parent = parent, depth = depth }
    if depth >= FIXTURE_DEPTH then
        return
    end
    local me = idText(prop(fixture, "FID", "fid")) or present(prop(fixture, "NAME", "Name"))
    for _, child in ipairs(children(fixture)) do
        walkFixtures(child, stage, me, depth + 1, out)
    end
end

-- Every fixture of every stage in the patch, depth first in patch order.
function RPC_TOOLS.fixtures(args)
    local offset = intArg(args, "offset", 0, 0, 100000)
    local limit = intArg(args, "limit", 100, 1, 300)
    local patch = try(Patch)
    if patch == nil then
        error("cannot read the patch (Patch() is not available)", 0)
    end
    local stages = childNamed(patch, "Stages")
    if stages == nil then
        error("cannot find the stages in the patch", 0)
    end
    local all = {}
    for _, stage in ipairs(children(stages)) do
        local fixtures = childNamed(stage, "Fixtures")
        if fixtures ~= nil then
            local stageName = present(prop(stage, "NAME", "Name"))
            for _, fixture in ipairs(children(fixtures)) do
                walkFixtures(fixture, stageName, nil, 0, all)
            end
        end
    end
    local items = {}
    for i = offset + 1, math.min(#all, offset + limit) do
        items[#items + 1] = fixtureInfo(all[i])
    end
    return { total = #all, offset = offset, items = items, more = offset + limit < #all }
end

local function parseRequest(text)
    local tool, args = nil, {}
    for line in string.gmatch(text, "[^\n]+") do
        if tool == nil then
            tool = line
        else
            local k, v = string.match(line, "^([%w_]+)=(.*)$")
            if k ~= nil then
                args[k] = v
            end
        end
    end
    return tool, args
end

local function rpcReply(id, reply)
    local data = b64encode(toJson(reply))
    local count = math.max(1, math.ceil(#data / RPC_CHUNK))
    if count > RPC_MAX_CHUNKS then
        data = b64encode(toJson({ ok = false, error = "reply too large; ask for fewer items (limit/offset)" }))
        count = 1
    end
    for i = 1, count do
        local chunk = string.sub(data, (i - 1) * RPC_CHUNK + 1, i * RPC_CHUNK)
        send("/ma3osc/rpc", "siis", id .. "," .. i .. "," .. count .. "," .. chunk)
    end
end

-- Runs a waiting request, if any. Errors become an error reply.
local function handleRpc()
    local request = GetVar(GlobalVars(), RPC_VAR)
    if request == nil or request == "" then
        return
    end
    SetVar(GlobalVars(), RPC_VAR, "")
    local id, body = string.match(tostring(request), "^(%w+):([%w%+/=]*)$")
    if id == nil then
        Printf("MA3-OSC plugin: ignored a malformed request in " .. RPC_VAR)
        return
    end
    local ok, result = pcall(function()
        local tool, args = parseRequest(b64decode(body))
        local f = RPC_TOOLS[tool or ""]
        if f == nil then
            error("unknown request '" .. tostring(tool) .. "'", 0)
        end
        return f(args)
    end)
    if ok then
        rpcReply(id, { ok = true, result = result })
    else
        rpcReply(id, { ok = false, error = tostring(result) })
    end
end

local function settings()
    return {
        resendButtons = GetVar(GlobalVars(), "automaticResendButtons") or false,
        colors = GetVar(GlobalVars(), "sendColors") or false,
        names = GetVar(GlobalVars(), "sendNames") or false,
        timecode = GetVar(GlobalVars(), "sendTimecode") or false,
        fixedPage = tonumber(GetVar(GlobalVars(), "fixedPageNr") or 0) or 0,
    }
end

local function main()
    -- Running the plugin while it is already running stops it. "Running" is
    -- detected from a timestamp the loop refreshes every second, so a stale
    -- flag saved in the show file can never block a fresh start.
    local last = GetVar(GlobalVars(), "ma3OscAlive")
    if last ~= nil and math.abs(os.time() - tonumber(last)) <= 2 then
        SetVar(GlobalVars(), "ma3OscStop", true)
        Printf("MA3-OSC plugin: stopping")
        return
    end
    SetVar(GlobalVars(), "ma3OscStop", false)
    SetVar(GlobalVars(), "ma3OscAlive", os.time())
    SetVar(GlobalVars(), TARGET_VAR, "")
    SetVar(GlobalVars(), RPC_VAR, "")
    oscEntry = tonumber(GetVar(GlobalVars(), "ma3OscEntry") or DEFAULT_OSC_ENTRY) or DEFAULT_OSC_ENTRY
    sendFailed = false

    local cfg = settings()
    Printf("MA3-OSC plugin: started (OSC line " .. oscEntry .. ", colors " .. tostring(cfg.colors) .. ", names " .. tostring(cfg.names) ..
        ", timecode " .. tostring(cfg.timecode) .. ", fixed page " .. cfg.fixedPage .. ")")

    local page = nil
    local fixed = nil
    local lastFixedReport = nil
    local forceReload = true
    local tickCount = 0
    local lastError = nil

    local function poll()
        tickCount = tickCount + 1
        if tickCount % HEARTBEAT_TICKS == 0 then
            send("/pam/heartbeat", "i", 1)
            SetVar(GlobalVars(), "ma3OscAlive", os.time())
        end

        if GetVar(GlobalVars(), "forceReload") == true then
            SetVar(GlobalVars(), "forceReload", false)
            cfg = settings()
            forceReload = true
        end
        local resendButtons = cfg.resendButtons and (tickCount % RESEND_TICKS == 0)

        local locked = DeskLocked() and true or false
        if locked ~= oldDeskLocked or forceReload then
            oldDeskLocked = locked
            send("/status/deskLocked", locked and "T" or "F", "")
        end

        for _, name in ipairs(MASTERS) do
            local v = masterEnabled(name)
            if v ~= oldMaster[name] or forceReload then
                oldMaster[name] = v
                send("/masterEnabled/" .. name, "i", v and 1 or 0)
            end
        end

        handleCmdline(forceReload)
        handleRpc()

        local pageObj = CurrentExecPage()
        if cfg.fixedPage ~= 0 and DataPool().Pages[cfg.fixedPage] then
            pageObj = DataPool().Pages[cfg.fixedPage]
        end
        if pageObj == nil then
            return
        end
        if pageObj.index ~= page or forceReload then
            if pageObj.index ~= page then
                forceReload = true
            end
            page = pageObj.index
            send("/updatePage/current", "i", page)
        end

        -- Fixed executors of other pages take the slot on this page. Scanning
        -- all pages costs a little, so it is done once per second.
        if forceReload or fixed == nil or tickCount % HEARTBEAT_TICKS == 0 then
            fixed = fixedExecutors()
        end

        local byNo, pageOf = {}, {}
        for _, exec in pairs(pageObj:Children()) do
            byNo[exec.No] = exec
        end
        -- A fixed executor covers this page's own executor with the same
        -- number, even when MA reports that covered executor as fixed too.
        local covered = {}
        for no, f in pairs(fixed) do
            if f.page ~= page then
                if byNo[no] ~= nil then
                    covered[#covered + 1] = no
                end
                byNo[no] = f.exec
                pageOf[no] = f.page
            end
        end

        -- Report once what Fix resolves to, so it can be checked in the System Monitor.
        local nos = {}
        for no, f in pairs(fixed) do
            nos[#nos + 1] = no
        end
        table.sort(nos)
        local parts = {}
        for _, no in ipairs(nos) do
            parts[#parts + 1] = no .. "@page " .. fixed[no].page
        end
        table.sort(covered)
        local report = "fixed executors: " .. (#parts > 0 and table.concat(parts, ", ") or "none") ..
            (#covered > 0 and ("; covering page " .. page .. "'s own " .. table.concat(covered, ", ")) or "")
        if report ~= lastFixedReport then
            lastFixedReport = report
            Printf("MA3-OSC plugin: " .. report)
        end

        for _, no in ipairs(executorsToWatch) do
            local fader, button, color, name, isFlash = 0, false, "0;0;0;0", ";", false
            local exec = byNo[no]
            -- The address carries the executor's own page, so MA3-OSC sends
            -- fader moves to the fixed executor instead of this page's slot.
            local execPage = pageOf[no] or page
            local moved = oldPage[no] ~= execPage
            oldPage[no] = execPage
            local reload = forceReload or moved
            if exec ~= nil then
                fader = exec:GetFader({ token = "FaderMaster", faderDisabled = false }) or 0
                isFlash = exec.KEY == "Flash"
                local obj = exec.Object
                if obj ~= nil then
                    button = isRunning(obj)
                    if cfg.colors then
                        color = appearanceColor(obj)
                    end
                    if cfg.names then
                        name = displayName(obj)
                    end
                end
            end
            local prefix = "/Page" .. execPage

            if reload or (oldFader[no] ~= fader and not (isFlash and button and fader == 100)) then
                oldFader[no] = fader
                send(prefix .. "/Fader" .. no, "i", math.floor(fader * 1.27 + 0.5))
            end
            if reload or resendButtons or oldButton[no] ~= button then
                oldButton[no] = button
                send(prefix .. "/Button" .. no, "s", button and "On" or "Off")
            end
            if cfg.colors and (reload or oldColor[no] ~= color) then
                oldColor[no] = color
                send(prefix .. "/Color" .. no, "s", color)
            end
            if cfg.names and (reload or oldName[no] ~= name) then
                oldName[no] = name
                send(prefix .. "/Name" .. no, "s", name)
            end
        end

        if cfg.timecode then
            for _, slot in pairs(Root().TimecodeSlots:Children()) do
                local time = slot.timestring
                if forceReload or oldTimecodes[slot.no] ~= time then
                    oldTimecodes[slot.no] = time
                    send("/Timecode" .. slot.no, "s", time)
                end
            end
        end

        forceReload = false
    end

    while GetVar(GlobalVars(), "ma3OscStop") ~= true do
        local ok, err = pcall(poll)
        if not ok then
            if tostring(err) ~= lastError then
                Printf("MA3-OSC plugin error (continuing): " .. tostring(err))
                lastError = tostring(err)
            end
            forceReload = true
        end
        coroutine.yield(TICK)
    end
    SetVar(GlobalVars(), "ma3OscAlive", 0)
    Printf("MA3-OSC plugin: stopped")
end

return main
