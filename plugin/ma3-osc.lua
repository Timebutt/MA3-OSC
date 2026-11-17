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

local function send(addr, typ, value)
    local result = Cmd('SendOSC ' .. oscEntry .. ' "' .. addr .. ',' .. typ .. ',' .. tostring(value) .. '"')
    -- Cmd returns "Ok" on success (older versions may return nothing)
    local failed = result ~= nil and result ~= "Ok" and result ~= true
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
