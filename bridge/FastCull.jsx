#target bridge
// FastCull Bridge entry, ExtendScript ES3. No metadata is written on launch.
(function () {
    if (BridgeTalk.appName != "bridge") { return; }
    var scriptFolder = new File($.fileName).parent;
    var api = {};
    api.isMac = /mac/i.test($.os || "");
    api.version = "2.7.7";
    // Capture roots only; recursive enumeration belongs to the detached Rust
    // process, never to Bridge's menu display callback or metadata API.
    api.collect = function (selected) {
        var paths = [], seen = {}, i, entry, path, key;
        for (i = 0; i < selected.length; ++i) {
            entry = selected[i].container ? new Folder(selected[i].spec) : new File(selected[i].spec);
            path = entry.fsName;
            if (!entry.exists || (!selected[i].container && !/\.(arw|cr2|nef|jpg|jpeg|jpe)$/i.test(path))) { continue; }
            if (/[\r\n]/.test(path)) { throw new Error("照片或文件夹名称含换行，请先重命名再打开。"); }
            key = "$" + (api.isMac ? path : path.toLowerCase().replace(/\//g, "\\"));
            if (!seen[key]) { paths.push(path); seen[key] = true; }
        }
        return paths;
    };
    api.paths = function () {
        return api.collect(app.document ? app.document.selections : []);
    };
    // Only the exe and a generated request filename reach cmd.exe. Photo names
    // (including %, &, parentheses and Unicode) remain data inside the UTF-8 file.
    api.quote = function (path) {
        if (api.isMac) {
            if (/[\r\n]/.test(path) || path.indexOf(String.fromCharCode(0)) >= 0) {
                throw new Error("程序或请求路径含换行或 NUL。");
            }
            return "'" + path.replace(/'/g, "'\"'\"'") + "'";
        }
        // ExtendScript's legacy RegExp rejects \0 inside a character class at
        // execution time (error 8), although Node accepts it. Check characters
        // directly so the same path protection works in the real Bridge engine.
        var forbidden = ['%', '!', '"', '\r', '\n', String.fromCharCode(0)];
        for (var i = 0; i < forbidden.length; ++i) {
            if (path.indexOf(forbidden[i]) >= 0) {
                throw new Error("程序或请求目录含不支持的命令字符，请重新安装到不含 % 或 ! 的路径。");
            }
        }
        return '"' + path + '"';
    };
    api.runCommand = function (command) { return app.system(command); };
    api.open = function () {
        var request = null;
        var stage = "读取所选照片";
        try {
            var paths = api.paths();
            if (!paths.length) { throw new Error("请先在 Bridge 中选中 RAW/JPEG 照片或包含照片的文件夹。"); }
            stage = "读取程序配置";
            var config = new File(scriptFolder.fsName + "/FastCull-path.txt");
            config.encoding = "UTF-8";
            if (!config.open("r")) { throw new Error("缺少 FastCull 配置，请重新运行安装脚本。"); }
            var executablePath;
            try { executablePath = config.read().replace(/^\uFEFF/, "").replace(/[\r\n]+$/, ""); }
            finally { config.close(); }
            var executable = new File(executablePath);
            if (!executable.exists || (!api.isMac && !/\.exe$/i.test(executable.fsName))) {
                throw new Error("找不到 FastCull 程序，请重新运行安装脚本。");
            }
            stage = "准备启动请求";
            var directory = new Folder(Folder.userData.fsName + "/FastCull/bridge-requests");
            var root = new Folder(Folder.userData.fsName + "/FastCull");
            if ((!root.exists && !root.create()) || (!directory.exists && !directory.create())) {
                throw new Error("无法创建 Bridge 选片请求目录。");
            }
            var name = (new Date()).getTime() + "-" + Math.floor(Math.random() * 1000000000);
            // Always launch the configured executable. ExtendScript File.execute()
            // can show Open With even when Windows ShellExecute resolves the type.
            // Ignore legacy FastCull-shell.txt markers left by older installers.
            request = new File(directory.fsName + "/" + name + ".txt");
            if (request.exists) { throw new Error("请求编号重复，请重试。"); }
            stage = "检查启动路径";
            // app.system's direct executable handling differs from a cmd shell.
            // Explicit cmd disables AutoRun/delayed expansion and keeps the nested
            // quotes required for paths with spaces and the detached native helper.
            var command = api.quote(executable.fsName) + " --bridge-open " + api.quote(request.fsName);
            if (!api.isMac) { command = 'cmd.exe /d /v:off /s /c "' + command + '"'; }
            stage = "保存照片清单";
            request.encoding = "UTF-8";
            request.lineFeed = "Unix";
            if (!request.open("w")) { throw new Error("无法写入选片请求。"); }
            try {
                if (!request.write("FASTCULL_BRIDGE_V1\n" + paths.join("\n") + "\n")) {
                    throw new Error("选片请求写入失败。");
                }
            } finally { request.close(); }
            // The native helper validates and snapshots the list, starts a detached
            // GUI and returns immediately. No wait for GUI exit, no RAW/XMP parsing.
            stage = "启动 Rust 选片器";
            var result = api.runCommand(command);
            if (typeof result == "number" && result != 0) {
                throw new Error("FastCull 启动失败，退出码：" + result);
            }
        } catch (error) {
            // Retain an unsuccessful request for diagnosis. Native errors are also
            // shown by FastCull; no silent loss of the user's selection.
            var detail = stage + "：" + error.message;
            if (error.number !== undefined) { detail += "\n错误号：" + error.number; }
            if (error.line !== undefined) { detail += "，行号：" + error.line; }
            Window.alert("FastCull\n\n" + detail);
        }
    };
    function register(id, location) {
        var item = MenuElement.find(id);
        if (!item) {
            item = new MenuElement("command", "FastCull · 打开选片器" + (api.isMac ? "" : "（Ctrl+Alt+K）"), location, id);
        }
        item.onSelect = api.open;
        item.onDisplay = function () {
            try { item.enabled = api.paths().length > 0; }
            catch (error) { item.enabled = false; }
        };
    }
    register("com.zrtc.fastcull.open", "at the end of Tools");
    register("com.zrtc.fastcull.context", "at the end of Thumbnail");
    // A stable namespace allows diagnostics without touching Bridge selections.
    if (typeof FastCullBridge != "undefined" && FastCullBridge.hotkeyTask) {
        try { app.cancelTask(FastCullBridge.hotkeyTask); } catch (ignored) {}
    }
    FastCullBridge = api;
    if (api.isMac) { return; }
    // Bridge can suspend scheduled tasks during long operations/dialogs. The
    // native listener then expires its 30-second host lease. Recover on resume,
    // without launching a new process while a healthy listener is present.
    api.lastHelperStart = null;
    api.ensureHotkey = function () {
        if (!api.hotkeyHelper || !api.hotkeyDirectory) { return; }
        var now = (new Date()).getTime();
        var heartbeat = new File(api.hotkeyDirectory.fsName + "/helper-alive");
        var stamp = 0;
        if (heartbeat.exists && heartbeat.open("r")) {
            try { stamp = Number(heartbeat.read()); } finally { heartbeat.close(); }
        }
        if (stamp > 0 && now >= stamp && now - stamp < 12000) { return; }
        if (api.lastHelperStart !== null && now >= api.lastHelperStart && now - api.lastHelperStart < 10000) { return; }
        api.lastHelperStart = now;
        if (api.hotkeyHelper.execute()) { api.hotkeyError = null; }
        else { api.hotkeyError = "无法启动快捷键助手，将自动重试。"; }
    };
    api.pollHotkey = function () {
        var directory = new Folder(Folder.userData.fsName + "/FastCull/hotkeys-bridge");
        var request = new File(directory.fsName + "/request");
        if (request.exists && request.open("r")) {
            var stamp = Number(request.read()); request.close(); request.remove();
            var age = (new Date()).getTime() - stamp;
            if (age >= 0 && age < 1500) { api.open(); }
        }
        api.hotkeyTicks++;
        if (api.hotkeyTicks % 20 == 0) {
            var alive = new File(directory.fsName + "/alive");
            if (alive.open("w")) { alive.write("active"); alive.close(); }
            try { api.ensureHotkey(); }
            catch (error) { api.hotkeyError = error.message; }
        }
    };
    api.hotkeyTicks = 0;
    try {
        var keyConfig = new File(scriptFolder.fsName + "/FastCull-path.txt");
        keyConfig.encoding = "UTF-8";
        var helper = null;
        if (keyConfig.open("r")) {
            var keyExecutable = keyConfig.read().replace(/^\uFEFF/, "").replace(/[\r\n]+$/, "");
            keyConfig.close();
            helper = new File(new File(keyExecutable).parent.fsName + "/fastcull-keys.exe");
        }
        if (helper && helper.exists && typeof app.scheduleTask == "function") {
            var root = new Folder(Folder.userData.fsName + "/FastCull");
            if (!root.exists) { root.create(); }
            var directory = new Folder(root.fsName + "/hotkeys-bridge");
            if (!directory.exists) { directory.create(); }
            var stale = new File(directory.fsName + "/request");
            if (stale.exists) { stale.remove(); }
            var alive = new File(directory.fsName + "/alive");
            if (alive.open("w")) { alive.write("active"); alive.close(); }
            api.hotkeyHelper = helper;
            api.hotkeyDirectory = directory;
            // Install recovery even if the initial launch temporarily fails.
            api.hotkeyTask = app.scheduleTask("FastCullBridge.pollHotkey()", 250, true);
            api.ensureHotkey();
        }
    } catch (hotkeyError) { api.hotkeyError = hotkeyError.message; }
}());
