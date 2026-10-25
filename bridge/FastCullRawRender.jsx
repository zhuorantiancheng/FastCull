/* FastCull private Adobe Camera Raw renderer. Loaded by the embedded COM worker.
   The source passed to render MUST be FastCull's cached copy, never the original. */
var FastCullRaw = (function () {
    var cancelPath = null;
    function checkCancel() {
        if (cancelPath && new File(cancelPath).exists) throw Error('Adobe RAW preview cancelled.');
    }
    function clean(value) { return String(value).replace(/[\r\n\t]/g, ' '); }
    function readText(path) {
        var file = new File(path);
        file.encoding = 'UTF-8';
        if (!file.open('r')) throw Error('Cannot read XMP packet: ' + file.error);
        try { return file.read(); } finally { file.close(); }
    }
    function assertNotOpen(path) {
        var target = new File(path).fsName.toLowerCase();
        for (var i = 0; i < app.documents.length; i++) {
            var existing = null;
            try { existing = app.documents[i].fullName.fsName.toLowerCase(); } catch (_) {}
            if (existing === target)
                throw Error('This RAW is already open in Photoshop. Close that document and retry.');
        }
    }
    function effectiveWhiteBalance(document) {
        try {
            if (!ExternalObject.AdobeXMPScript)
                ExternalObject.AdobeXMPScript = new ExternalObject('lib:AdobeXMPScript');
            var xmp = new XMPMeta(document.xmpMetadata.rawData);
            var ns = 'http://ns.adobe.com/camera-raw-settings/1.0/';
            var temperature = xmp.getProperty(ns, 'Temperature');
            var tint = xmp.getProperty(ns, 'Tint');
            return [temperature ? clean(temperature.value) : '', tint ? clean(tint.value) : ''];
        } catch (_) { return ['', '']; }
    }
    function writeMetadata(path, packetPath, safe) {
        if (!ExternalObject.AdobeXMPScript)
            ExternalObject.AdobeXMPScript = new ExternalObject('lib:AdobeXMPScript');
        var packet = new XMPMeta(readText(packetPath));
        var file = new XMPFile(path, XMPConst.FILE_UNKNOWN, XMPConst.OPEN_FOR_UPDATE);
        try {
            if (!file.canPutXMP(packet)) throw Error('Adobe XMP cannot update this file.');
            file.putXMP(packet);
            // Private preview copies can update their padded packet in place.
            // Actual user saves always use Adobe's crash-safe update mode.
            file.closeFile(safe ? XMPConst.CLOSE_UPDATE_SAFELY : 0);
            file = null;
        } finally {
            if (file) { try { file.closeFile(); } catch (_) {} }
        }
    }
    function render(request) {
        var document = null, previous = null, dialogs = app.displayDialogs;
        try {
            if (app.documents.length) previous = app.activeDocument;
            var previousIds = {};
            for (var i = 0; i < app.documents.length; i++) previousIds[app.documents[i].id] = true;
            app.displayDialogs = DialogModes.NO;
            checkCancel();
            assertNotOpen(request.source);
            var phase = new Date().getTime();
            if (request.embed) writeMetadata(request.source, request.packet, false);
            var metadataMs = new Date().getTime() - phase;
            checkCancel();
            // No CameraRAWOpenOptions: those override the existing ACR recipe.
            phase = new Date().getTime();
            document = app.open(new File(request.source));
            var openMs = new Date().getTime() - phase;
            if (previousIds[document.id]) {
                // app.open can return an existing document. Never edit/close a
                // document that belonged to the user before this operation.
                document = null;
                throw Error('Photoshop reused an existing document; preview was cancelled.');
            }
            checkCancel();
            phase = new Date().getTime();
            var whiteBalance = effectiveWhiteBalance(document);
            if (document.mode != DocumentMode.RGB) document.changeMode(ChangeMode.RGB);
            if (document.colorProfileName != 'sRGB IEC61966-2.1')
                document.convertProfile('sRGB IEC61966-2.1', Intent.RELATIVECOLORIMETRIC, true, false);
            var profileMs = new Date().getTime() - phase;
            checkCancel();
            phase = new Date().getTime();
            var width = document.width.as('px'), height = document.height.as('px');
            if (request.maxEdge > 0 && Math.max(width, height) > request.maxEdge) {
                var factor = request.maxEdge / Math.max(width, height);
                document.resizeImage(UnitValue(Math.max(1, Math.round(width * factor)), 'px'),
                    UnitValue(Math.max(1, Math.round(height * factor)), 'px'), null, ResampleMethod.BICUBIC);
            }
            if (document.bitsPerChannel != BitsPerChannelType.EIGHT)
                document.bitsPerChannel = BitsPerChannelType.EIGHT;
            var resizeMs = new Date().getTime() - phase;
            checkCancel();
            phase = new Date().getTime();
            var options;
            if (/\.jpe?g$/i.test(request.output)) {
                options = new JPEGSaveOptions();
                options.quality = 11;
                options.embedColorProfile = true;
                options.formatOptions = FormatOptions.STANDARDBASELINE;
            } else {
                options = new PNGSaveOptions();
                options.compression = 1;
                options.interlaced = false;
            }
            document.saveAs(new File(request.output), options, true, Extension.LOWERCASE);
            var writeMs = new Date().getTime() - phase;
            return 'OK\t' + Math.round(document.width.as('px')) + '\t' +
                Math.round(document.height.as('px')) + '\t' + clean(document.colorProfileName) + '\t' +
                [metadataMs, openMs, profileMs, resizeMs, writeMs].join('\t') + '\t' + whiteBalance.join('\t');
        } catch (error) {
            return 'ERR\t' + clean(error.message) + ' (Adobe line ' + clean(error.line) + ')';
        } finally {
            if (document) { try { document.close(SaveOptions.DONOTSAVECHANGES); } catch (_) {} }
            if (previous) { try { app.activeDocument = previous; } catch (_) {} }
            app.displayDialogs = dialogs;
        }
    }
    return {
        setCancel: function (path) { cancelPath = path; },
        render: render,
        embed: function (source, packet) {
            try { checkCancel(); assertNotOpen(source); writeMetadata(source, packet, true); return 'OK'; }
            catch (error) { return 'ERR\t' + clean(error.message); }
        }
    };
})();
