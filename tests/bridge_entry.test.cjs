// Exercise the ExtendScript entry against the Bridge DOM contract, with no
// Adobe application or real photo changes. Run: node --test tests/bridge_entry.test.cjs
const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const path = require("node:path");
const source = fs.readFileSync(path.join(__dirname, "../bridge/FastCull.jsx"), "utf8").replace(/^#target.*$/m, "");
function host(mac = false) {
    const menus = {}, files = {}, commands = [], alerts = [];
    const executable = mac ? "/Users/测试 O'Brien/$photos/FastCull.app/Contents/MacOS/fastcull" : "E:\\Photo tools\\FastCull\\fastcull.exe";
    const config = mac ? "/Users/测试/Library/Application Support/Adobe/Bridge 2026/Startup Scripts/FastCull-path.txt" : "C:\\UserData\\Adobe\\Bridge 2026\\Startup Scripts\\FastCull-path.txt";
    files[config] = executable + "\r\n";
    files[executable] = "";
    function File(spec) {
        if (!(this instanceof File)) return new File(spec);
        this.fsName = typeof spec === "object" ? spec.fsName : (mac ? spec : spec.replace(/\//g, "\\"));
        this.parent = {fsName: this.fsName.slice(0, this.fsName.lastIndexOf(mac ? "/" : "\\"))};
        Object.defineProperty(this, "exists", {get: () => Object.hasOwn(files, this.fsName)});
    }
    File.prototype.open = function (mode) { this.mode = mode; return mode === "w" || this.exists; };
    File.prototype.read = function () { return files[this.fsName]; };
    File.prototype.write = function (text) { files[this.fsName] = text; return true; };
    File.prototype.close = function () {};
    function Folder(spec) { this.fsName = typeof spec === "object" ? spec.fsName : (mac ? spec : spec.replace(/\//g, "\\")); this.exists = true; }
    Folder.userData = {fsName: mac ? "/Users/测试/Library/Application Support" : "C:\\UserData"};
    Folder.prototype.create = function () { return true; };
    Folder.prototype.getFiles = function () { throw new Error("must not scan folders in Bridge"); };
    function MenuElement(type, text, location, id) {
        if (menus[id]) throw new Error("duplicate menu");
        Object.assign(this, {type, text, location, id});
        menus[id] = this;
    }
    MenuElement.find = id => menus[id] || null;
    const context = vm.createContext({
        app: {document: {selections: []}, system: command => { commands.push(command); return 0; }},
        BridgeTalk: {appName: "bridge"}, File, Folder, MenuElement,
        $: {os: mac ? "Macintosh" : "Windows", fileName: mac ? config.replace("FastCull-path.txt", "FastCull.jsx") : "C:/UserData/Adobe/Bridge 2026/Startup Scripts/FastCull.jsx"},
        Window: {alert: message => alerts.push(message)},
    });
    function select(...specs) {
        context.app.document.selections = specs.map(spec => {
            files[spec] = "raw";
            const thumb = {spec, container: false};
            Object.defineProperty(thumb, "synchronousMetadata", {get: () => { throw new Error("must not export XMP on launch"); }});
            return thumb;
        });
    }
    vm.runInContext(source, context);
    return {menus, files, commands, alerts, context, select, config, executable, reload: () => vm.runInContext(source, context)};
}
test("registers a Tools entry and photo context entry, idempotently", () => {
    const h = host();
    h.reload();
    assert.equal(Object.keys(h.menus).length, 2);
    assert.equal(h.menus["com.zrtc.fastcull.open"].location, "at the end of Tools");
    assert.equal(h.menus["com.zrtc.fastcull.context"].location, "at the end of Thumbnail");
});
test("unsupported selection disables both entries and cannot launch", () => {
    const h = host();
    h.select("D:\\photo.mp4", "D:\\photo.png");
    for (const menu of Object.values(h.menus)) { menu.onDisplay(); assert.equal(menu.enabled, false); }
    h.context.FastCullBridge.open();
    assert.equal(h.commands.length, 0);
    assert.match(h.alerts[0], /RAW/);
});
test("selected order, mixed formats and case-insensitive duplicates", () => {
    const h = host();
    h.select("D:\\照片 100% & (一).ARW", "D:\\two.arw", "D:\\two.ARW", "D:\\photo.jpg", "D:\\three.CR2", "D:\\four.NEF", "D:\\ignore.CR3", "D:\\ignore.xmp", "D:\\ignore.mp4");
    h.menus["com.zrtc.fastcull.context"].onSelect();
    assert.equal(h.alerts.length, 0);
    const request = Object.keys(h.files).find(name => /bridge-requests.*\.txt$/.test(name));
    assert.equal(h.files[request], "FASTCULL_BRIDGE_V1\nD:\\照片 100% & (一).ARW\nD:\\two.arw\nD:\\photo.jpg\nD:\\three.CR2\nD:\\four.NEF\n");
    assert.equal(h.commands.length, 1);
    assert.equal(h.commands[0], 'cmd.exe /d /v:off /s /c ""' + h.executable + '" --bridge-open "' + request + '""');
    assert.ok(!h.commands[0].includes("100%"));
});
test("large selections use a constant-size command, not thousands of arguments", () => {
    const h = host();
    h.select(...Array.from({length: 5000}, (_, i) => "D:\\photos\\photo-" + i + ".ARW"));
    h.context.FastCullBridge.open();
    assert.equal(h.alerts.length, 0);
    assert.ok(h.commands[0].length < 512);
    const request = Object.keys(h.files).find(name => /bridge-requests.*\.txt$/.test(name));
    assert.equal(h.files[request].trim().split("\n").length, 5001);
});
test("Bridge and Rust agree on each enabled and deferred file extension", () => {
    const h=host();
    for (const ext of ['ARW','cr2','NEF','JPG','jpeg','JPE']) {
        h.select('D:\\photo.'+ext);assert.equal(h.context.FastCullBridge.paths().length,1);
    }
    for (const ext of ['CR3','RAF','DNG','RW2','ORF','png','xmp','mp4']) {
        h.select('D:\\photo.'+ext);assert.equal(h.context.FastCullBridge.paths().length,0);
    }
});
test("multiple folder selection enables menus and passes roots without scanning Bridge", () => {
    const h = host();
    h.context.app.document.selections = [
        {container: true, spec: "D:\\上午"},
        {container: true, spec: "D:\\下午"},
        {container: true, spec: "D:\\上午"},
    ];
    for (const menu of Object.values(h.menus)) { menu.onDisplay(); assert.equal(menu.enabled, true); }
    h.menus["com.zrtc.fastcull.context"].onSelect();
    assert.equal(h.alerts.length, 0);
    const request = Object.keys(h.files).find(name => /bridge-requests.*\.txt$/.test(name));
    assert.equal(h.files[request], "FASTCULL_BRIDGE_V1\nD:\\上午\nD:\\下午\n");
    assert.equal(h.commands.length, 1);
});
test("missing exe and shell-expanding config paths fail without spawning", () => {
    for (const config of ["E:\\missing.exe", "E:\\%EVIL%\\fastcull.exe", "E:\\!VAR!\\fastcull.exe"]) {
        const h = host();
        h.files[h.config] = config;
        if (config.includes("fastcull")) h.files[config] = "";
        h.select("D:\\a.ARW");
        h.context.FastCullBridge.open();
        assert.equal(h.commands.length, 0);
        assert.equal(h.alerts.length, 1);
    }
});
test("native helper failure is reported and retains the request", () => {
    const h = host(); h.select("D:\\a.ARW");
    h.context.app.system = () => 7;
    h.context.FastCullBridge.open();
    assert.match(h.alerts[0], /7/);
    assert.ok(Object.keys(h.files).some(name => /bridge-requests.*\.txt$/.test(name)));
});
test("path protection rejects each unsafe character and preserves valid Unicode", () => {
    const h = host();
    for (const character of ['%', '!', '"', '\r', '\n', '\0']) {
        assert.throws(() => h.context.FastCullBridge.quote("C:/before" + character + "after"), /命令字符/);
    }
    assert.equal(h.context.FastCullBridge.quote("D:/照片 & (一)/fastcull.exe"), '"D:/照片 & (一)/fastcull.exe"');
    // Read the crate version instead of repeating it here, so a release bump
    // cannot leave this contract test behind.
    const version = /^version = "([^"]+)"/m.exec(
        fs.readFileSync(path.join(__dirname, "../Cargo.toml"), "utf8"))[1];
    assert.equal(h.context.FastCullBridge.version, version);
});
test("launch failures retain stage and native ExtendScript error details", () => {
    const h = host(); h.select("D:\\a.ARW");
    h.context.app.system = () => { const error = new Error("native failure"); error.number = 8; error.line = 73; throw error; };
    h.context.FastCullBridge.open();
    assert.match(h.alerts[0], /启动 Rust 选片器/);
    assert.match(h.alerts[0], /错误号：8，行号：73/);
});

test("legacy shell marker cannot route photo requests through File.execute", () => {
    const h = host(); h.select("D:\\照片 100% & (一).NEF");
    h.files[h.config.replace('FastCull-path.txt', 'FastCull-shell.txt')] = 'FastCull.Request';
    h.context.File.prototype.execute = () => { throw new Error('Windows Open With dialog'); };
    h.context.FastCullBridge.open();
    assert.equal(h.alerts.length, 0);
    assert.equal(h.commands.length, 1);
    const request = Object.keys(h.files).find(name => /bridge-requests.*\.txt$/.test(name));
    assert.ok(request);
    assert.match(h.files[request], /照片 100% & \(一\)\.NEF/);
    assert.ok(h.commands[0].includes(' --bridge-open '));
    assert.ok(!h.commands[0].includes('100%'));
});

test("hotkey consumes a fresh request exactly once and discards stale requests", () => {
    const h=host();
    h.context.File.prototype.remove=function(){ delete h.files[this.fsName]; return true; };
    const key='C:\\UserData\\FastCull\\hotkeys-bridge\\request';
    let calls=0;h.context.FastCullBridge.open=()=>calls++;
    h.files[key]=String(Date.now());h.context.FastCullBridge.pollHotkey();
    h.context.FastCullBridge.pollHotkey();assert.equal(calls,1);
    h.files[key]=String(Date.now()-5000);h.context.FastCullBridge.pollHotkey();
    assert.equal(calls,1);assert.ok(!Object.hasOwn(h.files,key));
});

test("reloading Bridge script replaces its scheduled task without duplicate listeners", () => {
    const h=host(),scheduled=[],cancelled=[];
    h.context.File.prototype.remove=function(){delete h.files[this.fsName];return true;};
    h.context.File.prototype.execute=function(){return true;};
    h.files[h.executable.replace('fastcull.exe','fastcull-keys.exe')]='helper';
    h.context.app.scheduleTask=(code,delay,repeat)=>{scheduled.push({code,delay,repeat});return scheduled.length;};
    h.context.app.cancelTask=id=>cancelled.push(id);
    h.reload();h.reload();
    assert.deepEqual(cancelled,[1]);assert.equal(scheduled.length,2);
    assert.deepEqual(scheduled[1],{code:'FastCullBridge.pollHotkey()',delay:250,repeat:true});
});

test("Bridge restarts a lost hotkey listener after its heartbeat expires", () => {
    const h=host(); let now=100000, launches=0;
    h.context.Date=class extends Date { constructor(){super(now);} };
    h.context.File.prototype.remove=function(){delete h.files[this.fsName];return true;};
    h.context.File.prototype.execute=function(){launches++;return true;};
    h.files[h.executable.replace('fastcull.exe','fastcull-keys.exe')]='helper';
    h.context.app.scheduleTask=()=>1;
    h.reload();assert.equal(launches,1);
    const heartbeat='C:\\UserData\\FastCull\\hotkeys-bridge\\helper-alive';
    h.files[heartbeat]=String(now);
    for(let i=0;i<40;i++){now+=250;h.files[heartbeat]=String(now);h.context.FastCullBridge.pollHotkey();}
    assert.equal(launches,1,'healthy listener must not spawn extra processes');
    // Native listener has exited while Bridge was blocked; Bridge resumes.
    now+=35000;
    for(let i=0;i<20;i++){now+=250;h.context.FastCullBridge.pollHotkey();}
    assert.equal(launches,2,'resumed scheduler must restore the shortcut listener');
    for(let i=0;i<8;i++){now+=250;h.context.FastCullBridge.pollHotkey();}
    assert.equal(launches,2,'restart attempts must be rate limited');
});

test("failed initial helper launch keeps polling and retries without opening photos", () => {
    const h=host(); let now=100000, launches=0, scheduled=0;
    h.context.Date=class extends Date {constructor(){super(now);}};
    h.context.File.prototype.remove=function(){delete h.files[this.fsName];return true;};
    h.context.File.prototype.execute=function(){launches++;return launches>1;};
    h.files[h.executable.replace('fastcull.exe','fastcull-keys.exe')]='helper';
    h.context.app.scheduleTask=()=>++scheduled;
    h.reload();assert.equal(scheduled,1);assert.equal(launches,1);
    assert.ok(h.context.FastCullBridge.hotkeyError);
    now+=11000;
    for(let i=0;i<20;i++){h.context.FastCullBridge.pollHotkey();}
    assert.equal(launches,2);assert.equal(h.context.FastCullBridge.hotkeyError,null);
    assert.equal(h.commands.length,0);assert.equal(h.alerts.length,0);
});

test("macOS launches the Mach-O executable with POSIX quoting, retaining case and backslashes", () => {
    const h = host(true);
    h.select("/Volumes/照片/A.ARW", "/Volumes/照片/a.ARW", "/Volumes/照片/a\\b.JPG");
    h.context.FastCullBridge.open();
    assert.equal(h.alerts.length, 0);
    assert.equal(h.commands.length, 1);
    assert.ok(!h.commands[0].includes("cmd.exe"));
    const request = Object.keys(h.files).find(name => /bridge-requests.*\.txt$/.test(name));
    assert.equal(h.files[request], "FASTCULL_BRIDGE_V1\n/Volumes/照片/A.ARW\n/Volumes/照片/a.ARW\n/Volumes/照片/a\\b.JPG\n");
    const quote = s => "'" + s.replace(/'/g, "'\"'\"'") + "'";
    assert.equal(h.commands[0], quote(h.executable) + " --bridge-open " + quote(request));
    assert.ok(!h.menus["com.zrtc.fastcull.open"].text.includes("Ctrl+Alt+K"));
    assert.equal(h.context.FastCullBridge.hotkeyTask, undefined);
});

test("macOS refuses control characters in executable paths before creating a request", () => {
    const h = host(true);
    for (const value of ["a\nb", "a\rb", "a\u0000b"]) {
        assert.throws(() => h.context.FastCullBridge.quote(value));
    }
});

test("macOS does not split a newline filename into multiple Bridge requests", () => {
    const h = host(true);
    h.select('/Photos/bad\nname.ARW');
    h.context.FastCullBridge.open();
    assert.equal(h.commands.length, 0);
    assert.match(h.alerts[0], /重命名/);
    assert.equal(Object.keys(h.files).filter(name => /bridge-requests/.test(name)).length, 0);
});
