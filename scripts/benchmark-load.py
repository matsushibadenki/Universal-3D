#!/usr/bin/env python3
"""Replay real local USD/.u3d/recovery inputs in fresh, headless production-CPU load paths.
Inputs are read-only. Each run writes only to its own temporary directory.
"""
import argparse
import bisect
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
TEST = 'load_benchmark::measure'


def descendant_memory(rows, root):
    """Rows contain PID, PPID, KiB RSS. Never include unrelated processes."""
    owned = {root}
    while True:
        children = {pid for pid, parent, _ in rows if parent in owned}
        newer = owned | children
        if newer == owned:
            break
        owned = newer
    rss = {pid: kib * 1024 for pid, _, kib in rows if pid in owned}
    if root not in rss or rss[root] <= 0:
        return None
    return {'root_rss_bytes': rss[root], 'tree_rss_sum_bytes': sum(rss.values()),
            'process_count': len(rss)}


def sample_memory(pid):
    result = subprocess.run(['ps', '-axo', 'pid=,ppid=,rss='], text=True,
                            capture_output=True, check=True)
    rows = []
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) == 3:
            rows.append(tuple(map(int, fields)))
    return descendant_memory(rows, pid)


def executable():
    build = subprocess.run(['cargo', 'test', '--offline', '--manifest-path',
                            'src-tauri/Cargo.toml', '--no-run', '--message-format=json'],
                           cwd=ROOT, text=True, capture_output=True, check=True)
    for line in build.stdout.splitlines():
        record = json.loads(line)
        if record.get('reason') == 'compiler-artifact' and record.get('executable') and record.get('profile', {}).get('test'):
            return record['executable']
    raise RuntimeError('Test executable not found')


def command(exe):
    return [exe, '--exact', TEST, '--ignored', '--nocapture', '--test-threads=1']


def stream_lines(stream, messages, channel):
    for line in stream:
        messages.put((time.monotonic(), channel, line))
    stream.close()


def annotate_memory(profile, samples, events):
    times = [event['at_us'] for event in events]
    if times != sorted(times):
        raise ValueError('Non-monotonic phase events')
    for sample in samples:
        index = bisect.bisect_right(times, sample['at_us']) - 1
        sample['phase'] = events[index]['phase'] if index >= 0 else 'startup'
    phase_memory = {}
    for phase in sorted({p['phase'] for p in profile['phases']} | {'first_snap'}):
        selected = [sample for sample in samples if sample['phase'] == phase and sample['root_rss_bytes'] > 0]
        phase_memory[phase] = {
            'samples': len(selected),
            'sampled_peak_root_rss_bytes': max((r['root_rss_bytes'] for r in selected), default=None),
            'sampled_peak_tree_rss_sum_bytes': max((r['tree_rss_sum_bytes'] for r in selected), default=None)}
    return phase_memory


def run(exe, mode, source, work, interval, timeout, implementation="current"):
    env = {**os.environ, 'U3D_LOAD_MODE': mode, 'U3D_LOAD_INPUT': str(source), 'U3D_LOAD_WORK': str(work), 'U3D_SURFACE_PREVIOUS': '1' if implementation == 'previous' else '0'}
    start = time.monotonic()
    process = subprocess.Popen(command(exe), cwd=ROOT, env=env, text=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    messages = queue.Queue()
    threads = [threading.Thread(target=stream_lines, args=(stream, messages, name))
               for stream, name in [(process.stdout, 'out'), (process.stderr, 'err')]]
    for thread in threads:
        thread.start()
    events, samples, output, errors = [], [], [], []
    pid, anchor, next_sample = None, None, start
    try:
        while process.poll() is None or any(t.is_alive() for t in threads) or not messages.empty():
            if time.monotonic() - start > timeout:
                raise TimeoutError(f'Load run exceeded {timeout}s')
            while True:
                try:
                    received, channel, line = messages.get_nowait()
                except queue.Empty:
                    break
                output.append(line)
                if channel == 'out' and 'LOAD_STAGE ' in line:
                    event = json.loads(line.split('LOAD_STAGE ', 1)[1])
                    events.append(event)
                    pid = event['pid']
                    if anchor is None:
                        anchor = received - event['at_us'] / 1_000_000
            now = time.monotonic()
            if pid and process.poll() is None and now >= next_sample:
                try:
                    memory = sample_memory(pid)
                    if memory is not None:
                        memory['at_us'] = round((time.monotonic() - anchor) * 1_000_000)
                        samples.append(memory)
                except (OSError, subprocess.SubprocessError, ValueError) as error:
                    if str(error) not in errors:
                        errors.append(str(error))
                next_sample = time.monotonic() + interval
            time.sleep(min(interval, 0.01))
        if process.wait() != 0:
            raise RuntimeError(''.join(output))
    finally:
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)  # this run's private process group only
            except ProcessLookupError:
                pass
            process.wait()
        for thread in threads:
            thread.join()
    records = [json.loads(line[len('LOAD_METRICS '):]) for line in output if line.startswith('LOAD_METRICS ')]
    if len(records) != 1:
        raise RuntimeError('Missing load metrics: ' + ''.join(output))
    record = records[0]
    phase_memory = annotate_memory(record['profile'], samples, events)
    gaps = [b['at_us'] - a['at_us'] for a, b in zip(samples, samples[1:])]
    record['memory'] = {'observed_sample_interval_us': {'median': statistics.median(gaps) if gaps else None, 'max': max(gaps, default=None)},
                        'interval_ms': interval * 1000, 'errors': errors, 'samples': samples,
                        'per_phase': phase_memory,
                        'sampled_peak_root_rss_bytes': max((r['root_rss_bytes'] for r in samples), default=None),
                        'sampled_peak_tree_rss_sum_bytes': max((r['tree_rss_sum_bytes'] for r in samples), default=None)}
    record['stage_events'] = events
    record['supervisor_wall_us'] = round((time.monotonic() - start) * 1_000_000)
    return record


def summarize(records):
    phases = sorted({p['phase'] for r in records for p in r['profile']['phases']})
    durations = {phase: [p['elapsed_us'] for r in records for p in r['profile']['phases'] if p['phase'] == phase] for phase in phases}
    surface = [r['diagnostics']['surface_build'] for r in records if 'surface_build' in r.get('diagnostics', {})]
    return {'surface_build_median_us': {key: statistics.median(row[key] for row in surface) for key in surface[0] if key.endswith('_us')} if surface else {},
            'median_total_us': statistics.median(r['profile']['total_us'] for r in records),
            'median_phase_us': {phase: statistics.median(values) for phase, values in durations.items()},
            'phase_observed_runs': {phase: len(values) for phase, values in durations.items()},
            'max_sampled_tree_rss_sum_bytes': max((r['memory']['sampled_peak_tree_rss_sum_bytes'] for r in records if r['memory']['sampled_peak_tree_rss_sum_bytes'] is not None), default=None)}


def file_digest(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(65536), b''):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=['usd', 'open', 'recovery', 'prepare'], required=True)
    parser.add_argument('--input', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--sample-ms', type=float, default=50)
    parser.add_argument('--timeout', type=float, default=900)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--fixture-dir', type=Path, help='New directory for prepare mode; never overwrite existing files')
    parser.add_argument('--label', default='')
    parser.add_argument('--surface-implementation', choices=['current', 'previous', 'compare'], default='current', help='Test-only frozen baseline comparison; native app always uses current')
    args = parser.parse_args()
    if platform.system() not in ('Darwin', 'Linux'):
        parser.error('RSS sampling supports macOS/Linux')
    if not args.input.is_file() or args.runs < 1 or not 5 <= args.sample_ms <= 1000 or args.timeout <= 0:
        parser.error('Input must exist; runs >= 1, sample-ms 5..1000, timeout > 0')
    source = args.input.resolve()
    exe = executable()
    if args.mode == 'prepare':
        if not args.fixture_dir:
            parser.error('prepare requires --fixture-dir')
        args.fixture_dir.mkdir(parents=True, exist_ok=False)
        process = subprocess.Popen(command(exe), cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True,
                                   env={**os.environ, 'U3D_LOAD_MODE':'prepare','U3D_LOAD_INPUT':str(source),
                                        'U3D_LOAD_WORK':str(args.fixture_dir.resolve())})
        try:
            stdout, stderr = process.communicate(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.communicate()
            raise
        if process.returncode:
            raise RuntimeError(stdout + stderr)
        print(stdout)
        return
    initial_stat = source.stat()
    digest = file_digest(source)
    records = []
    for iteration in range(args.runs):
        variants = (['previous', 'current'] if iteration % 2 == 0 else ['current', 'previous']) if args.surface_implementation == 'compare' else [args.surface_implementation]
        for variant in variants:
            with tempfile.TemporaryDirectory(prefix='u3d-load-benchmark-') as temporary:
                record = run(exe, args.mode, source, Path(temporary), args.sample_ms / 1000, args.timeout, variant)
            record['run'] = iteration + 1
            record['surface_implementation'] = variant
            records.append(record)
            if record['memory']['errors'] or not record['memory']['samples']:
                print('RSS sampling incomplete; inspect memory.errors and null fields in the report', file=sys.stderr)
            print(f'{args.mode} {variant} {iteration + 1}/{args.runs}: {record["profile"]["total_us"]/1000:.1f} ms; surface {record["diagnostics"]["surface_build"]["total_us"]/1000:.1f} ms; sampled tree RSS {record["memory"]["sampled_peak_tree_rss_sum_bytes"]}', flush=True)
    final_stat = source.stat()
    if (initial_stat.st_size, initial_stat.st_mtime_ns) != (final_stat.st_size, final_stat.st_mtime_ns) or file_digest(source) != digest:
        raise RuntimeError('Input changed during measurement; report not written')
    report = {'platform':platform.platform(),'machine':platform.machine(),'profile':'debug',
              'rustc':subprocess.check_output(['rustc','--version'],text=True).strip(),
              'input':{'name':source.name,'sha256':digest,'bytes':initial_stat.st_size,'label':args.label,'sha256_verified_after_runs':True},
              'mode':args.mode,'surface_implementation':args.surface_implementation,'runs':records,
              'summary':({'by_implementation':{variant:summarize([r for r in records if r['surface_implementation']==variant]) for variant in ['previous','current']}} if args.surface_implementation == 'compare' else summarize(records)),
              'scope':'Fresh processes, warm filesystem cache possible; headless CPU replay, not native UI/GPU readiness. RSS sum can double-count shared pages; sampled peaks miss short spikes. Phase attribution uses first stdout event as clock anchor and is approximate. Null means no RSS samples, never zero usage.'}
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report['summary'],indent=2))


if __name__ == '__main__':
    main()
