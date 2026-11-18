//! Runs plugin/ma3-osc.lua in Lua 5.4 against a small mock of the grandMA3
//! API and checks the OSC commands it issues.

use mlua::{Function, Lua};

const PLUGIN: &str = include_str!("../plugin/ma3-osc.lua");

const MOCK: &str = r#"
sent, printed, vars = {}, {}, {}
function GlobalVars() return vars end
function GetVar(t, k) return t[k] end
function SetVar(t, k, v) t[k] = v end
cmdResult = "Ok"
function Cmd(s) sent[#sent + 1] = s; return cmdResult end
function Printf(s) printed[#printed + 1] = s end
function DeskLocked() return false end
local grand = { highlight = { FADERENABLED = false }, lowlight = { FADERENABLED = true },
                solo = { FADERENABLED = false }, blind = { FADERENABLED = false } }
function MasterPool() return { Grand = grand } end
seq = { NAME = 'Front "wash", A;B', CUENAME = 'Cue 1',
        APPEARANCE = { BACKR = 255, BACKG = 0, BACKB = 0, BACKALPHA = 255 },
        preferCueAppearance = false,
        HasActivePlayback = function() oldApiCalls = oldApiCalls + 1; return true end,
        IsRunningPlayback = function() return true end,
        CurrentChild = function() return nil end }
faderValue = 50
oldApiCalls = 0
exec201 = { No = 201, KEY = 'Temp', Object = seq, GetFader = function(self, o) return faderValue end }
page1 = { index = 1, Children = function() return { exec201 } end }
function DataPool() return { Pages = { [1] = page1 } } end
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
    lua.load(r#"cmdResult = "Illegal Command"; step(5)"#).exec().unwrap();
    let printed: Vec<String> = lua.load("return printed").eval().unwrap();
    let hints = printed.iter().filter(|p| p.contains("SendOSC on OSC line 2 failed")).count();
    assert_eq!(hints, 1, "{printed:?}");
    lua.load(r#"cmdResult = "Ok"; faderValue = 100; step(1)"#).exec().unwrap();
    assert!(check(&lua, "printed[#printed] == 'MA3-OSC plugin: SendOSC works again'"));
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
