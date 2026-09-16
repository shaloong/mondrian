/* Native Premiere 24.x public ExtendScript adapter. No QE or PProHeadless. */
$._mondrianColorCapture = (function () {
    function encode(value) {
        if (value === null || value === undefined) return "null";
        if (typeof value === "boolean" || typeof value === "number") return String(value);
        if (typeof value === "string") return '"' + value.replace(/[\\"\x00-\x1f\u2028\u2029]/g, function (c) {
            var hex = c.charCodeAt(0).toString(16); return "\\u" + ("0000" + hex).slice(-4);
        }) + '"';
        var fields = [], key;
        if (value instanceof Array) { for (key = 0; key < value.length; ++key) fields.push(encode(value[key])); return "[" + fields.join(",") + "]"; }
        for (key in value) if (value.hasOwnProperty(key)) fields.push(encode(key) + ":" + encode(value[key]));
        return "{" + fields.join(",") + "}";
    }
    function describe(seq) {
        var settings = seq.getSettings();
        if (!settings) throw new Error("native sequence settings unavailable");
        return { sequence_id: String(seq.sequenceID), name: seq.name,
            width: settings.videoFrameWidth, height: settings.videoFrameHeight,
            frame_duration_ticks: String(settings.videoFrameRate.ticks),
            pixel_aspect: String(settings.videoPixelAspectRatio),
            working_color_space: settings.workingColorSpace === undefined ? null : String(settings.workingColorSpace),
            maximum_bit_depth: settings.maximumBitDepth === undefined ? null : settings.maximumBitDepth,
            maximum_render_quality: settings.maximumRenderQuality === undefined ? null : settings.maximumRenderQuality,
            in_ticks: String(seq.getInPointAsTime().ticks), out_ticks: String(seq.getOutPointAsTime().ticks) };
    }
    function selected(id) {
        if (!app.project || !app.project.path) throw new Error("open the frozen native project first");
        var matches = [];
        for (var i = 0; i < app.project.sequences.numSequences; ++i) {
            var seq = app.project.sequences[i];
            if (String(seq.sequenceID) === id) matches.push(seq);
        }
        if (matches.length !== 1) throw new Error("sequence identity is missing or ambiguous");
        return matches[0];
    }
    function observe(id) {
        try { return encode({ status: "observed", version: app.version, build: String(app.build),
            project_path: new File(app.project.path).fsName, settings: describe(selected(id)) }); }
        catch (error) { return encode({ status: "failed", error: String(error) }); }
    }
    function capture(id, output, preset, startTicks, endTicks) {
        var result = { status: "failed", error: null, restoration_error: null }, seq, oldIn, oldOut;
        try {
            seq = selected(id); oldIn = seq.getInPointAsTime(); oldOut = seq.getOutPointAsTime();
            var start = new Time(), end = new Time(); start.ticks = startTicks; end.ticks = endTicks;
            seq.setInPoint(start.seconds); seq.setOutPoint(end.seconds);
            result.settings = describe(seq);
            if (result.settings.in_ticks !== startTicks || result.settings.out_ticks !== endTicks) throw new Error("native range readback differs from exact frame ticks");
            result.native_result = seq.exportAsMediaDirect(output, preset, app.encoder.ENCODE_IN_TO_OUT);
            if (result.native_result !== true && result.native_result !== 0) throw new Error("native exporter reported failure");
            if (!(new File(output)).exists) throw new Error("native exporter did not create the exact requested file");
            result.status = "captured";
        } catch (error) { result.error = String(error); }
        finally {
            if (seq && oldIn && oldOut) try {
                seq.setInPoint(oldIn.seconds); seq.setOutPoint(oldOut.seconds);
                if (String(seq.getInPointAsTime().ticks) !== String(oldIn.ticks) || String(seq.getOutPointAsTime().ticks) !== String(oldOut.ticks)) throw new Error("native range restoration mismatch");
            } catch (cleanup) { result.restoration_error = String(cleanup); result.status = "failed"; }
        }
        return encode(result);
    }
    return { observe: observe, capture: capture };
}());
