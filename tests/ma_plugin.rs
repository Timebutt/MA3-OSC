//! Runs plugin/ma3-osc.lua in Lua 5.4 against a small mock of the grandMA3
//! API and checks the OSC commands it issues.

use mlua::{Function, Lua};

const PLUGIN: &str = include_str!("../plugin/ma3-osc.lua");

const MOCK: &str = r#"
sent, printed, vars = {}, {}, {}
function GlobalVars() return vars end
function GetVar(t, k) return t[k] end
function SetVar(t, k, v) t[k] = v end
cmdResult = "OK"  -- what grandMA3 2.5 returns
function Cmd(s) sent[#sent + 1] = s; return cmdResult end
function Printf(s) printed[#printed + 1] = s end
function DeskLocked() return false end
local grand = { highlight = { FADERENABLED = false }, lowlight = { FADERENABLED = true },
                solo = { FADERENABLED = false }, blind = { FADERENABLED = false } }
function MasterPool() return { Grand = grand } end
seq = { No = 3, NAME = 'Front "wash", A;B', CUENAME = 'Cue 1',
        GetClass = function() return "Sequence" end,
        APPEARANCE = { BACKR = 255, BACKG = 0, BACKB = 0, BACKALPHA = 255 },
        preferCueAppearance = false,
        HasActivePlayback = function() oldApiCalls = oldApiCalls + 1; return true end,
        IsRunningPlayback = function() return true end,
        CurrentChild = function() return nil end }
faderValue = 50
oldApiCalls = 0
exec201 = { No = 201, KEY = 'Temp', Object = seq, GetFader = function(self, o) return faderValue end }
page1 = { index = 1, NAME = "Main", Children = function() return { exec201 } end }
groups = {}
dp = { Pages = { [1] = page1 }, Groups = groups }
function DataPool() return dp end
function Version() return "2.5.1.1" end
failNext = false
function CurrentExecPage()
    if failNext then failNext = false; error('boom') end
    return page1
end
function Root() return { TimecodeSlots = { Children = function() return {} end } } end
function has(s) for _, v in ipairs(sent) do if v == s then return true end end return false end
function step(n) for i = 1, n do assert(coroutine.resume(co)) end end
"#;

fn setup() -> Lua {
    let lua = Lua::new();
    lua.load(MOCK).exec().unwrap();
    let main: Function = lua.load(PLUGIN).set_name("ma3-osc.lua").eval().unwrap();
    lua.globals().set("plugin_main", main).unwrap();
    lua.load("vars.sendNames = true; vars.sendColors = true; co = coroutine.create(plugin_main)").exec().unwrap();
    lua
}

fn check(lua: &Lua, expr: &str) -> bool {
    lua.load(format!("return {expr}")).eval().unwrap()
}

fn sent(lua: &Lua) -> Vec<String> {
    lua.load("return sent").eval().unwrap()
}

#[test]
fn sends_clean_osc_and_heartbeat() {
    let lua = setup();
    lua.load("step(10)").exec().unwrap();
    for s in sent(&lua) {
        assert!(s.starts_with("SendOSC 2 \"") && s.ends_with('"') && s.matches('"').count() == 2, "{s}");
    }
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Fader201,i,64"')"#), "{:?}", sent(&lua));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Button201,s,On"')"#));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Color201,s,255;0;0;255"')"#));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Name201,s,Front \'wash\'  A:B;Cue 1"')"#), "{:?}", sent(&lua));
    assert!(check(&lua, r#"has('SendOSC 2 "/masterEnabled/lowlight,i,1"')"#));
    assert!(check(&lua, r#"has('SendOSC 2 "/status/deskLocked,F,"')"#));
    assert!(check(&lua, r#"has('SendOSC 2 "/pam/heartbeat,i,1"')"#));
}

#[test]
fn uses_new_playback_api_and_falls_back() {
    let lua = setup();
    lua.load("step(5)").exec().unwrap();
    assert!(check(&lua, "oldApiCalls == 0"));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Button201,s,On"')"#));

    // older MA3 without IsRunningPlayback
    let lua = setup();
    lua.load("seq.IsRunningPlayback = nil; step(5)").exec().unwrap();
    assert!(check(&lua, "oldApiCalls > 0"));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Button201,s,On"')"#));
}

#[test]
fn send_failure_is_explained_once() {
    let lua = setup();
    // MA's own error text, e.g. for a missing OSC line
    lua.load(r#"cmdResult = "Illegal object"; step(5)"#).exec().unwrap();
    let printed: Vec<String> = lua.load("return printed").eval().unwrap();
    let hints = printed.iter().filter(|p| p.contains("SendOSC on OSC line 2 failed")).count();
    assert_eq!(hints, 1, "{printed:?}");
    lua.load(r#"cmdResult = "OK"; faderValue = 100; step(1)"#).exec().unwrap();
    assert!(check(&lua, "printed[#printed] == 'MA3-OSC plugin: SendOSC works again'"));
    // success is not reported as a failure, whatever case MA uses
    lua.load(r#"printed = {}; cmdResult = "Ok"; faderValue = 50; step(1)"#).exec().unwrap();
    let printed: Vec<String> = lua.load("return printed").eval().unwrap();
    assert!(printed.is_empty(), "{printed:?}");
}

#[test]
fn osc_line_is_configurable() {
    let lua = setup();
    lua.load("vars.ma3OscEntry = 3; step(3)").exec().unwrap();
    assert!(check(&lua, r#"has('SendOSC 3 "/Page1/Fader201,i,64"')"#), "{:?}", sent(&lua));
}

#[test]
fn only_changes_are_sent() {
    let lua = setup();
    lua.load("step(2); sent = {}; step(1)").exec().unwrap();
    assert!(sent(&lua).is_empty(), "{:?}", sent(&lua));
    lua.load("faderValue = 100; step(1)").exec().unwrap();
    assert_eq!(sent(&lua), vec![r#"SendOSC 2 "/Page1/Fader201,i,127""#]);
}

#[test]
fn errors_do_not_stop_the_loop() {
    let lua = setup();
    lua.load("step(2); sent = {}; failNext = true; step(2)").exec().unwrap();
    assert!(check(&lua, "coroutine.status(co) == 'suspended'"));
    assert!(check(&lua, "printed[#printed]:find('boom') ~= nil"));
    // after an error everything is re-sent
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Fader201,i,64"')"#));
}

#[test]
fn second_run_stops_and_stale_flag_does_not_block() {
    let lua = setup();
    lua.load("step(1); plugin_main(); step(1)").exec().unwrap();
    assert!(check(&lua, "coroutine.status(co) == 'dead'"));

    // a show file saved while running holds an old timestamp: must start normally
    lua.load("vars.ma3OscStop = false; vars.ma3OscAlive = os.time() - 3600; co = coroutine.create(plugin_main); step(3)")
        .exec()
        .unwrap();
    assert!(check(&lua, "coroutine.status(co) == 'suspended'"));
}

#[test]
fn fixed_executor_of_another_page_is_reported_under_its_page() {
    let lua = setup();
    // 201 is set to Fix on page 1; page 2 has its own (unfixed) 201 and a 202
    lua.load(
        r#"
        exec201.FIX = true
        other201 = { No = 201, KEY = 'Temp', Object = nil, GetFader = function() return 0 end }
        exec202 = { No = 202, KEY = 'Temp', Object = nil, GetFader = function() return 100 end }
        page2 = { index = 2, Children = function() return { other201, exec202 } end }
        function DataPool() return { Pages = { [1] = page1, [2] = page2 } } end
        current = page2
        function CurrentExecPage() return current end
        step(2)
    "#,
    )
    .exec()
    .unwrap();
    assert!(check(&lua, r#"has('SendOSC 2 "/updatePage/current,i,2"')"#));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page1/Fader201,i,64"')"#), "{:?}", sent(&lua));
    assert!(check(&lua, r#"has('SendOSC 2 "/Page2/Fader202,i,127"')"#));
    assert!(!check(&lua, r#"has('SendOSC 2 "/Page2/Fader201,i,0"')"#));

    // the covered executor on page 2 may itself report Fix: page 1's still wins,
    // and moving page 1's fader in MA is fed back
    lua.load("other201.FIX = true; sent = {}; step(10); sent = {}; faderValue = 100; step(1)").exec().unwrap();
    assert_eq!(sent(&lua), vec![r#"SendOSC 2 "/Page1/Fader201,i,127""#]);
    assert!(check(&lua, "printed[#printed]:find('201@page 1') ~= nil"), "{:?}", lua.load("return printed").eval::<Vec<String>>().unwrap());

    // un-fixing it gives the slot back to page 2 within a second
    lua.load("other201.FIX = false").exec().unwrap();
    lua.load("exec201.FIX = false; sent = {}; step(10)").exec().unwrap();
    assert!(check(&lua, r#"has('SendOSC 2 "/Page2/Fader201,i,0"')"#), "{:?}", sent(&lua));
}

#[test]
fn reports_command_line_and_completes_it_with_the_pressed_executor() {
    let lua = setup();
    lua.load(
        r#"
        cmdline = { CMDTEXT = "" }
        function cmdline:ClearCmd() self.CMDTEXT = "" end
        function CmdObj() return cmdline end
        step(1)
    "#,
    )
    .exec()
    .unwrap();
    assert!(check(&lua, r#"has('SendOSC 2 "/cmdline,s,"')"#), "{:?}", sent(&lua));

    lua.load(r#"cmdline.CMDTEXT = "Assign Sequence 5"; sent = {}; step(1)"#).exec().unwrap();
    assert_eq!(sent(&lua), vec![r#"SendOSC 2 "/cmdline,s,Assign Sequence 5""#]);

    // MA3-OSC reports the pressed executor
    lua.load(r#"vars.ma3OscTarget = "Page 1.201"; sent = {}; step(1)"#).exec().unwrap();
    let s = sent(&lua);
    assert_eq!(s[0], "Assign Sequence 5 At Page 1.201", "{s:?}");
    assert!(s.contains(&r#"SendOSC 2 "/cmdline,s,""#.to_string()), "{s:?}");
    assert!(check(&lua, r#"cmdline.CMDTEXT == "" and vars.ma3OscTarget == """#));
    assert!(check(&lua, "printed[#printed]:find('ClearCmd') ~= nil"));

    // keyword-only commands take the executor directly
    lua.load(r#"cmdline.CMDTEXT = "Store /Merge"; step(1); vars.ma3OscTarget = "Page 2.305"; sent = {}; step(1)"#)
        .exec()
        .unwrap();
    assert_eq!(sent(&lua)[0], "Store /Merge Page 2.305");
    lua.load(r#"cmdline.CMDTEXT = "As Seq 7 At "; step(1); vars.ma3OscTarget = "Page 1.101"; sent = {}; step(1)"#)
        .exec()
        .unwrap();
    assert_eq!(sent(&lua)[0], "As Seq 7 At Page 1.101");
}

#[test]
fn unclearable_command_line_is_explained_once() {
    let lua = setup();
    lua.load(
        r#"
        cmdline = { CMDTEXT = "Store" }
        function CmdObj() return cmdline end
        step(1)
        vars.ma3OscTarget = "Page 1.201"; step(1)
        cmdline.CMDTEXT = "Store"; vars.ma3OscTarget = "Page 1.202"; step(1)
    "#,
    )
    .exec()
    .unwrap();
    assert!(check(&lua, r#"has("Store Page 1.201") and has("Store Page 1.202")"#), "{:?}", sent(&lua));
    let printed: Vec<String> = lua.load("return printed").eval().unwrap();
    assert_eq!(printed.iter().filter(|p| p.contains("could not clear")).count(), 1, "{printed:?}");
}

fn b64decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("bad base64"),
    } as u32;
    let mut out = Vec::new();
    for chunk in s.as_bytes().chunks(4) {
        let n = chunk.iter().take_while(|c| **c != b'=').count();
        let mut acc = 0u32;
        for (i, c) in chunk.iter().take(n).enumerate() {
            acc |= val(*c) << (18 - 6 * i);
        }
        out.extend_from_slice(&acc.to_be_bytes()[1..n]);
    }
    out
}

#[test]
fn xml_is_up_to_date() {
    let xml = include_str!("../plugin/ma3-osc.xml");
    let start = xml.find("MA3-OSC Start Stop").unwrap();
    let end = xml.find("MA3-OSC Settings").unwrap();
    let embedded: Vec<u8> = xml[start..end]
        .split("Base64=\"")
        .skip(1)
        .flat_map(|b| b64decode(&b[..b.find('"').unwrap()]))
        .collect();
    assert!(
        embedded == PLUGIN.as_bytes(),
        "plugin/ma3-osc.xml is stale: run `python3 plugin/make_xml.py`"
    );
}

fn b64encode(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Sets the request variable as MA3-OSC does, runs one poll and returns the
/// decoded reply and the number of chunks it came in.
fn rpc(lua: &Lua, id: &str, request: &str) -> (serde_json::Value, usize) {
    lua.load(format!("sent = {{}}; vars.ma3OscRpc = '{id}:{}'; step(1)", b64encode(request.as_bytes())))
        .exec()
        .unwrap();
    assert!(check(lua, "vars.ma3OscRpc == ''"), "request not consumed");
    let prefix = format!("SendOSC 2 \"/ma3osc/rpc,siis,{id},");
    let mut parts = Vec::new();
    for s in sent(lua).iter().filter_map(|s| s.strip_prefix(&prefix)) {
        let s = s.strip_suffix('"').unwrap();
        let mut f = s.splitn(3, ',');
        let (i, n, chunk) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
        assert!(s.len() < 1800, "chunk too long for one OSC packet");
        parts.push((i.parse::<usize>().unwrap(), n.parse::<usize>().unwrap(), chunk.to_string()));
    }
    assert!(!parts.is_empty(), "no reply: {:?}", sent(lua));
    let n = parts[0].1;
    assert!(parts.iter().enumerate().all(|(k, p)| p.0 == k + 1 && p.1 == n) && parts.len() == n, "{parts:?}");
    let data: String = parts.iter().map(|p| p.2.as_str()).collect();
    (serde_json::from_slice(&b64decode(&data)).unwrap(), n)
}

#[test]
fn rpc_hello_status_and_executors() {
    let lua = setup();
    lua.load("step(1)").exec().unwrap();
    let (r, _) = rpc(&lua, "r1", "hello");
    assert_eq!(r, serde_json::json!({ "ok": true, "result": { "protocol": 1, "version": "2.5.1.1" } }));

    let (r, _) = rpc(&lua, "r2", "status");
    assert_eq!(r["result"]["page"], 1);
    assert_eq!(r["result"]["deskLocked"], false);
    assert_eq!(r["result"]["mastersEnabled"]["lowlight"], true);

    let (r, _) = rpc(&lua, "r3", "executors\npage=1");
    let e = &r["result"]["executors"][0];
    assert_eq!(r["result"]["name"], "Main");
    assert_eq!(e["no"], 201);
    assert_eq!(e["object"]["class"], "Sequence");
    assert_eq!(e["object"]["name"], "Front \"wash\", A;B");
    assert_eq!(e["running"], true);

    let (r, _) = rpc(&lua, "r4", "executors\npage=7");
    assert_eq!(r, serde_json::json!({ "ok": false, "error": "page 7 does not exist" }));
}

#[test]
fn rpc_pool_is_paged_and_chunked() {
    let lua = setup();
    lua.load(r#"for i = 1, 400 do groups[i] = { No = i, NAME = "Group with a long name " .. i } end; step(1)"#)
        .exec()
        .unwrap();
    let (r, chunks) = rpc(&lua, "r1", "pool\npool=groups\noffset=10\nlimit=300");
    assert!(chunks > 3, "{chunks}");
    let res = &r["result"];
    assert_eq!((res["total"].clone(), res["offset"].clone(), res["more"].clone()), (400.into(), 10.into(), true.into()));
    assert_eq!(res["items"].as_array().unwrap().len(), 300);
    assert_eq!(res["items"][0]["name"], "Group with a long name 11");

    let (r, _) = rpc(&lua, "r2", "pool\npool=presets");
    assert!(r["error"].as_str().unwrap().contains("presetPool"), "{r}");
    let (r, _) = rpc(&lua, "r3", "pool\npool=groups\nlimit=9999");
    assert_eq!(r["error"], "limit must be a whole number from 1 to 500");
    let (r, _) = rpc(&lua, "r4", "store\nwhat=everything");
    assert_eq!(r["error"], "unknown request 'store'");
}

#[test]
fn rpc_malformed_request_is_dropped_and_loop_keeps_running() {
    let lua = setup();
    lua.load(r#"step(1); sent = {}; vars.ma3OscRpc = 'r1:not base64!'; step(1)"#).exec().unwrap();
    assert!(check(&lua, "vars.ma3OscRpc == ''"));
    assert!(!sent(&lua).iter().any(|s| s.contains("/ma3osc/rpc")));
    assert!(check(&lua, "printed[#printed]:find('malformed') ~= nil"));
    assert!(check(&lua, "coroutine.status(co) == 'suspended'"));
    let (r, _) = rpc(&lua, "r2", "hello");
    assert_eq!(r["ok"], true);
}

#[test]
fn rpc_fixtures_walks_the_whole_patch() {
    let lua = setup();
    lua.load(r#"
        local function fixture(t)
            local subs = t.subs or {}
            t.subs = nil
            t.Children = function() return subs end
            return t
        end
        local spot = { NAME = "Spot" }
        local grouping = { NAME = "Grouping" }
        -- like a real show: MA reports empty IDs, layers, ... as "None"
        local univ = fixture { FID = "None", CID = "None", NAME = "Univ", FIXTURETYPE = { NAME = "Universal" } }
        local front = fixture { FID = 100, CID = "None", NAME = "Front", FIXTURETYPE = grouping, PATCH = "",
            LAYER = "None", CLASS = "None", subs = {
                fixture { FID = 101, CID = "None", NAME = "Spot 1", FIXTURETYPE = spot, MODE = { NAME = "Mode 1" },
                    PATCH = "1.001", LAYER = { NAME = "Front" }, CLASS = "Spots",
                    subs = { fixture { FID = "101.1", NAME = "Cell 1" } } },
                fixture { FID = 102, NAME = "Spot 2", FIXTURETYPE = spot, PATCH = "1.021", LAYER = "Front" },
            } }
        function Patch() return { Stages = { { NAME = "Stage 1", Fixtures = { univ, front } } } } end
        step(1)
    "#)
    .exec()
    .unwrap();
    let (r, _) = rpc(&lua, "r1", "fixtures");
    let items = r["result"]["items"].as_array().unwrap();
    assert_eq!(r["result"]["total"], 5, "{r}");
    let names: Vec<_> = items.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Univ", "Front", "Spot 1", "Cell 1", "Spot 2"]);

    // "None" and "" are left out, never sent as text
    for f in items {
        for (k, v) in f.as_object().unwrap() {
            assert!(v != "None" && v != "", "{k} in {f}");
        }
    }
    assert!(items[0].get("fid").is_none() && items[0].get("cid").is_none(), "{}", items[0]);
    assert!(items[1].get("layer").is_none() && items[1].get("patch").is_none(), "{}", items[1]);
    assert_eq!(items[1]["subfixtures"], 2);

    // the real fixture under the grouping fixture, with IDs always as text
    let spot1 = &items[2];
    assert_eq!(spot1["fid"], "101");
    assert_eq!(spot1["parent"], "100");
    assert_eq!(spot1["depth"], 1);
    assert_eq!(spot1["fixtureType"], "Spot");
    assert_eq!(spot1["mode"], "Mode 1");
    assert_eq!(spot1["patch"], "1.001");
    assert_eq!(spot1["layer"], "Front");
    assert_eq!(spot1["class"], "Spots");
    assert_eq!(items[3]["fid"], "101.1");
    assert_eq!((items[3]["parent"].clone(), items[3]["depth"].clone()), ("101".into(), 2.into()));

    let (r, _) = rpc(&lua, "r2", "fixtures\noffset=4\nlimit=1");
    assert_eq!((r["result"]["items"][0]["name"].clone(), r["result"]["more"].clone()), ("Spot 2".into(), false.into()));

    lua.load("Patch = nil").exec().unwrap();
    let (r, _) = rpc(&lua, "r3", "fixtures");
    assert_eq!(r["error"], "cannot read the patch (Patch() is not available)");
}
