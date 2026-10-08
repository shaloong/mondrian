"""Attest an actually completed independent official C++ call-through build.

This only reads current sources/toolchain/artifacts. It never builds the helper
or executes the product; commands and transcript must come from the real build.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def sha(path: Path) -> str:
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def artifact(path: Path) -> dict:
    actual = path.resolve(strict=True)
    return {'path':str(actual), 'sha256':sha(actual)}


def main() -> None:
    parser = argparse.ArgumentParser()
    for name in ['worktree','helper-binary','official-include-root','compiler',
                 'build-transcript','dependency-file','output']:
        parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--build-command', required=True)
    args = parser.parse_args()
    root = args.worktree.resolve(strict=True)
    include = args.official_include_root.resolve(strict=True)
    manifest = json.loads((root / 'vendor/vst3-sdk/GENERATION-SOURCES.json').read_text(encoding='utf-8'))
    official_count = 0
    for item in manifest['sources']:
        if not item['role'].startswith('official-'):
            continue
        relative = Path(item['path'])
        if relative.is_absolute() or '..' in relative.parts:
            raise RuntimeError('Invalid official source path')
        if sha(include / 'pluginterfaces' / relative) != item['sha256']:
            raise RuntimeError(f'Official compiler include differs from pinned source: {relative}')
        official_count += 1
    if official_count != 69:
        raise RuntimeError('Full official 69-file source tree required')
    compiler = args.compiler.resolve(strict=True)
    compiler_summary = subprocess.check_output([str(compiler), '--version'], text=True, encoding='utf-8', errors='replace').strip()
    receipt = {'schema':1, 'sdk_commit':manifest['sdk_commit'],
               'pluginterfaces_commit':manifest['pluginterfaces_commit'],
               'cpp_source_sha256':sha(root / 'tests/vst3_reference/com-callthrough.cpp'),
               'official_include_root':str(include), 'verified_official_files':official_count,
               'compiler':compiler_summary, 'compiler_binary':artifact(compiler),
               'build_command':args.build_command, 'build_transcript':artifact(args.build_transcript),
               'dependency_file':artifact(args.dependency_file),
               'helper_binary':artifact(args.helper_binary),
               'scope':'Official C++ virtual calls into canonical Rust COM object. This attests actual ABI call-through only; not optional host semantics or external vendor compatibility.'}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf-8', newline='\n') as output:
        json.dump(receipt, output, indent=2)
        output.write('\n')
    print(json.dumps({'receipt':str(args.output.resolve()), 'sha256':sha(args.output)}))


if __name__ == '__main__':
    main()
