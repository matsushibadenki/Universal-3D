#!/usr/bin/env python3
"""Run synthetic snap-index measurements in fresh test processes (no application data)."""
import argparse
import csv
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--cases', nargs='+', choices=['shared', 'dense', 'masked', 'many'],
                        default=['shared', 'dense', 'masked', 'many'])
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if platform.system() not in ("Darwin", "Linux"):
        parser.error("RSS measurement supports macOS and Linux")
    if args.runs < 1:
        parser.error('--runs must be positive')
    build = subprocess.run(['cargo', 'test', '--offline', '--manifest-path',
                            'src-tauri/Cargo.toml', '--no-run', '--message-format=json'],
                           cwd=ROOT, text=True, capture_output=True, check=True)
    executable = None
    for line in build.stdout.splitlines():
        record = json.loads(line)
        if record.get('reason') == 'compiler-artifact' and record.get('profile', {}).get('test') and record.get('executable'):
            executable = record['executable']
    if executable is None:
        raise RuntimeError('Test executable not found')
    runs = []
    for case in args.cases:
        for iteration in range(args.runs):
            command = [executable, '--exact', 'snapping::tests::surface_preparation_measurement',
                       '--ignored', '--nocapture', '--test-threads=1']
            # A fresh Python supervisor makes RUSAGE_CHILDREN specific to this run.
            supervisor = (
                "import json,resource,subprocess,sys; "
                "p=subprocess.run(sys.argv[1:],text=True,capture_output=True); "
                "print(p.stdout,end=''); print(p.stderr,file=sys.stderr,end=''); "
                "print('PEAK_RSS '+str(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)); "
                "sys.exit(p.returncode)"
            )
            result = subprocess.run([sys.executable, '-c', supervisor] + command, cwd=ROOT,
                                    env={**os.environ, 'U3D_SNAP_MEMORY_CASE': case},
                                    text=True, capture_output=True)
            if result.returncode:
                raise RuntimeError(result.stdout + result.stderr)
            match = re.search(r'SNAP_METRICS (\{[^\n]+\})', result.stdout)
            if not match:
                raise RuntimeError(result.stdout + result.stderr)
            record = json.loads(match.group(1))
            peak_match = re.search(r'PEAK_RSS (\d+)', result.stdout)
            peak = int(peak_match.group(1)) if peak_match else None
            if peak is not None and platform.system() != 'Darwin':
                peak *= 1024
            record.update(run=iteration + 1, peak_rss_bytes=peak)
            runs.append(record)
            print(f'{case} {iteration + 1}/{args.runs}: prepare={record["prepare_us"] / 1000:.1f} ms, peakRSS={peak}', flush=True)
    summary = {}
    for case in args.cases:
        rows = [r for r in runs if r['case'] == case]
        summary[case] = {key: statistics.median(r[key] for r in rows)
                         for key in ['prepare_us', 'install_us', 'first_edge_us', 'first_face_us']}
        peaks = [r['peak_rss_bytes'] for r in rows if r['peak_rss_bytes'] is not None]
        summary[case]['max_peak_rss_bytes'] = max(peaks) if peaks else None
        summary[case]['retained_buffer_bytes'] = rows[0]['diagnostics']['buffer_bytes']
    report = {'platform': platform.platform(), 'machine': platform.machine(),
              'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
              'summary': summary, 'runs': runs}
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        with args.output.with_suffix('.csv').open('w', newline='') as stream:
            fields = ['case', 'run', 'prepare_us', 'install_us', 'first_edge_us', 'first_face_us', 'peak_rss_bytes', 'retained_buffer_bytes',
                      'source_buffer_bytes', 'scene_entry_payload_bytes', 'edges', 'triangles']
            writer = csv.DictWriter(stream, fieldnames=fields, extrasaction='ignore')
            writer.writeheader()
            writer.writerows({**row,
                              'retained_buffer_bytes': row['diagnostics']['buffer_bytes'],
                              'scene_entry_payload_bytes': row['diagnostics']['memory']['scene_entry_payload_bytes'],
                              'edges': row['prepared_memory']['edges'],
                              'triangles': row['prepared_memory']['triangles']} for row in runs)
    print(json.dumps(summary, indent=2))

if __name__ == '__main__':
    main()
