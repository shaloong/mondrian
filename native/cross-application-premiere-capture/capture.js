/* Local CEP host. Every acquisition remains captured, never self-qualified. */
'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const output = document.getElementById('result');
const button = document.getElementById('capture');
const input = document.getElementById('request');
let busy = false;
let unsettled = false;
function sha(file) {
  const descriptor = fs.openSync(file, 'r');
  try {
    const hash = crypto.createHash('sha256'), chunk = Buffer.alloc(65536);
    let count;
    while ((count = fs.readSync(descriptor, chunk, 0, chunk.length, null)) !== 0) hash.update(chunk.subarray(0, count));
    return hash.digest('hex');
  } finally { fs.closeSync(descriptor); }
}
function keys(value, expected) {
  if (!value || Array.isArray(value) || typeof value !== 'object' || Object.keys(value).sort().join('|') !== expected.slice().sort().join('|')) throw new Error('Missing or unknown request fields');
}
function fixedFile(base, item) {
  keys(item, ['path', 'sha256']);
  if (!/^[a-f0-9]{64}$/.test(item.sha256)) throw new Error('Invalid file hash');
  const file = path.resolve(base, item.path), stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size === 0 || stat.size > 2 ** 31 || sha(file) !== item.sha256) throw new Error('Frozen input mismatch: ' + file);
  return file;
}
function host(method, args) {
  const source = '$._mondrianColorCapture.' + method + '(' + args.map(value => JSON.stringify(String(value)).replace(/\u2028/g, '\\u2028').replace(/\u2029/g, '\\u2029')).join(',') + ')';
  return new Promise((resolve, reject) => {
    // An in-host synchronous export cannot safely be killed independently of a
    // user's Premiere process. Report timeout as unsettled, never native closure.
    const timer = setTimeout(() => { unsettled = true; reject(new Error('Native host timeout; export settlement is unknown. Verify Premiere has finished before restarting this panel.')); }, 120000);
    window.__adobe_cep__.evalScript(source, raw => { clearTimeout(timer); try { resolve(JSON.parse(raw)); } catch (error) { reject(error); } });
  });
}
input.addEventListener('change', () => { button.disabled = busy || unsettled || !input.files.length; });
button.addEventListener('click', async () => {
  if (busy || unsettled) return;
  busy = true;
  button.disabled = true;
  let report, root, reportPath;
  try {
    const requestPath = input.files[0].path, base = path.dirname(requestPath);
    if (fs.statSync(requestPath).size > 1048576) throw new Error('Request exceeds one MiB');
    const request = JSON.parse(fs.readFileSync(requestPath, 'utf8').replace(/^\uFEFF/, ''));
    keys(request, ['schema_version', 'run_id', 'expected_version', 'expected_build', 'project', 'dependencies', 'cases', 'output_directory']);
    if (request.schema_version !== 1 || !/^[a-zA-Z0-9_-]{1,128}$/.test(request.run_id)) throw new Error('Invalid request identity');
    if (!Array.isArray(request.dependencies) || request.dependencies.length > 1024 || !Array.isArray(request.cases) || !request.cases.length || request.cases.length > 64) throw new Error('Unbounded inventory');
    const project = fixedFile(base, request.project), frozen = [[project, request.project.sha256], [requestPath, sha(requestPath)]];
    request.dependencies.forEach(item => frozen.push([fixedFile(base, item), item.sha256]));
    const seen = new Set();
    for (const item of request.cases) {
      keys(item, ['case_id', 'sequence_id', 'frame_index', 'frame_duration_ticks', 'width', 'height', 'working_color_space', 'preset', 'suffix']);
      if (!/^[a-z0-9_-]{1,128}$/.test(item.case_id) || seen.has(item.case_id) || !Number.isSafeInteger(item.frame_index) || item.frame_index < 0 || item.frame_index > 1048574 || !/^[1-9][0-9]{0,11}$/.test(item.frame_duration_ticks) || !['.png', '.exr', '.mov'].includes(item.suffix)) throw new Error('Invalid case frame or identity');
      seen.add(item.case_id); frozen.push([fixedFile(base, item.preset), item.preset.sha256]);
    }
    root = path.resolve(base, request.output_directory);
    fs.mkdirSync(root); reportPath = path.join(root, 'capture.json');
    report = { schema_version: 1, producer: 'adobe_premiere_pro', run_id: request.run_id, request_sha256: sha(requestPath), project_sha256: request.project.sha256, adapter_sha256: sha(path.join(__dirname, 'capture.js')), host_adapter_sha256: sha(path.join(__dirname, 'capture.jsx')), status: 'failed', artifacts: [] };
    for (const item of request.cases) {
      const observed = await host('observe', [item.sequence_id]);
      if (observed.status !== 'observed' || observed.version !== request.expected_version || observed.build !== request.expected_build || path.resolve(observed.project_path).toLowerCase() !== project.toLowerCase()) throw new Error('Native application/project identity differs');
      const settings = observed.settings;
      if (settings.width !== item.width || settings.height !== item.height || settings.frame_duration_ticks !== item.frame_duration_ticks || settings.working_color_space !== item.working_color_space) throw new Error('Native sequence raster/cadence/color differs');
      const start = Number(item.frame_duration_ticks) * item.frame_index, end = start + Number(item.frame_duration_ticks);
      if (!Number.isSafeInteger(end)) throw new Error('Frame ticks exceed exact JS integer authority');
      const target = path.join(root, item.case_id + item.suffix);
      const acquired = await host('capture', [item.sequence_id, target, fixedFile(base, item.preset), start, end]);
      const evidence = { case_id: item.case_id, observed, acquired };
      report.artifacts.push(evidence);
      if (acquired.status !== 'captured' || acquired.restoration_error !== null) throw new Error('Native capture or restoration failed');
      evidence.path = path.basename(target); evidence.sha256 = sha(target); evidence.bytes = fs.statSync(target).size;
      if (!evidence.bytes) throw new Error('Empty native artifact');
    }
    frozen.forEach(([file, hash]) => { if (sha(file) !== hash) throw new Error('Frozen input changed during native acquisition'); });
    report.status = 'captured';
  } catch (error) {
    if (report) report.failure = String(error);
    output.textContent = String(error);
  } finally {
    if (report && reportPath) {
      report.native_host_settled = !unsettled;
      try { fs.writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n', { encoding: 'utf8', flag: 'wx' }); output.textContent = report.status + '\n' + reportPath + (report.failure ? '\n' + report.failure : ''); }
      catch (error) { output.textContent = 'Report publication failed: ' + error + '\n' + JSON.stringify(report); }
    }
    busy = false;
    button.disabled = unsettled;
  }
});
